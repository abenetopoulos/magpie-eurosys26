use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::mem::MaybeUninit;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Once};
use std::thread::{self, JoinHandle};
#[cfg(any(
    feature = "timing-core-sched",
    feature = "timing-sched-submission",
    feature = "timing-sched-completion"
))]
use std::time::{Duration, Instant};

use crossbeam_channel;
use dashmap::{DashMap, DashSet};
use execution_definitions::nando_handle::SharedHandleState;
use execution_definitions::{
    activation::{ActivationFunction, NandoActivation, ResolvedNandoArgument},
    nando_handle::NandoHandle,
};
#[cfg(feature = "observability")]
use lazy_static::lazy_static;
use logging::LogManager;
use nando_support::{
    activation_id::ActivationId,
    activation_intent::{NandoActivationIntent, NandoResult},
    ecb_id::EcbId,
    epic_control,
    iptr::IPtr,
    nando_metadata::{NandoKind, NandoMetadata},
};
use object_lib::ObjectId;
#[cfg(not(feature = "offline"))]
use object_lib::ObjectVersion;
use object_tracker::ObjectTracker;
#[cfg(not(feature = "offline"))]
use ownership_tracker::HostId;
use ownership_tracker::{OwnershipTracker, Schedulable};
use parking_lot::{RwLock, RwLockReadGuard};
#[cfg(feature = "observability")]
use prometheus::{register_histogram_vec, HistogramVec};
use rand::{rngs::SmallRng, Rng, SeedableRng};
use rtrb::{Consumer, PopError, Producer, PushError, RingBuffer};
#[cfg(feature = "telemetry")]
use telemetry;

use crate::built_ins;
use crate::config::{Config, SchedulerConfig};
use crate::nando_executor::{ExecutorWorkerId, NandoExecutor};
use crate::plans::plan_manager::{PhysicalPlanManager, SubmissionLocation};
use crate::transaction_manager::TransactionManager;

#[cfg(feature = "observability")]
lazy_static! {
    static ref OPS_NS_NOTIFY_HISTOGRAM: HistogramVec = register_histogram_vec!(
        "nando_scheduler_forward_notifications_latency_histogram",
        "Scheduler-observed latency of forwarding task completion notifications in microseconds",
        &["num_tasks", "completed_task_intent"],
        vec![10.0, 100.0, 1000.0, 10000.0],
    )
    .unwrap();
}

macro_rules! park_spawned_task {
    ($st:expr, $epic_control_registry:ident) => {{
        // We "park" this transaction locally, and wait for dependencies to
        // be met.
        let spawned_task_ecb: epic_control::ECB = (&$st).into();
        $epic_control_registry.insert(
            spawned_task_ecb.id,
            ControlRegistryEntry::new(spawned_task_ecb, $st.intent.clone(), None),
        );
    }};
}

macro_rules! schedule {
    ($ac:ident, $ch:ident, $et:ident) => {
        // NOTE this is actually safe
        let channel = unsafe { $ch.get_unchecked_mut($et) };

        #[cfg(feature = "timing")]
        {
            let timestamp = Instant::now();
            $ac.set_timing_entity("scheduled_at", timestamp);
            $ac.set_timing_entity("sched_to_exec_queue_start", timestamp);
        }

        match channel.push($ac) {
            Ok(_) => (),
            Err(PushError::Full(_)) => todo!(
                "queue {} was full, need to implement some backpressure mechanism",
                $et
            ),
        };
    };
}

macro_rules! schedule_or_park {
    (
        $txn_dependencies:ident,
        $pending_txns:ident,
        $pending_txn_dependencies:ident,
        $aggregate_executor_stats:ident,
        $activation:ident,
        $channels:ident,
        $executor_queue_capacity:expr,
        $et:ident
    ) => {{
        let activation_id = $activation.activation_id;
        if let Some(txn_dependencies) = $txn_dependencies {
            if $activation.is_part_of_epic() {
                $pending_txn_dependencies
                    .entry(activation_id)
                    .and_modify(|e| {
                        let prev = Box::new(e.clone());
                        *e = TxnDependency::PendingTxn {
                            dependency_count: txn_dependencies.len().try_into().unwrap(),
                            target_executor_thread: $et,
                            downstream_pending_txn: Some(prev),
                        };
                    })
                    .or_insert(TxnDependency::PendingTxn {
                        dependency_count: txn_dependencies.len().try_into().unwrap(),
                        target_executor_thread: $et,
                        downstream_pending_txn: None,
                    });
                $pending_txns.insert(activation_id, $activation);
            } else {
                // this nando cannot be immediately scheduled, we need to wait for the
                // dependency set of transactions to complete
                let pending_activation_entry = TxnDependency::PendingTxn {
                    dependency_count: txn_dependencies.len().try_into().unwrap(),
                    target_executor_thread: $et,
                    downstream_pending_txn: None,
                };
                $pending_txns.insert(activation_id, $activation);
                $pending_txn_dependencies.insert(activation_id, pending_activation_entry);
            }

            for txn_dependency in txn_dependencies {
                $pending_txn_dependencies.insert(
                    txn_dependency,
                    TxnDependency::PendingTxnRef { txn: activation_id },
                );
            }
        } else {
            {
                let stats = $aggregate_executor_stats.executor_stats.get(&$et).unwrap();
                let current_depth = stats.current_queue_depth.fetch_add(1, Ordering::Relaxed);

                $aggregate_executor_stats
                    .available_executors
                    .remove_if(&$et, |_e| {
                        (current_depth + 1) as f64 / $executor_queue_capacity
                            >= Self::EXECUTOR_DEPTH_THRESHOLD
                    });
            }

            schedule!($activation, $channels, $et);
        }
    }};
}

macro_rules! generate_epic_activation {
    ($activation_intent:expr, $activation_metadata:ident, $scheduler: expr, $activation_id:expr) => {{
        let activation_closure = $scheduler
            .get_nando_function(&$activation_intent.name)
            .expect(&format!(
                "cannot find closure for pending key {}",
                $activation_intent.name
            ));

        let activation = NandoActivation::from_intent(
            $activation_intent.clone(),
            $activation_id,
            activation_closure,
            $activation_metadata,
        );

        activation
    }};
}

// FIXME this is what's breaking the deferred ecb creation jazz now.
macro_rules! generate_epic_activation_from_rest {
    ($ecb_at_rest:expr, $activation_intent:expr, $scheduler: expr) => {{
        let activation_id = $ecb_at_rest.get_associated_activation_id();
        let activation_metadata = $scheduler
            .get_nando_metadata(&$activation_intent.name)
            .expect(&format!(
                "cannot find kind for pending key {}",
                $activation_intent.name
            ));
        let mut activation = generate_epic_activation!(
            $activation_intent,
            activation_metadata,
            $scheduler,
            activation_id
        );
        let task_control_info = $ecb_at_rest.to_task_control_info($activation_intent.clone());
        activation.set_task_control_info(task_control_info);

        activation
    }};
}

macro_rules! entry_from_dep_ref {
    ($dep_ref:ident, $epic_control_registry:expr) => {{
        match $dep_ref {
            epic_control::DependencyRef::EcbRef(dependent_ecb_id) => {
                let dependendent_entry_entry = $epic_control_registry
                    .get(dependent_ecb_id)
                    .expect("failed to get dep entry");
                let entry = dependendent_entry_entry.value();
                Arc::clone(&entry.control_info)
            }
            epic_control::DependencyRef::Ptr(parkable_control_block) => {
                Arc::clone(&parkable_control_block)
            }
        }
    }};
}

macro_rules! process_object_argument {
    ($activation:ident,
     $mutable_argument_indices:ident,
     $oa:ident,
     $idx:ident,
     $dependency_stats:ident,
     $stats_map:ident,
     $aggregate_executor_stats:ident,
     $existing_cores:ident
    ) => {{
        let dependency = $oa.get_object_id();
        let mut first_access = false;
        if !$stats_map.contains_key(&dependency) {
            first_access = true;
            let mut object_stats = ObjectStats::default();
            let min_core_stats = $aggregate_executor_stats
                .executor_stats
                .iter()
                .map(|e| {
                    (*e.key(), {
                        let executor_stats = e.value();
                        100 * executor_stats.owned_objects.len()
                            + executor_stats.current_queue_depth.load(Ordering::Relaxed)
                    })
                })
                .min_by_key(|(_, a)| *a)
                .unwrap();
            object_stats.current_owning_thread = min_core_stats.0.try_into().unwrap();
            if let Some(ref mut e) = $aggregate_executor_stats
                .executor_stats
                .get_mut(&min_core_stats.0)
            {
                e.owned_objects.insert(dependency);
            }

            $stats_map.insert(dependency, Rc::new(RefCell::new(object_stats)));
        }

        let entry = $stats_map.get(&dependency).unwrap().clone();
        {
            let mut entry = entry.borrow_mut();

            #[cfg(debug_assertions)]
            println!("will consider dependency {}", dependency);

            #[cfg(not(feature = "no-persist"))]
            if !$activation.meta.kind.is_write_only() {
                // FIXME we also probably need to check if this activation is part of an epic, and
                // if not default all objects (mut and not) to Rw versions for multi-object
                // individual nanotransactions.
                if !$mutable_argument_indices.contains(&$idx) {
                    #[cfg(debug_assertions)]
                    println!("not mutable");
                    if !entry.mv_disabled {
                        #[cfg(debug_assertions)]
                        println!("mv enabled");
                        // Set this so that the executor thread will resolve a Ro version
                        $oa.set_ro_unresolved();
                        continue;
                    }
                }
            }

            #[cfg(debug_assertions)]
            println!("either mutable or mv disabled");

            // Set this so that the executor thread will resolve to the owning version.
            $oa.set_rw_unresolved(first_access || !entry.scheduled_rw_transaction);
            entry.scheduled_rw_transaction = true;

            $existing_cores.push((
                entry.current_owning_thread,
                if entry.times_scheduled > 0 {
                    1.0 / entry.times_scheduled as f32
                } else {
                    0.0
                },
            ));
        }
        $dependency_stats.push((dependency, entry));
    }};
}

type MoveWeight = f32;
#[allow(dead_code)]
struct ObjectStats {
    current_owning_thread: ExecutorWorkerId,
    // TODO set this to something that inversely correlates with time since last move
    move_weight: MoveWeight,
    times_scheduled: u64,
    latest_scheduled_txn: ActivationId,
    latest_committed_txn: ActivationId,
    mv_disabled: bool,
    scheduled_rw_transaction: bool,
}

impl Default for ObjectStats {
    fn default() -> Self {
        Self {
            current_owning_thread: 0,
            move_weight: 1.0,
            times_scheduled: 0,
            latest_committed_txn: ActivationId::default(),
            latest_scheduled_txn: ActivationId::default(),
            mv_disabled: false,
            scheduled_rw_transaction: false,
        }
    }
}

#[derive(Clone, Debug)]
pub enum TxnDependency {
    // FIXME this should probably own a set of txn references, since depending on the associated
    // txns' data dependencies, there might be more than one downstream operation waiting for the
    // given transaction to finish.
    PendingTxnRef {
        txn: ActivationId,
    },
    PendingTxn {
        dependency_count: u8,
        #[allow(dead_code)]
        target_executor_thread: usize,

        downstream_pending_txn: Option<Box<TxnDependency>>,
    },
}

#[derive(Debug)]
pub struct ExecutorStats {
    pub current_queue_depth: AtomicUsize,
    pub historical: AtomicUsize,
    pub owned_objects: HashSet<ObjectId>,
}

#[derive(Debug)]
pub struct AggregateExecutorStats {
    pub available_executors: DashSet<usize>,
    pub executor_stats: DashMap<usize, ExecutorStats>,
}

impl AggregateExecutorStats {
    pub fn new_with_capacity(num_executors: usize) -> Self {
        Self {
            available_executors: DashSet::with_capacity(num_executors),
            executor_stats: DashMap::with_capacity(num_executors),
        }
    }
}

pub enum SchedulerInputKind {
    Activation(NandoActivation),
    // FIXME should be intent so that we can update object stats
    ActivationCompletion(ActivationId, Option<(ActivationId, usize, NandoResult)>),
    UpdateMvEnabled(ObjectId, bool),
    OutputTelemetryStats,
}

// TODO @cache this should also probably contain an entry with a list of activations to be
// forwarded (like in the case of a write causing a set of cache invalidations to be spawned)
pub enum ScheduleResult {
    Nando(NandoHandle),
    Epic(NandoHandle),
}

pub struct ControlRegistryEntry {
    control_info: Arc<RwLock<epic_control::ParkableControlBlock>>,
    #[allow(dead_code)]
    handle_state: Option<SharedHandleState>,
    pending_activations: Vec<NandoActivation>,
}

impl ControlRegistryEntry {
    pub fn get_control_info_read(&self) -> RwLockReadGuard<'_, epic_control::ParkableControlBlock> {
        self.control_info.read()
    }
}

unsafe impl Send for ControlRegistryEntry {}
unsafe impl Sync for ControlRegistryEntry {}

#[derive(Debug)]
pub struct TaskCompletionNotification {
    pub completed_task_id: EcbId,
    pub tasks_to_notify: Vec<(
        epic_control::DownstreamTaskDependency,
        epic_control::TaskStatus,
        Option<NandoResult>,
    )>,
    pub subgraph_allocations: HashSet<ObjectId>,
}

impl TaskCompletionNotification {
    pub fn new(
        completed_task_id: EcbId,
        tasks_to_notify: Vec<(
            epic_control::DownstreamTaskDependency,
            epic_control::TaskStatus,
            Option<NandoResult>,
        )>,
    ) -> Self {
        Self {
            completed_task_id,
            tasks_to_notify,
            subgraph_allocations: HashSet::default(),
        }
    }
}

pub struct SubtaskCompletionResult {
    pub schedulable_tasks: Vec<ControlRegistryEntry>,
    pub spawned_tasks_to_fwd: Vec<epic_control::SpawnedTask>,
    pub external_tasks_to_notify: Vec<TaskCompletionNotification>,
}

pub enum TaskCompletionPropagationResult {
    TaskEntry(ControlRegistryEntry),
    Notifications(
        Vec<epic_control::DownstreamTaskDependency>,
        epic_control::TaskStatus,
        Option<Vec<epic_control::SpawnedTask>>,
    ),
    // HACK only used for update_cache task result propagation
    MultiNotifications(
        Vec<(epic_control::DownstreamTaskDependency, NandoResult)>,
        epic_control::TaskStatus,
        Option<Vec<epic_control::SpawnedTask>>,
    ),
}

impl ControlRegistryEntry {
    fn new(
        control_block: epic_control::ECB,
        intent: NandoActivationIntent,
        handle_state: Option<SharedHandleState>,
    ) -> Self {
        let num_notifying_tasks = control_block.notifying_tasks.len();
        Self {
            control_info: Arc::new(RwLock::new(epic_control::ParkableControlBlock {
                control_block,
                intent,
                completed_tasks: HashMap::with_capacity(num_notifying_tasks),
                status: epic_control::TaskStatus::InProgress,
                cache_invalidation_tasks: None,
                allocations: HashSet::default(),
            })),
            handle_state,
            pending_activations: vec![],
        }
    }
}

pub struct NandoScheduler {
    // NOTE we probably don't _need_ to keep the handle to the scheduler thread around
    _scheduler_submission_thread: JoinHandle<()>,
    _scheduler_completion_threads: Vec<JoinHandle<()>>,
    sender: crossbeam_channel::Sender<SchedulerInputKind>,

    nando_libraries: DashMap<String, libloading::Library>,
    epic_control_registry: Arc<DashMap<EcbId, ControlRegistryEntry>>,
    cached_functions: DashMap<String, Arc<ActivationFunction>>,
    cached_metadata: DashMap<String, NandoMetadata>,

    physical_plan_manager: Arc<PhysicalPlanManager>,

    #[cfg(feature = "telemetry")]
    telemetry_handle: telemetry::TelemetryEventSender,
}

impl NandoScheduler {
    fn new(
        object_tracker: Arc<ObjectTracker>,
        config: SchedulerConfig,
        num_worker_threads: u16,
    ) -> Self {
        let (send, recv) =
            crossbeam_channel::bounded::<SchedulerInputKind>(config.input_channel_capacity.into());

        // FIXME this wil probably grow out of control
        let pending_txn_dependencies: Arc<DashMap<ActivationId, TxnDependency>> =
            Arc::new(DashMap::with_capacity(128));

        let aggregate_executor_stats = Arc::new(AggregateExecutorStats::new_with_capacity(
            num_worker_threads.into(),
        ));
        for i in 0..num_worker_threads {
            aggregate_executor_stats
                .available_executors
                .insert(i.into());
            aggregate_executor_stats.executor_stats.insert(
                i.into(),
                ExecutorStats {
                    current_queue_depth: AtomicUsize::new(0),
                    historical: AtomicUsize::new(0),
                    owned_objects: HashSet::with_capacity(100),
                },
            );
        }

        let num_cores = core_affinity::get_core_ids()
            .expect("failed to get list of core ids")
            .len();

        // NOTE the logging thread is pinned to the "last" core for now, so we start pinning
        // scheduler threads starting from the second-to-last core id.
        let submission_thread_core_id = num_cores - 2;

        let scheduler_submission_thread = {
            let recv = recv.clone();
            let pending_txn_dependencies_clone = Arc::clone(&pending_txn_dependencies);
            let aggregate_executor_stats = Arc::clone(&aggregate_executor_stats);
            let core_id = core_affinity::CoreId {
                id: submission_thread_core_id,
            };
            let object_tracker = Arc::clone(&object_tracker);
            thread::Builder::new()
                .name("scheduler-submission-thread".to_string())
                .spawn(move || {
                    if !core_affinity::set_for_current(core_id) {
                        eprintln!("failed to set core affinity for scheduler thread");
                    }

                    let nando_executor_instance = NandoExecutor::get_nando_executor(None, None);
                    let executor_channels = nando_executor_instance.init(object_tracker);
                    let executor_queue_capacity =
                        nando_executor_instance.get_input_channel_capacity();

                    Self::poll_and_schedule(
                        recv,
                        pending_txn_dependencies_clone,
                        aggregate_executor_stats,
                        executor_channels,
                        executor_queue_capacity,
                    );
                })
                .expect("failed to spawn scheduler thread")
        };

        // FIXME better initial capacity
        let epic_control_registry = Arc::new(DashMap::with_capacity(1 << 16));
        // let epic_control_registry = Arc::new(SccMap::with_capacity(1 << 16));
        let num_completion_threads = config.num_completion_threads;
        let mut completion_thread_handles = Vec::with_capacity(num_completion_threads.into());
        let mut log_scheduler_queue_send_handles =
            Vec::with_capacity(num_completion_threads.into());
        for i in 0..num_completion_threads {
            let (log_scheduler_queue_send, log_scheduler_queue_recv) =
                RingBuffer::new(config.log_channel_capacity.into());
            log_scheduler_queue_send_handles.push(log_scheduler_queue_send);
            completion_thread_handles.push({
                let send = send.clone();
                let epic_control_registry = Arc::clone(&epic_control_registry);
                let aggregate_executor_stats = Arc::clone(&aggregate_executor_stats);
                let core_id = core_affinity::CoreId {
                    id: (submission_thread_core_id - (i as usize + 1)),
                };
                thread::Builder::new()
                    .name(format!("scheduler-completion-thread-{}", i))
                    .spawn(move || {
                        if !core_affinity::set_for_current(core_id) {
                            eprintln!(
                                "failed to set core affinity for scheduler completion thread {}",
                                i
                            );
                        }
                        Self::poll_log_queue(
                            log_scheduler_queue_recv,
                            send,
                            aggregate_executor_stats,
                            epic_control_registry,
                        );
                    })
                    .expect("failed to spawn scheduler thread")
            });
        }

        LogManager::get_txn_log_manager(None)
            .init_persistence_loop(log_scheduler_queue_send_handles);

        let loaded_libraries = DashMap::with_capacity(config.library_paths.len());
        for (library_key, path) in &config.library_paths {
            // FIXME we should probably just load the resolver function and be done
            let lib = unsafe { libloading::Library::new(path).unwrap() };
            loaded_libraries.insert(library_key.clone(), lib);
        }

        let physical_plan_manager = Arc::clone(PhysicalPlanManager::get_physical_plan_manager(
            config.physical_plans_path,
            Some(num_worker_threads),
        ));

        #[cfg(feature = "telemetry")]
        let telemetry_handle = telemetry::get_telemetry_handle();

        Self {
            _scheduler_submission_thread: scheduler_submission_thread,
            _scheduler_completion_threads: completion_thread_handles,
            sender: send,
            nando_libraries: loaded_libraries,
            cached_functions: DashMap::new(),
            cached_metadata: DashMap::new(),
            epic_control_registry,

            physical_plan_manager,

            #[cfg(feature = "telemetry")]
            telemetry_handle,
        }
    }

    // FIXME should probably return an Arc
    pub fn get_nando_scheduler(
        maybe_object_tracker: Option<Arc<ObjectTracker>>,
        maybe_nando_lib_config: Option<Config>,
    ) -> &'static NandoScheduler {
        let nando_scheduler =
            Self::get_nando_scheduler_mut(maybe_object_tracker, maybe_nando_lib_config);

        nando_scheduler
    }

    pub fn get_nando_scheduler_mut(
        maybe_object_tracker: Option<Arc<ObjectTracker>>,
        maybe_nando_lib_config: Option<Config>,
    ) -> &'static mut NandoScheduler {
        static mut INSTANCE: MaybeUninit<NandoScheduler> = MaybeUninit::uninit();
        static mut ONCE: Once = Once::new();

        unsafe {
            ONCE.call_once(|| {
                let nando_lib_config = maybe_nando_lib_config
                    .expect("cannot initialize nando scheduler without a valid config");
                INSTANCE.as_mut_ptr().write(NandoScheduler::new(
                    maybe_object_tracker.expect(
                        "cannot initialize nando scheduler without a valid object tracker instance",
                    ),
                    nando_lib_config.scheduler_config,
                    nando_lib_config.executor_config.num_worker_threads,
                ));
            });
        }

        unsafe { &mut *INSTANCE.as_mut_ptr() }
    }

    // FIXME this return type is horrible because it forces us to re-run the ownership tracker
    // args resolution check in TM (because there's no way to return the list of object ids to wait
    // on).
    pub fn handle_subtask_completion(
        &self,
        task_id: EcbId,
        tasks_to_notify: &Vec<(
            epic_control::DownstreamTaskDependency,
            epic_control::TaskStatus,
            Option<NandoResult>,
        )>,
        submit_local: bool,
    ) -> SubtaskCompletionResult {
        let ownership_tracker = OwnershipTracker::get_ownership_tracker(None);

        let mut schedulable_tasks = vec![];
        let mut external_tasks = vec![];
        let mut external_tasks_to_notify: HashMap<
            EcbId,
            Vec<(
                epic_control::DownstreamTaskDependency,
                epic_control::TaskStatus,
                Option<NandoResult>,
            )>,
        > = HashMap::new();

        let mut tasks_to_notify: Vec<(
            EcbId,
            epic_control::DownstreamTaskDependency,
            epic_control::TaskStatus,
            Option<NandoResult>,
        )> = tasks_to_notify
            .into_iter()
            .map(|(t, s, r)| (task_id, t.clone(), s.clone(), r.clone()))
            .collect();

        loop {
            if tasks_to_notify.is_empty() {
                break;
            }
            let mut new_notifications = vec![];

            for (completed_task_id, task_to_notify, status, result) in &tasks_to_notify {
                if !self.dependency_is_local(&task_to_notify) {
                    external_tasks_to_notify
                        .entry(*completed_task_id)
                        .and_modify(|e| {
                            e.push((task_to_notify.clone(), status.clone(), result.clone()))
                        })
                        .or_insert(vec![(
                            task_to_notify.clone(),
                            status.clone(),
                            result.clone(),
                        )]);
                    continue;
                }

                match self.handle_task_completion_for_dependency(
                    *completed_task_id,
                    task_to_notify,
                    *status,
                    result.clone(),
                ) {
                    None => {}
                    Some((TaskCompletionPropagationResult::TaskEntry(entry), None, None)) => {
                        {
                            let mut control_info = entry.control_info.write();
                            let meta = self.get_nando_metadata(&control_info.intent.name).expect(
                                &format!("failed to get meta for {}", control_info.intent.name),
                            );

                            let intent_name = control_info.intent.name.clone();
                            let task_is_schedulable = ownership_tracker
                                .args_are_resolvable_locally(
                                    meta.kind.is_read_only(),
                                    meta.mutable_argument_indices,
                                    Some(&intent_name),
                                    &mut control_info.intent.args,
                                );

                            match task_is_schedulable {
                                Schedulable::Immediately => {
                                    // NOTE @hack we want to push this up to the activation router
                                    // if it's a whomstone (or whomstone and move) intent because
                                    // of the established processing pipeline.
                                    let should_push_up = control_info.intent.is_whomstone_intent()
                                        || control_info.intent.is_whomstone_and_move_intent();
                                    if should_push_up {
                                        let spawned_task_to_fwd = control_info
                                            .control_block
                                            .to_task_control_info(control_info.intent.clone());
                                        external_tasks.push(spawned_task_to_fwd);
                                        continue;
                                    }

                                    if submit_local {
                                        let mut activation = generate_epic_activation!(
                                            control_info.intent,
                                            meta,
                                            self,
                                            control_info
                                                .control_block
                                                .get_associated_activation_id()
                                        );
                                        activation.set_task_control_info(
                                            control_info
                                                .control_block
                                                .to_task_control_info(control_info.intent.clone()),
                                        );
                                        let handle = NandoHandle::new_epic_top_level_handle(
                                            control_info.control_block.id,
                                        );
                                        let handle_state =
                                            Arc::clone(&handle.get_completion_state());
                                        activation.set_handle_state(handle_state);

                                        self.sender
                                            .try_send(SchedulerInputKind::Activation(activation))
                                            .expect("failed to submit downstream activation");

                                        for pending_activation in entry.pending_activations {
                                            self.sender
                                                .try_send(SchedulerInputKind::Activation(
                                                    pending_activation,
                                                ))
                                                .expect("failed to submit downstream activation");
                                        }

                                        continue;
                                    }
                                }
                                Schedulable::AfterWaitingForArrival(_) => {
                                    /* NOTE will be added to schedulable_tasks */
                                }
                                Schedulable::ArgsUnresolvableLocally => {
                                    let spawned_task_to_fwd = control_info
                                        .control_block
                                        .to_task_control_info(control_info.intent.clone());
                                    external_tasks.push(spawned_task_to_fwd);

                                    continue;
                                }
                            }
                        }

                        schedulable_tasks.push(entry);
                    }
                    Some((
                        TaskCompletionPropagationResult::Notifications(
                            notifications,
                            status,
                            mut _maybe_cache_invalidation_tasks,
                        ),
                        Some(task_id),
                        result_to_propagate,
                    )) => {
                        #[cfg(feature = "object-caching")]
                        if let Some(ref mut cache_invalidation_tasks) =
                            _maybe_cache_invalidation_tasks
                        {
                            if !cache_invalidation_tasks.is_empty() {
                                #[cfg(debug_assertions)]
                                println!("[DEBUG] Completed task with invalidations, will append to schedulable external tasks");
                                external_tasks.append(cache_invalidation_tasks);
                            }
                        }

                        for task_to_notify in notifications.into_iter() {
                            if self.dependency_is_local(&task_to_notify) {
                                new_notifications.push((
                                    task_id,
                                    task_to_notify,
                                    status,
                                    result_to_propagate.clone(),
                                ));
                                continue;
                            }

                            external_tasks_to_notify
                                .entry(task_id)
                                .and_modify(|e| {
                                    e.push((
                                        task_to_notify.clone(),
                                        status.clone(),
                                        result_to_propagate.clone(),
                                    ))
                                })
                                .or_insert(vec![(
                                    task_to_notify.clone(),
                                    status.clone(),
                                    result_to_propagate.clone(),
                                )]);
                        }
                    }
                    Some((
                        TaskCompletionPropagationResult::MultiNotifications(
                            notifications_and_results,
                            status,
                            mut _maybe_cache_invalidation_tasks,
                        ),
                        Some(task_id),
                        _result_to_propagate,
                    )) => {
                        #[cfg(feature = "object-caching")]
                        if let Some(ref mut cache_invalidation_tasks) =
                            _maybe_cache_invalidation_tasks
                        {
                            if !cache_invalidation_tasks.is_empty() {
                                #[cfg(debug_assertions)]
                                println!("[DEBUG] Completed task with invalidations, will append to schedulable external tasks");
                                external_tasks.append(cache_invalidation_tasks);
                            }
                        }

                        for (task_to_notify, result) in notifications_and_results.into_iter() {
                            if self.dependency_is_local(&task_to_notify) {
                                new_notifications.push((
                                    task_id,
                                    task_to_notify,
                                    status,
                                    Some(result),
                                ));
                                continue;
                            }

                            external_tasks_to_notify
                                .entry(task_id)
                                .and_modify(|e| {
                                    e.push((task_to_notify.clone(), status, Some(result.clone())))
                                })
                                .or_insert(vec![(task_to_notify, status, Some(result))]);
                        }
                    }
                    _ => panic!("should not happen"),
                }
            }

            tasks_to_notify = new_notifications;
        }

        let external_tasks_to_notify = external_tasks_to_notify
            .into_iter()
            .map(|(k, v)| {
                let mut notification = TaskCompletionNotification::new(k, v);
                let Some(task_entry) = self.epic_control_registry.get(&k) else {
                    println!("No task entry for {k} (handling completion of {task_id})");
                    return notification;
                };
                let task_entry = task_entry.value().control_info.read();
                notification
                    .subgraph_allocations
                    .extend(&task_entry.allocations);

                // FIXME this somehow needs to be the transitive closure of tasks under the current
                // task
                for spawned_task in &task_entry.control_block.spawned_tasks {
                    let Some(task_entry) = self.epic_control_registry.get(&spawned_task.id) else {
                        continue;
                    };
                    let task_entry = task_entry.value().control_info.read();
                    notification
                        .subgraph_allocations
                        .extend(&task_entry.allocations);
                }

                notification
            })
            .collect();

        SubtaskCompletionResult {
            schedulable_tasks,
            spawned_tasks_to_fwd: external_tasks,
            external_tasks_to_notify,
        }
    }

    pub fn get_nando_metadata_external(nando_name: &String) -> Result<NandoMetadata, String> {
        let nando_scheduler = NandoScheduler::get_nando_scheduler(None, None);
        nando_scheduler.get_nando_metadata(&nando_name)
    }

    fn get_nando_metadata(&self, full_name: &String) -> Result<NandoMetadata, String> {
        match self.cached_metadata.get(full_name) {
            Some(entry) => return Ok(*entry.value()),
            None => {}
        }

        let nando_path: Vec<&str> = full_name.split("::").collect();
        match nando_path.len() {
            1 => {
                let metadata = built_ins::get_nando_metadata(nando_path[0]);
                self.cached_metadata.insert(full_name.to_string(), metadata);
                Ok(metadata)
            }
            len @ _ => {
                if len == 0 {
                    let error_msg = format!(
                        "Nando path '{:?}' cannot be parsed, invalid namespace",
                        nando_path
                    );
                    eprintln!("{}", error_msg);
                    return Err(error_msg);
                }

                let key = nando_path[0];
                let intent_name = nando_path[1..].join("::");

                let lib = match self.nando_libraries.get(key) {
                    Some(l) => l,
                    _ => {
                        let error_msg = format!("library '{}' is not visible to the runtime", key);
                        eprintln!("{}", error_msg);
                        return Err(error_msg);
                    }
                };
                let get_metadata_function: libloading::Symbol<fn(&str) -> NandoMetadata> =
                    unsafe { lib.get(b"get_nando_metadata").unwrap() };

                let metadata = get_metadata_function(&intent_name);
                self.cached_metadata.insert(full_name.to_string(), metadata);
                Ok(metadata)
            }
        }
    }

    fn get_nando_function(&self, full_name: &String) -> Result<Arc<ActivationFunction>, String> {
        match self.cached_functions.get(full_name) {
            Some(entry) => return Ok(entry.value().clone()),
            None => {}
        }

        let nando_path: Vec<&str> = full_name.split("::").collect();
        match nando_path.len() {
            1 => match built_ins::resolve_function(nando_path[0]) {
                Some(f) => {
                    let entry = Arc::clone(&f);
                    self.cached_functions.insert(full_name.to_string(), entry);
                    Ok(f)
                }
                None => {
                    let error_msg = format!(
                        "nando '{:?}' does not exist in the global namespace",
                        nando_path,
                    );
                    eprintln!("{}", error_msg);
                    Err(error_msg)
                }
            },
            len @ _ => {
                if len == 0 {
                    let error_msg = format!(
                        "Nando path '{:?}' cannot be parsed, invalid namespace",
                        nando_path
                    );
                    eprintln!("{}", error_msg);
                    return Err(error_msg);
                }

                let key = nando_path[0];
                let intent_name = nando_path[1..].join("::");

                let lib = match self.nando_libraries.get(key) {
                    Some(l) => l,
                    _ => {
                        let error_msg = format!("library '{}' is not visible to the runtime", key);
                        eprintln!("{}", error_msg);
                        return Err(error_msg);
                    }
                };
                let resolve_function: libloading::Symbol<
                    fn(&str) -> Option<Arc<ActivationFunction>>,
                > = unsafe { lib.get(b"resolve_function").unwrap() };

                match resolve_function(&intent_name) {
                    Some(f) => {
                        let entry = Arc::clone(&f);
                        self.cached_functions.insert(full_name.to_string(), entry);
                        Ok(f)
                    }
                    None => {
                        let error_msg =
                            format!("nando '{:?}' is not visible to the scheduler", nando_path,);
                        eprintln!("{}", error_msg);
                        Err(error_msg)
                    }
                }
            }
        }
    }

    #[cfg(feature = "telemetry")]
    #[inline(always)]
    fn submit_telemetry_event(&self, event: telemetry::TelemetryEvent) {
        telemetry::submit_telemetry_event(&self.telemetry_handle, event);
    }

    pub fn schedule_activation(
        &self,
        txn_id: logging::TxnId,
        activation_intent: NandoActivationIntent,
        with_plan: Option<String>,
    ) -> Result<ScheduleResult, String> {
        let activation_function = match self.get_nando_function(&activation_intent.name) {
            Ok(f) => f,
            Err(e) => return Err(e),
        };

        let function_metadata = match self.get_nando_metadata(&activation_intent.name) {
            Ok(metadata) => metadata,
            Err(e) => return Err(e),
        };

        let (handle, activation) = match function_metadata.spawns_nandos {
            false => {
                let handle = NandoHandle::new_nando_handle();
                let handle_state = Arc::clone(&handle.get_completion_state());
                (
                    handle,
                    NandoActivation::new(
                        activation_intent,
                        txn_id,
                        Some(handle_state),
                        activation_function,
                        function_metadata,
                    ),
                )
            }
            true => {
                let host_idx = OwnershipTracker::get_host_idx_static(None)
                    .expect("cannot spawn an epic without a valid host idx");

                let mut activation = NandoActivation::new(
                    activation_intent,
                    txn_id,
                    None,
                    activation_function,
                    function_metadata,
                );
                let ecb_id = EcbId::new(host_idx, activation.activation_id);
                let handle = NandoHandle::new_epic_top_level_handle(ecb_id.clone());
                let handle_state = Arc::clone(&handle.get_completion_state());
                activation.set_handle_state(handle_state);
                activation.set_top_level();
                let mut spawned_task =
                    epic_control::SpawnedTask::new_top_level(ecb_id, &activation.activation_intent);

                if let Some(plan) = with_plan {
                    // top-level always has index of 1
                    spawned_task.planning_context.idx = 1;
                    spawned_task.planning_context.plan_key = plan;
                }

                #[cfg(feature = "telemetry")]
                {
                    let telemetry_ts = telemetry::zoned_timestamp_now();
                    spawned_task.intent.args = activation.get_args_as_intent_args();
                    self.submit_telemetry_event(telemetry::TelemetryEvent::new_spawn(
                        &spawned_task,
                        telemetry_ts.clone(),
                    ));
                }

                activation.set_task_control_info(spawned_task);

                (handle, activation)
            }
        };

        #[cfg(feature = "timing-core-sched")]
        {
            let timestamp = Instant::now();

            activation.set_timing_entity("transaction_start", timestamp);
            #[cfg(feature = "timing")]
            activation.set_timing_entity("tm_to_sched_queue_start", timestamp);
        }

        self.sender
            .try_send(SchedulerInputKind::Activation(activation))
            .expect("failed to enqueue top-level epic activation");

        if function_metadata.spawns_nandos {
            return Ok(ScheduleResult::Epic(handle));
        }

        Ok(ScheduleResult::Nando(handle))
    }

    pub fn schedule_externally_spawned_task(
        &self,
        txn_id: logging::TxnId,
        #[cfg(feature = "object-caching")] mut spawned_task: epic_control::SpawnedTask,
        #[cfg(not(feature = "object-caching"))] spawned_task: epic_control::SpawnedTask,
    ) -> Result<ScheduleResult, String> {
        let activation_function = match self.get_nando_function(&spawned_task.intent.name) {
            Ok(f) => f,
            Err(e) => return Err(e),
        };

        let function_metadata = match self.get_nando_metadata(&spawned_task.intent.name) {
            Ok(metadata) => metadata,
            Err(e) => return Err(e),
        };

        let spawned_task_id = spawned_task.id;
        let handle = NandoHandle::new_epic_top_level_handle(spawned_task_id);
        let handle_state = Arc::clone(&handle.get_completion_state());
        let mut activation = NandoActivation::new(
            spawned_task.intent.clone(),
            txn_id,
            Some(handle_state),
            activation_function,
            function_metadata,
        );

        #[cfg(debug_assertions)]
        println!(
            "spawned task {} should invalidate? {}",
            activation.activation_intent.name,
            activation.meta.invalidate_on_completion.is_some()
        );

        #[cfg(feature = "object-caching")]
        if !spawned_task.mask_invalidations && activation.meta.invalidate_on_completion.is_some() {
            spawned_task.mask_invalidations = true;
        }

        activation.set_task_control_info(spawned_task);
        activation.set_non_top_level();

        self.sender
            .try_send(SchedulerInputKind::Activation(activation))
            .expect("failed to enqueue top-level epic activation");

        Ok(ScheduleResult::Epic(handle))
    }

    pub fn schedule_parkable_entry(&self, entry: ControlRegistryEntry) {
        #[cfg(feature = "telemetry")]
        {
            // TODO capture task information
        }

        let activation = {
            let control_info = entry.control_info.read();
            let mut activation = generate_epic_activation_from_rest!(
                control_info.control_block,
                control_info.intent,
                self
            );
            let handle = NandoHandle::new_epic_top_level_handle(control_info.control_block.id);
            let handle_state = Arc::clone(&handle.get_completion_state());
            activation.set_handle_state(handle_state);

            activation
        };

        self.sender
            .try_send(SchedulerInputKind::Activation(activation))
            .expect("failed to submit downstream activation");

        for pending_activation in entry.pending_activations {
            self.sender
                .try_send(SchedulerInputKind::Activation(pending_activation))
                .expect("failed to submit downstream activation");
        }
    }

    pub fn store_externally_spawned_task(&self, spawned_task: epic_control::SpawnedTask) {
        let spawned_task_ecb: epic_control::ECB = (&spawned_task).into();
        self.epic_control_registry.insert(
            spawned_task_ecb.id,
            ControlRegistryEntry::new(spawned_task_ecb, spawned_task.intent.clone(), None),
        );
    }

    // FIXME this is a bit clunky
    pub fn get_ecb_result_and_status(
        &self,
        ecb_id: EcbId,
    ) -> Option<(Option<NandoResult>, epic_control::TaskStatus)> {
        let control_info = match self.epic_control_registry.get(&ecb_id) {
            Some(r) => Arc::clone(&r.value().control_info),
            None => return None,
        };

        let control_info = control_info.read();
        Some((
            control_info.control_block.get_result(),
            control_info.get_status(),
        ))
    }

    fn get_safe_majority(
        worker_allocations: &Vec<(ExecutorWorkerId, MoveWeight)>,
    ) -> ExecutorWorkerId {
        // The below is a weighted version of the traditional Boyer-Moore majority vote algorithm,
        // where each entry has a vote whose weight is inversely proportional to the time elapsed
        // since it was moved to its current owning thread (to avoid/minimize thrashing).

        if worker_allocations.len() == 0 {
            // FIXME this should just pick an executor at random, but it will do for now.
            return 0;
        }

        let mut current_majority_votes = 0.0;
        let mut majority_core = 0;

        for (executor_worker_id, move_weight) in worker_allocations {
            if current_majority_votes <= 0.0 {
                majority_core = *executor_worker_id;
                current_majority_votes = *move_weight;
                continue;
            }

            if majority_core == *executor_worker_id {
                current_majority_votes += *move_weight;
            } else {
                current_majority_votes -= *move_weight;
            }
        }

        majority_core
    }

    fn compute_activation_dependencies(
        activation: &NandoActivation,
        stats_map: &mut HashMap<ObjectId, Rc<RefCell<ObjectStats>>>,
        num_executors: usize,
        existing_cores: &mut Vec<(u16, f32)>,
        dependency_stats: &mut Vec<(ObjectId, Rc<RefCell<ObjectStats>>)>,
        rng: &mut SmallRng,
        aggregate_executor_stats: Arc<AggregateExecutorStats>,
    ) -> (usize, Option<Vec<ActivationId>>) {
        let mut activation_args = activation.get_resolved_args().borrow_mut();

        let mutable_argument_indices = match activation.meta.mutable_argument_indices {
            Some(i) => i,
            None => &[],
        };

        // Iterate over resolved activation arguments, and replace objects with either a ReadWrite
        // or a ReadOnly variant, depending on MV state of each object and the transaction type.
        for (idx, arg) in activation_args.iter_mut().enumerate() {
            match arg {
                ResolvedNandoArgument::Object(oa) => process_object_argument!(
                    activation,
                    mutable_argument_indices,
                    oa,
                    idx,
                    dependency_stats,
                    stats_map,
                    aggregate_executor_stats,
                    existing_cores
                ),
                ResolvedNandoArgument::Objects(ref mut oas) => {
                    for oa in oas {
                        process_object_argument!(
                            activation,
                            mutable_argument_indices,
                            oa,
                            idx,
                            dependency_stats,
                            stats_map,
                            aggregate_executor_stats,
                            existing_cores
                        )
                    }
                }
                _ => continue,
            }
        }

        match dependency_stats.len() {
            0 => (rng.gen::<usize>() % num_executors, None),
            1 => (existing_cores[0].0 as usize, None),
            _ => {
                let target_executor_thread = Self::get_safe_majority(existing_cores);
                let mut txn_dependencies = vec![];
                for (object_id, entry) in dependency_stats {
                    let mut entry_mut = entry.borrow_mut();
                    if entry_mut.current_owning_thread != target_executor_thread {
                        if entry_mut.latest_scheduled_txn > entry_mut.latest_committed_txn {
                            txn_dependencies.push(entry_mut.latest_scheduled_txn);
                        }
                        entry_mut.move_weight = 1.0;
                        entry_mut.times_scheduled = 1;
                    } else {
                        entry_mut.move_weight = (entry_mut.move_weight - 0.01).max(0.01);
                        entry_mut.times_scheduled += 1;
                    }

                    entry_mut.current_owning_thread = target_executor_thread;
                    if let Some(ref mut e) = aggregate_executor_stats
                        .executor_stats
                        .get_mut(&target_executor_thread.try_into().unwrap())
                    {
                        e.owned_objects.remove(object_id);
                    }
                }

                if txn_dependencies.is_empty() {
                    return (target_executor_thread as usize, None);
                }

                (target_executor_thread as usize, Some(txn_dependencies))
            }
        }
    }

    // FIXME this should be configurable
    const EXECUTOR_DEPTH_THRESHOLD: f64 = 0.95;

    #[inline]
    fn at_capacity(aggregate_executor_stats: Arc<AggregateExecutorStats>) -> bool {
        aggregate_executor_stats.available_executors.is_empty()
    }

    #[cfg(feature = "telemetry")]
    pub fn log_stats(&self) {
        self.sender
            .try_send(SchedulerInputKind::OutputTelemetryStats)
            .expect("failed to submit telemetry output event");
    }

    // Task submission handling.
    pub fn poll_and_schedule(
        external_queue_recv: crossbeam_channel::Receiver<SchedulerInputKind>,
        pending_txn_dependencies: Arc<DashMap<ActivationId, TxnDependency>>,
        aggregate_executor_stats: Arc<AggregateExecutorStats>,
        // executor_channels: Vec<Rc<RefCell<Producer<NandoActivation>>>>,
        mut executor_channels: Vec<Producer<NandoActivation>>,
        executor_queue_capacity: usize,
    ) {
        let scheduler = Self::get_nando_scheduler(None, None);
        // FIXME initial capacity
        // FIXME @speed chances are that frequent lookups in this map are going to ruin the
        // scheduler's decision latency
        // FIXME probably switch to a fixed-size cache
        let mut stats_map: HashMap<ObjectId, Rc<RefCell<ObjectStats>>> =
            HashMap::with_capacity(8192);

        let mut pending_txns: HashMap<ActivationId, NandoActivation> = HashMap::with_capacity(128);

        let num_executors = executor_channels.len();
        let executor_queue_capacity = executor_queue_capacity as f64;

        // for zero-dependency ops.
        let mut last_rr_assigned_core = 0;

        // NOTE recycled across invocations of `compute_object_dependencies()`
        let mut existing_cores = Vec::with_capacity(8);
        // NOTE recycled across invocations of `compute_object_dependencies()`
        let mut dependency_stats = Vec::with_capacity(8);
        // NOTE recycled across invocations of `compute_object_dependencies()`
        let mut rng = SmallRng::from_entropy();

        #[cfg(feature = "telemetry")]
        let mut num_tasks_scheduled = 0;

        loop {
            #[cfg(feature = "timing-core-sched")]
            let (iteration_start, mut did_ext_work, did_log_work) =
                { (Instant::now(), false, false) };
            #[cfg(feature = "timing-core-sched")]
            let mut external_queue_work_duration = Duration::default();
            #[cfg(feature = "timing-core-sched")]
            let log_queue_work_duration = Duration::default();

            // attempt to schedule an externally requested activation
            match external_queue_recv.try_recv() {
                // match external_queue_recv.recv() {
                Ok(SchedulerInputKind::Activation(mut activation)) => {
                    if activation.activation_intent.name == "reset_scheduler_state" {
                        stats_map.clear();
                        for mut stat in aggregate_executor_stats.executor_stats.iter_mut() {
                            stat.owned_objects.clear();
                        }
                        activation.mark_done_and_wake();
                        continue;
                    }

                    #[cfg(feature = "timing-sched-submission")]
                    let start = Instant::now();
                    #[cfg(feature = "timing-sched-submission")]
                    let intent_name = activation.activation_intent.name.clone();

                    #[cfg(feature = "timing-core-sched")]
                    let external_queue_work_start = {
                        let timestamp = Instant::now();
                        did_ext_work = true;

                        #[cfg(feature = "timing")]
                        {
                            activation.set_timing_entity("tm_to_sched_queue_end", timestamp);
                            activation.set_timing_entity("scheduling_start", timestamp);
                        }

                        timestamp
                    };

                    let (target_executor_thread, txn_dependencies) = match activation.meta.kind {
                        NandoKind::ReadOnly => Self::compute_activation_dependencies(
                            &activation,
                            &mut stats_map,
                            num_executors,
                            &mut existing_cores,
                            &mut dependency_stats,
                            &mut rng,
                            Arc::clone(&aggregate_executor_stats),
                        ),
                        _ => Self::compute_activation_dependencies(
                            &activation,
                            &mut stats_map,
                            num_executors,
                            &mut existing_cores,
                            &mut dependency_stats,
                            &mut rng,
                            Arc::clone(&aggregate_executor_stats),
                        ),
                    };
                    existing_cores.drain(..);
                    dependency_stats.drain(..);

                    #[cfg(feature = "timing")]
                    activation.set_timing_entity("scheduling_end", Instant::now());

                    activation.executor = target_executor_thread;

                    #[cfg(feature = "telemetry")]
                    {
                        num_tasks_scheduled += 1;

                        if let Some(mut spawned_task) = activation.get_task_control_info() {
                            let queue_depth: usize = {
                                let producer: &Producer<NandoActivation> =
                                    executor_channels.get(target_executor_thread).unwrap();
                                producer.buffer().capacity() - producer.slots()
                            };
                            spawned_task.downgrade_parent_dependency();
                            // spawned_task.intent.args = activation.get_args_as_intent_args();
                            let telemetry_ts = telemetry::zoned_timestamp_now();
                            scheduler.submit_telemetry_event(
                                telemetry::TelemetryEvent::new_schedule(
                                    spawned_task.id,
                                    queue_depth,
                                    telemetry_ts,
                                ),
                            );
                        }
                    }

                    schedule_or_park!(
                        txn_dependencies,
                        pending_txns,
                        pending_txn_dependencies,
                        aggregate_executor_stats,
                        activation,
                        executor_channels,
                        executor_queue_capacity,
                        target_executor_thread
                    );

                    #[cfg(feature = "timing-core-sched")]
                    {
                        external_queue_work_duration = external_queue_work_start.elapsed();
                    }

                    #[cfg(feature = "timing-sched-submission")]
                    {
                        let duration = start.elapsed();
                        println!(
                            "scheduling {intent_name} took {}us ({}ns)",
                            duration.as_micros(),
                            duration.as_nanos()
                        );
                    }
                }
                Ok(SchedulerInputKind::ActivationCompletion(
                    activation_id,
                    maybe_argument_resolution,
                )) => {
                    match maybe_argument_resolution {
                        Some((txn, idx, argument)) => {
                            pending_txns.entry(txn).and_modify(|e| {
                                e.resolve_intent_arg(idx, argument, None);
                            });
                        }
                        None => {}
                    }

                    let pending_txn_id = match pending_txn_dependencies.remove(&activation_id) {
                        None => None,
                        Some((_, TxnDependency::PendingTxnRef { txn })) => {
                            let mut should_schedule = false;
                            pending_txn_dependencies.entry(txn).and_modify(|e| match e {
                                TxnDependency::PendingTxn {
                                    dependency_count,
                                    target_executor_thread: _,
                                    downstream_pending_txn: _,
                                } => {
                                    *dependency_count -= 1;
                                    if *dependency_count == 0 {
                                        should_schedule = true;
                                    }
                                }
                                TxnDependency::PendingTxnRef { .. } => {
                                    panic!("found chained PendingTxnRef entries for {}", txn,)
                                }
                            });

                            if should_schedule {
                                match pending_txn_dependencies.remove(&txn).unwrap() {
                                    (
                                        _,
                                        TxnDependency::PendingTxn {
                                            dependency_count: _,
                                            target_executor_thread: _,
                                            downstream_pending_txn: Some(tr),
                                        },
                                    ) => {
                                        pending_txn_dependencies.insert(txn, *tr);
                                    }
                                    (
                                        _,
                                        TxnDependency::PendingTxn {
                                            dependency_count: _,
                                            target_executor_thread: _,
                                            downstream_pending_txn: None,
                                        },
                                    ) => {}
                                    (_, TxnDependency::PendingTxnRef { .. }) => {
                                        panic!("found chained PendingTxnRef entries for {}", txn,)
                                    }
                                }
                            }

                            Some(txn)
                        }
                        Some((_, TxnDependency::PendingTxn { .. })) => {
                            panic!("should not happen?")
                        }
                    };

                    let Some(activation_id) = pending_txn_id else {
                        continue;
                    };

                    let mut activation = pending_txns.remove(&activation_id).unwrap();
                    let (target_executor_thread, txn_dependencies) = {
                        match activation.is_part_of_epic() {
                            false => (activation.executor, None),
                            true => match activation.meta.kind {
                                NandoKind::ReadOnly => {
                                    let target_executor_thread =
                                        last_rr_assigned_core % num_executors;
                                    last_rr_assigned_core += 1;
                                    (target_executor_thread, None)
                                }
                                _ => Self::compute_activation_dependencies(
                                    &activation,
                                    &mut stats_map,
                                    num_executors,
                                    &mut existing_cores,
                                    &mut dependency_stats,
                                    &mut rng,
                                    Arc::clone(&aggregate_executor_stats),
                                ),
                            },
                        }
                    };
                    activation.executor = target_executor_thread;
                    existing_cores.drain(..);
                    dependency_stats.drain(..);

                    schedule_or_park!(
                        txn_dependencies,
                        pending_txns,
                        pending_txn_dependencies,
                        aggregate_executor_stats,
                        activation,
                        executor_channels,
                        executor_queue_capacity,
                        target_executor_thread
                    );
                }
                Ok(SchedulerInputKind::UpdateMvEnabled(object_id, mv_enabled)) => {
                    #[cfg(debug_assertions)]
                    println!("about to update mv enabled for {object_id}: {mv_enabled}");
                    stats_map
                        .entry(object_id)
                        .and_modify(|e| {
                            let mut entry = e.borrow_mut();
                            // I am very smart
                            entry.mv_disabled = !mv_enabled;
                        })
                        .or_insert({
                            let mut entry = ObjectStats::default();
                            entry.mv_disabled = !mv_enabled;
                            let min_core_stats = aggregate_executor_stats
                                .executor_stats
                                .iter()
                                .map(|e| (*e.key(), e.value().owned_objects.len()))
                                .min_by_key(|(_, a)| *a)
                                .unwrap();
                            entry.current_owning_thread = min_core_stats.0.try_into().unwrap();
                            if let Some(ref mut e) = aggregate_executor_stats
                                .executor_stats
                                .get_mut(&min_core_stats.0)
                            {
                                e.owned_objects.insert(object_id);
                            }
                            Rc::new(RefCell::new(entry))
                        });
                }
                Ok(SchedulerInputKind::OutputTelemetryStats) => {
                    #[cfg(feature = "telemetry")]
                    {
                        let telemetry_ts = telemetry::zoned_timestamp_now();
                        scheduler.submit_telemetry_event(
                            telemetry::TelemetryEvent::new_scheduler_metrics(
                                num_tasks_scheduled,
                                telemetry_ts,
                            ),
                        );
                        let intent_name = "log_executor_stats".to_string();
                        let activation_function = match scheduler.get_nando_function(&intent_name) {
                            Ok(f) => f,
                            Err(e) => {
                                eprintln!("Ignoring `OutputTelemetryStats`: {e}");
                                continue;
                            }
                        };

                        let function_metadata = match scheduler.get_nando_metadata(&intent_name) {
                            Ok(metadata) => metadata,
                            Err(e) => {
                                eprintln!("Ignoring `OutputTelemetryStats`: {e}");
                                continue;
                            }
                        };

                        for channel in &mut executor_channels {
                            let activation = NandoActivation::new_executor_stats_activation(
                                None,
                                activation_function.clone(),
                                function_metadata.clone(),
                            );
                            match channel.push(activation) {
                                Ok(_) => (),
                                Err(PushError::Full(_)) => todo!("queue was full",),
                            };
                        }
                    }
                }
                Err(crossbeam_channel::TryRecvError::Empty) => (),
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    // FIXME better error handling here
                    panic!("external queue has been disconnected unexpectedly");
                }
            }

            #[cfg(feature = "timing-core-sched")]
            {
                let iteration_duration = iteration_start.elapsed();
                if iteration_duration.as_nanos() <= 500 {
                    continue;
                }

                let mut output_string = String::with_capacity(100);
                output_string.push_str(&format!(
                    "Scheduler iteration time: {}ns",
                    iteration_duration.as_nanos()
                ));

                if did_ext_work {
                    output_string.push_str(&format!(
                        "\nExternal Queue work time: {}ns",
                        external_queue_work_duration.as_nanos()
                    ));
                }

                if did_log_work {
                    output_string.push_str(&format!(
                        "\nLog queue work time: {}ns",
                        log_queue_work_duration.as_nanos()
                    ));
                }

                println!("{}", output_string);
            }
        }
    }

    // Task completion handling
    fn poll_log_queue(
        mut log_queue_recv: Consumer<NandoActivation>,
        scheduler_queue_send: crossbeam_channel::Sender<SchedulerInputKind>,
        aggregate_executor_stats: Arc<AggregateExecutorStats>,
        epic_control_registry: Arc<DashMap<EcbId, ControlRegistryEntry>>,
    ) {
        #[cfg(not(feature = "offline"))]
        let ownership_tracker = OwnershipTracker::get_ownership_tracker(None);
        let executor_queue_capacity =
            NandoExecutor::get_nando_executor(None, None).get_input_channel_capacity() as f64;
        let scheduler = Self::get_nando_scheduler(None, None);
        let plan_manager = PhysicalPlanManager::get_physical_plan_manager(None, None);
        let transaction_manager = TransactionManager::get_transaction_manager(None, None);

        #[cfg(not(feature = "offline"))]
        let own_host_id = ownership_tracker
            .get_own_host_id()
            .expect("failed to get own hostname");

        // reused across tasks
        let mut tasks_to_notify = Vec::with_capacity(16);

        loop {
            // attempt to process one commit entry
            match log_queue_recv.pop() {
                Ok(committed_activation) => {
                    #[cfg(feature = "timing-sched-completion")]
                    let start = Instant::now();
                    #[cfg(feature = "timing-sched-completion")]
                    let intent_name = committed_activation.activation_intent.name.clone();

                    let activation_id = committed_activation.activation_id;
                    #[cfg(feature = "object-caching")]
                    if committed_activation.is_pending() {
                        // since not actually committed...
                        let unexecuted_activation = committed_activation;
                        let Some(ResolvedNandoArgument::ControlBlock(dependency_id)) =
                            unexecuted_activation.get_resolved_args().borrow_mut().pop()
                        else {
                            panic!("no dependency identifier in unexecuted activation");
                        };

                        // FIXME this check is wrong, since a task might have been executed already
                        // and it still might be in the table because it is waiting for its spawns
                        // to finish. This means that some pending tasks added to existing
                        // dependencies will not get executed ever. We need to check if the entry
                        // we found in the table corresponds to an already executed transaction
                        // (in which case we just resubmit) or if it is still pending.
                        match epic_control_registry.get_mut(&dependency_id) {
                            None => {
                                // Either the registry has not been updated with the park task or
                                // the task has already been scheduled, either way we can resubmit.
                                scheduler_queue_send
                                    .try_send(SchedulerInputKind::Activation(unexecuted_activation))
                                    .expect("failed to submit activation of dependency");
                            }
                            Some(ref mut e) => {
                                e.value_mut()
                                    .pending_activations
                                    .push(unexecuted_activation);
                            }
                        }

                        continue;
                    }

                    #[cfg(feature = "telemetry")]
                    let telemetry_commit_ts = telemetry::zoned_timestamp_now();

                    #[cfg(feature = "telemetry")]
                    if committed_activation.is_part_of_epic() {
                        let task_id = committed_activation.get_task_control_id().unwrap();
                        scheduler.submit_telemetry_event(telemetry::TelemetryEvent::new_commit(
                            task_id,
                            telemetry_commit_ts.clone(),
                        ));
                    }

                    // Potentially disable MV for large objects, according to owning executor's
                    // status.
                    for (object_id, mv_enabled) in
                        committed_activation.mv_updates.borrow_mut().drain(..)
                    {
                        scheduler_queue_send
                            .try_send(SchedulerInputKind::UpdateMvEnabled(object_id, mv_enabled))
                            .expect("failed to propagate mv status notification");
                    }

                    #[cfg(not(feature = "offline"))]
                    {
                        let log_entry = committed_activation.activation_log_entry.borrow();
                        let objects_to_publish: Vec<(ObjectId, ObjectVersion)> = log_entry
                            .write_set
                            .iter()
                            .map(|(e, _)| (e.get_id(), e.get_version()))
                            .collect();
                        transaction_manager.publish_objects(objects_to_publish);
                    }

                    // update executor stats after successful nando completion
                    {
                        let stats = aggregate_executor_stats
                            .executor_stats
                            .get(&committed_activation.executor)
                            .unwrap();
                        let current_depth =
                            stats.current_queue_depth.fetch_sub(1, Ordering::Relaxed);

                        if (current_depth - 1) as f64 / executor_queue_capacity
                            < (Self::EXECUTOR_DEPTH_THRESHOLD - 0.05)
                        {
                            aggregate_executor_stats
                                .available_executors
                                .insert(committed_activation.executor);
                        }

                        stats.historical.fetch_add(1, Ordering::Relaxed);
                    }

                    // process completion of standalone nandos
                    if !committed_activation.is_part_of_epic() {
                        if committed_activation.is_failed() {
                            committed_activation.mark_failed_and_wake();
                        } else {
                            committed_activation.mark_done_and_wake();
                        }

                        #[cfg(feature = "timing-sched-completion")]
                        {
                            let duration = start.elapsed();
                            println!(
                                "[{:?}] completion of {intent_name} took {}us ({}ns)",
                                std::thread::current().name(),
                                duration.as_micros(),
                                duration.as_nanos()
                            );
                        }

                        scheduler_queue_send
                            .try_send(SchedulerInputKind::ActivationCompletion(
                                activation_id,
                                None,
                            ))
                            .expect("failed to submit activation of dependency");

                        continue;
                    }

                    // Epic task graph updates.
                    let ecb = match committed_activation.get_ecb() {
                        None => {
                            eprintln!(
                                "no ecb for workflow nando of activation '{}': {:#?}",
                                committed_activation.activation_intent.name, committed_activation,
                            );
                            committed_activation.mark_failed_and_wake();

                            continue;
                        }
                        Some(e) => e,
                    };
                    let ecb = ecb.borrow();

                    let mut should_mark_done = false;
                    if ecb.is_top_level() {
                        // need to resolve the pending (request) handle with a notification about
                        // triggering an epic.
                        let handle_state = committed_activation
                            .handle_state
                            .as_ref()
                            .expect("no handle for top-level");
                        let activation_handle = handle_state.borrow();
                        activation_handle.append_spawn(ecb.id);

                        if !ecb.has_spawned_tasks() {
                            should_mark_done = true;
                        }
                    }

                    // the below will simply notify the client that triggered the epic
                    // (if any) about the fact
                    committed_activation.mark_triggered_and_wake();

                    tasks_to_notify.truncate(0);
                    if !ecb.has_spawned_tasks() && ecb.has_downstream_dependents() {
                        let mut downstream_dependents = ecb.get_downstream_dependents();
                        let mut downstream_dependents = downstream_dependents.drain(..);
                        let ecb_result = ecb.get_result();
                        let status = match committed_activation.is_failed() {
                            true => epic_control::TaskStatus::Failure,
                            false => epic_control::TaskStatus::Success,
                        };

                        match committed_activation
                            .activation_intent
                            .is_update_caches_intent()
                        {
                            true => {
                                let cached_object_ids = ecb_result.unwrap().get_object_ids();
                                for cached_object_id in cached_object_ids.into_iter() {
                                    tasks_to_notify.push((
                                        downstream_dependents.next().unwrap(),
                                        status,
                                        Some(IPtr::new(cached_object_id, 0, 0).into()),
                                    ));
                                }
                            }
                            false => tasks_to_notify.extend(
                                downstream_dependents
                                    .into_iter()
                                    .map(|d| (d, status, ecb_result.clone())),
                            ),
                        }
                    }

                    let ecb_id = ecb.id;
                    let handle_completion_state = committed_activation.get_handle_state();

                    let cloned_intent = committed_activation.activation_intent.clone();
                    let control_entry = ControlRegistryEntry::new(
                        ecb.clone(),
                        cloned_intent,
                        handle_completion_state,
                    );

                    // Process spawned tasks or completion notifications if no spawned tasks exist.
                    if committed_activation.is_done() {
                        let committed_ecb_status = epic_control::TaskStatus::Success;
                        let mut control_info = control_entry.control_info.write();
                        {
                            let log_entry = committed_activation.activation_log_entry.borrow();
                            control_info
                                .allocations
                                .extend(log_entry.write_set.iter().map(|(e, _)| e.get_id()));
                        }

                        let ecb = &mut control_info.control_block;
                        let at_capacity = Self::at_capacity(Arc::clone(&aggregate_executor_stats));

                        if ecb.has_spawned_tasks() {
                            // FIXME @hack we need proper result propagation logic for epics
                            ecb.set_last_spawn_as_result_task();
                        }

                        // NOTE we need to call this before we check if the committed task spawned
                        // any tasks because if we're following a pre-defined plan we might be
                        // introducing new tasks under the committed task in the form of post
                        // actions. If we're not following a plan this will have no effect on the
                        // committed task if it's a leaf task.
                        let mut processed_tasks = plan_manager.process_post_and_next_steps(
                            ecb,
                            &committed_activation,
                            at_capacity,
                        );

                        #[cfg(feature = "telemetry")]
                        for tasks in processed_tasks.values() {
                            for task in tasks {
                                scheduler.submit_telemetry_event(
                                    telemetry::TelemetryEvent::new_spawn(
                                        &task.0,
                                        telemetry_commit_ts.clone(),
                                    ),
                                );
                            }
                        }

                        if !ecb.has_spawned_tasks() && processed_tasks.is_empty() {
                            let parent_dependency = ecb.get_parent_as_downstream_dependency();
                            if !parent_dependency.is_none() && ecb.should_notify_parent() {
                                tasks_to_notify.push((
                                    parent_dependency,
                                    committed_ecb_status,
                                    ecb.get_result().clone(),
                                ));
                            }

                            let task_completion_result = scheduler.handle_subtask_completion(
                                ecb.id,
                                &tasks_to_notify,
                                false,
                            );

                            for entry in task_completion_result.schedulable_tasks.into_iter() {
                                let activation = {
                                    #[cfg(not(feature = "offline"))]
                                    {
                                        let mut control_info = entry.control_info.write();
                                        let meta = scheduler
                                            .get_nando_metadata(&control_info.intent.name)
                                            .expect(&format!(
                                                "failed to get meta for {}",
                                                control_info.intent.name
                                            ));

                                        let intent_name = control_info.intent.name.clone();
                                        let task_is_schedulable = ownership_tracker
                                            .args_are_resolvable_locally(
                                                meta.kind.is_read_only(),
                                                meta.mutable_argument_indices,
                                                Some(&intent_name),
                                                &mut control_info.intent.args,
                                            );

                                        match task_is_schedulable {
                                            Schedulable::AfterWaitingForArrival(_objects) => {
                                                // NOTE @hack the below is a shortcut to force the system to
                                                // wait on whatever dependencies of the spawned
                                                // task we need to wait on without introducing a
                                                // new call explicitly for that
                                                transaction_manager.forward_spawned_task_to(
                                                    control_info
                                                        .control_block
                                                        .to_task_control_info(
                                                            control_info.intent.clone(),
                                                        ),
                                                    own_host_id.clone(),
                                                );
                                                continue;
                                            }
                                            Schedulable::ArgsUnresolvableLocally => unreachable!(),
                                            Schedulable::Immediately => {}
                                        }
                                    }

                                    let control_info = entry.control_info.read();

                                    let mut activation = generate_epic_activation_from_rest!(
                                        control_info.control_block,
                                        control_info.intent,
                                        scheduler
                                    );
                                    let handle = NandoHandle::new_epic_top_level_handle(
                                        control_info.control_block.id,
                                    );
                                    let handle_state = Arc::clone(&handle.get_completion_state());
                                    activation.set_handle_state(handle_state);

                                    activation
                                };

                                // TODO @physical we should push these somewhere
                                scheduler_queue_send
                                    .try_send(SchedulerInputKind::Activation(activation))
                                    .expect("failed to submit downstream activation");

                                for pending_activation in entry.pending_activations {
                                    scheduler_queue_send
                                        .try_send(SchedulerInputKind::Activation(
                                            pending_activation,
                                        ))
                                        .expect("failed to submit downstream activation");
                                }
                            }

                            #[cfg(not(feature = "offline"))]
                            {
                                let mut spawned_tasks = task_completion_result.spawned_tasks_to_fwd;
                                let external_tasks_to_notify =
                                    task_completion_result.external_tasks_to_notify;

                                for spawned_task in &mut spawned_tasks {
                                    if !(committed_activation
                                        .activation_intent
                                        .is_spawn_cache_intent()
                                        && spawned_task.intent.is_whomstone_and_move_intent())
                                    {
                                        continue;
                                    }

                                    let output = committed_activation
                                        .get_result()
                                        .get_inner_result()
                                        .unwrap();
                                    spawned_task.intent.args.push(output);
                                }
                                transaction_manager.forward_spawned_tasks(&spawned_tasks);

                                #[cfg(feature = "observability")]
                                let notify_start = std::time::Instant::now();
                                #[cfg(feature = "observability")]
                                let notification_len = external_tasks_to_notify.len();
                                for external_task_to_notify in external_tasks_to_notify.into_iter()
                                {
                                    // FIXME performance, transitive inclusion of allocations from
                                    // subgraph
                                    transaction_manager
                                        .forward_task_completion(external_task_to_notify);
                                }

                                #[cfg(feature = "observability")]
                                {
                                    let notify_duration = notify_start.elapsed();
                                    OPS_NS_NOTIFY_HISTOGRAM
                                        .with_label_values(&[
                                            &notification_len.to_string(),
                                            &committed_activation.activation_intent.name,
                                        ])
                                        .observe(notify_duration.as_micros() as f64);
                                }
                            }
                        } else {
                            #[cfg(not(feature = "offline"))]
                            let spawned_task_locations: HashMap<
                                HostId,
                                Vec<EcbId>,
                            > = processed_tasks
                                .iter()
                                .filter(|(location, _)| {
                                    location.is_local() || location.get_remote().is_some()
                                })
                                .map(|(location, tasks)| {
                                    let task_ids: Vec<EcbId> =
                                        tasks.iter().map(|t| t.0.id).collect();
                                    match location.is_local() {
                                        true => (own_host_id.clone(), task_ids),
                                        false => (location.get_remote().unwrap(), task_ids),
                                    }
                                })
                                .collect();

                            if let Some(ref mut tasks_to_submit) =
                                processed_tasks.remove(&SubmissionLocation::Local)
                            {
                                let tmp_vec: Vec<(epic_control::SpawnedTask, Option<Schedulable>)> =
                                    tasks_to_submit.drain(..).collect();
                                let (cache_tasks_to_submit, mut tasks_to_submit): (
                                    Vec<(epic_control::SpawnedTask, Option<Schedulable>)>,
                                    Vec<(epic_control::SpawnedTask, Option<Schedulable>)>,
                                ) = tmp_vec
                                    .into_iter()
                                    .partition(|t| t.0.intent.is_spawn_cache_intent());
                                tasks_to_submit.extend(cache_tasks_to_submit);

                                // Here we need to iterate over the spawned tasks backwards so that we
                                // store tasks with local dependencies in the control registry so that we can then
                                // store a pointer to their control block in their task dependencies.
                                for (mut spawned_task, arg_schedulable) in
                                    tasks_to_submit.drain(..).rev()
                                {
                                    if !spawned_task.is_schedulable() {
                                        // We "park" this transaction locally, and wait for dependencies to
                                        // be met.
                                        park_spawned_task!(spawned_task, epic_control_registry);
                                        continue;
                                    }

                                    if let Some(arg_schedulable) = arg_schedulable {
                                        match arg_schedulable {
                                            #[cfg(feature = "offline")]
                                            Schedulable::AfterWaitingForArrival(_objects) => {
                                                panic!()
                                            }
                                            #[cfg(not(feature = "offline"))]
                                            Schedulable::AfterWaitingForArrival(_objects) => {
                                                // NOTE @hack the below is a shortcut to force the system to
                                                // wait on whatever dependencies of the spawned
                                                // task we need to wait on without introducing a
                                                // new call explicitly for that
                                                transaction_manager.forward_spawned_task_to(
                                                    spawned_task,
                                                    own_host_id.clone(),
                                                );
                                                continue;
                                            }
                                            Schedulable::ArgsUnresolvableLocally => unreachable!(),
                                            Schedulable::Immediately => {}
                                        }
                                    }

                                    spawned_task.parent_task =
                                        epic_control::DownstreamTaskDependency::ParentDependency(
                                            epic_control::DependencyRef::Ptr(Arc::clone(
                                                &control_entry.control_info,
                                            )),
                                        );

                                    for downstream_dependent in
                                        &mut spawned_task.downstream_dependents
                                    {
                                        let Some(ecb_id) = downstream_dependent.get_inner_ecb_id()
                                        else {
                                            continue;
                                        };

                                        if let Some(entry) = epic_control_registry.get(&ecb_id) {
                                            downstream_dependent
                                                .upgrade_to_ptr(Arc::clone(&entry.control_info));
                                        }
                                    }

                                    let spawned_task_meta = scheduler
                                        .get_nando_metadata(&spawned_task.intent.name)
                                        .unwrap();

                                    #[cfg(feature = "object-caching")]
                                    if !spawned_task.intent.is_invalidation_spawn_intent()
                                        && (!spawned_task.mask_invalidations
                                            && (spawned_task_meta
                                                .invalidate_on_completion
                                                .is_some()
                                                || ecb.mask_invalidations))
                                    {
                                        spawned_task.mask_invalidations = true;
                                    }

                                    let mut activation = generate_epic_activation!(
                                        spawned_task.intent,
                                        spawned_task_meta,
                                        scheduler,
                                        spawned_task.get_associated_activation_id()
                                    );
                                    let handle =
                                        NandoHandle::new_epic_top_level_handle(spawned_task.id);
                                    let handle_state = Arc::clone(&handle.get_completion_state());
                                    activation.set_handle_state(handle_state);

                                    activation.set_task_control_info(spawned_task);

                                    scheduler_queue_send
                                        .try_send(SchedulerInputKind::Activation(activation))
                                        .expect("failed to submit activation to local scheduler");
                                }
                            }

                            #[cfg(not(feature = "offline"))]
                            {
                                for (submission_location, tasks) in processed_tasks {
                                    if let SubmissionLocation::Remote(host_id) = submission_location
                                    {
                                        transaction_manager.forward_spawned_tasks_to(
                                            tasks.into_iter().map(|t| t.0).collect(),
                                            spawned_task_locations.clone(),
                                            host_id,
                                        );
                                    } else {
                                        println!(
                                            "About to forward {} tasks with unknown remote",
                                            tasks.len()
                                        );
                                        transaction_manager.forward_spawned_tasks(
                                            &tasks.into_iter().map(|t| t.0).collect(),
                                        );
                                    }
                                }
                            }
                        }
                    } else if committed_activation.is_failed() {
                        let control_info = control_entry.control_info.read();
                        let notifications = control_info.get_notification_targets();

                        let tasks_to_notify = notifications
                            .into_iter()
                            .map(|t| (t, epic_control::TaskStatus::Failure, None))
                            .collect();

                        let _ =
                            scheduler.handle_subtask_completion(ecb.id, &tasks_to_notify, false);
                        should_mark_done = false;
                    }

                    if should_mark_done {
                        #[cfg(debug_assertions)]
                        println!(
                            "Marking {} as done",
                            committed_activation.activation_intent.name
                        );
                        let mut control_info = control_entry.control_info.write();
                        control_info.mark_completed();
                        committed_activation.mark_done_and_wake();

                        if ecb.is_top_level() {
                            let handle_state = committed_activation
                                .handle_state
                                .as_ref()
                                .expect("no handle for top-level");
                            let activation_handle = handle_state.borrow();
                            let output = activation_handle.get_output();
                            if let Some(r) = output.get(0) {
                                match r.get_inner_result() {
                                    None => {}
                                    Some(r) => {
                                        let ecb = &mut control_info.control_block;
                                        ecb.set_result(r);
                                    }
                                }
                            }
                        }
                    }

                    epic_control_registry.insert(ecb_id, control_entry);

                    if should_mark_done && ecb.is_top_level() {
                        transaction_manager.mark_epic_completed(
                            ecb.id,
                            None,
                            epic_control::TaskStatus::Success,
                        );
                    }

                    #[cfg(feature = "timing-sched-completion")]
                    {
                        let duration = start.elapsed();
                        println!(
                            "[{:?}] completion of {intent_name} took {}us ({}ns)",
                            std::thread::current().name(),
                            duration.as_micros(),
                            duration.as_nanos()
                        );
                    }
                }
                Err(PopError::Empty) => {}
            }
        }
    }

    fn dependency_is_local(&self, dependency: &epic_control::DownstreamTaskDependency) -> bool {
        match dependency {
            epic_control::DownstreamTaskDependency::None => {
                panic!("should not call is_local on a null dependency")
            }
            epic_control::DownstreamTaskDependency::ControlDependency(dep_ref)
            | epic_control::DownstreamTaskDependency::DataDependency(dep_ref, _)
            | epic_control::DownstreamTaskDependency::ParentDependency(dep_ref) => {
                // NOTE here we rely on the fact that we overwrite a ptr dependency on egress to an
                // EcbRef. If a dependency is a pointer, we can immediately assume it's local.
                if dep_ref.is_ptr() {
                    return true;
                }

                let dependent_ecb_id = match dep_ref.get_inner_ecb_id() {
                    None => panic!("no ecb id available for task dependency"),
                    Some(ecb_id) => ecb_id,
                };
                self.epic_control_registry.contains_key(&dependent_ecb_id)
            }
        }
    }

    /// Note that the below operation is a no-op if called on a dependency that is not owned by the
    /// current host. It is the responsibility of the caller of this function to perform the
    /// ownership checks necessary to ensure that the dependency is local, and to also check if the
    /// result can be further processed locally or need to be propagated to other workers.
    fn handle_task_completion_for_dependency(
        &self,
        task_id: EcbId,
        dependency: &epic_control::DownstreamTaskDependency,
        status: epic_control::TaskStatus,
        result: Option<NandoResult>,
    ) -> Option<(
        TaskCompletionPropagationResult,
        Option<EcbId>,
        Option<NandoResult>,
    )> {
        let res = match dependency {
            epic_control::DownstreamTaskDependency::None => return None,
            epic_control::DownstreamTaskDependency::ParentDependency(dep_ref) => {
                let entry = entry_from_dep_ref!(dep_ref, self.epic_control_registry);
                let mut entry = entry.write();

                if !entry.add_completed_task(task_id, status) {
                    return None;
                }

                entry
                    .control_block
                    .maybe_set_result_from_task(task_id, result.clone());

                if !entry.all_tasks_have_joined() {
                    return None;
                }

                let status = entry.update_status();
                #[cfg(feature = "telemetry")]
                {
                    let telemetry_ts = telemetry::zoned_timestamp_now();
                    self.submit_telemetry_event(telemetry::TelemetryEvent::new_completed(
                        entry.control_block.id,
                        task_id,
                        telemetry_ts,
                    ));
                }

                let entry = parking_lot::RwLockWriteGuard::downgrade(entry);

                let cache_invalidation_tasks: Option<Vec<epic_control::SpawnedTask>> =
                    match &entry.cache_invalidation_tasks {
                        None => None,
                        Some(cit) => match cit.is_empty() {
                            true => None,
                            false => Some(cit.clone()),
                        },
                    };

                let result_to_propagate = entry.control_block.get_result();

                #[cfg(feature = "object-caching")]
                if entry.intent.is_update_caches_intent() {
                    let parent_ecb_id = entry.control_block.id;
                    let mut downstream_dependents = entry.control_block.get_downstream_dependents();
                    let mut downstream_dependents = downstream_dependents.drain(..);
                    let cached_object_ids = result_to_propagate.unwrap().get_object_ids();

                    let mut notifications = Vec::with_capacity(cached_object_ids.len());
                    for cached_object_id in cached_object_ids.into_iter() {
                        notifications.push((
                            downstream_dependents.next().unwrap(),
                            IPtr::new(cached_object_id, 0, 0).into(),
                        ));
                    }

                    return Some((
                        TaskCompletionPropagationResult::MultiNotifications(
                            notifications,
                            status,
                            cache_invalidation_tasks,
                        ),
                        Some(parent_ecb_id),
                        None,
                    ));
                }

                let parent_ecb_id = entry.control_block.id;
                let mut notifications = vec![];
                if !entry.control_block.is_top_level() {
                    notifications = entry.get_notification_targets();
                } else {
                    let completion_timestamp = None;
                    let transaction_manager =
                        TransactionManager::get_transaction_manager(None, None);
                    transaction_manager.mark_epic_completed(
                        parent_ecb_id,
                        completion_timestamp,
                        status,
                    );
                }

                Some((
                    TaskCompletionPropagationResult::Notifications(
                        notifications,
                        status,
                        cache_invalidation_tasks,
                    ),
                    Some(parent_ecb_id),
                    result_to_propagate,
                ))
            }
            epic_control::DownstreamTaskDependency::DataDependency(dep_ref, dependency_arg_idx) => {
                {
                    let entry = entry_from_dep_ref!(dep_ref, self.epic_control_registry);
                    let mut entry = entry.write();

                    // check if upstream task failed, if so propagate failed status
                    if status.is_failed() {
                        entry.mark_failed();

                        let notifications = entry.get_notification_targets();
                        let downstream_ecb_id = entry.control_block.id;

                        return Some((
                            TaskCompletionPropagationResult::Notifications(
                                notifications,
                                status,
                                None,
                            ),
                            Some(downstream_ecb_id),
                            None,
                        ));
                    }

                    let name = entry.intent.name.clone();
                    let result = match result {
                        None => {
                            eprintln!(
                                "cannot resolve arg at idx {dependency_arg_idx} for '{name}' without a result"
                                );

                            entry.mark_failed();
                            let notifications = entry.get_notification_targets();
                            let downstream_ecb_id = entry.control_block.id;

                            return Some((
                                TaskCompletionPropagationResult::Notifications(
                                    notifications,
                                    status,
                                    None,
                                ),
                                Some(downstream_ecb_id),
                                None,
                            ));
                        }
                        Some(r) => r,
                    };

                    #[cfg(feature = "telemetry")]
                    let result_to_log = result.clone();

                    entry
                        .intent
                        .resolve_intent_arg(*dependency_arg_idx, result, Some(task_id));

                    #[cfg(feature = "telemetry")]
                    {
                        let telemetry_ts = telemetry::zoned_timestamp_now();
                        self.submit_telemetry_event(
                            telemetry::TelemetryEvent::new_data_notification(
                                *dependency_arg_idx,
                                &result_to_log,
                                task_id,
                                entry.control_block.id,
                                telemetry_ts,
                            ),
                        );
                    }

                    if !entry.is_immediately_runnable() {
                        return None;
                    }
                }

                let dependent_ecb_id = dep_ref.get_inner_ecb_id().unwrap();
                Some((
                    TaskCompletionPropagationResult::TaskEntry(
                        self.epic_control_registry
                            .remove(&dependent_ecb_id)
                            .unwrap()
                            .1,
                    ),
                    None,
                    None,
                ))
            }
            epic_control::DownstreamTaskDependency::ControlDependency(dep_ref) => {
                {
                    let entry = entry_from_dep_ref!(dep_ref, self.epic_control_registry);
                    let mut entry = entry.write();

                    if status.is_failed() {
                        entry.mark_failed();

                        let notifications = entry.get_notification_targets();
                        let downstream_ecb_id = entry.control_block.id;

                        return Some((
                            TaskCompletionPropagationResult::Notifications(
                                notifications,
                                status,
                                None,
                            ),
                            Some(downstream_ecb_id),
                            None,
                        ));
                    }

                    entry.control_block.mark_control_dependency_done(task_id);

                    #[cfg(feature = "telemetry")]
                    {
                        let telemetry_ts = telemetry::zoned_timestamp_now();
                        self.submit_telemetry_event(
                            telemetry::TelemetryEvent::new_control_notification(
                                task_id,
                                entry.control_block.id,
                                telemetry_ts,
                            ),
                        );
                    }

                    if !entry.is_immediately_runnable() {
                        return None;
                    }
                }

                let dependent_ecb_id = dep_ref.get_inner_ecb_id().unwrap();
                Some((
                    TaskCompletionPropagationResult::TaskEntry(
                        self.epic_control_registry
                            .remove(&dependent_ecb_id)
                            .unwrap()
                            .1,
                    ),
                    None,
                    None,
                ))
            }
        };

        res
    }
}
