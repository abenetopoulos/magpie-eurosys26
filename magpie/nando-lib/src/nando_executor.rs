#![allow(dead_code)]

use std::collections::HashMap;
#[cfg(feature = "object-caching")]
use std::collections::HashSet;
use std::mem::MaybeUninit;
use std::ptr::copy_nonoverlapping;
use std::sync::{Arc, Once};
use std::thread;
#[cfg(any(feature = "timing", feature = "timing-exec", feature = "telemetry"))]
use std::time::Instant;

use core_affinity;
use execution_definitions::{
    activation::{NandoActivation, ObjectArgument, ObjectResolutionMode, ResolvedNandoArgument},
    nando_handle::ExecutionError,
};
#[cfg(feature = "observability")]
use lazy_static::lazy_static;
use logging::LogManager;
#[cfg(feature = "object-caching")]
use nando_support::{
    activation_id::ActivationId, epic_control, iptr::IPtr, HostIdx, NandoArgument,
};
use nando_support::{ecb_id::EcbId, nando_metadata::NandoKind, ObjectId, ObjectVersion};
use object_lib::tls as nando_tls;
use object_tracker::ObjectTracker;
use ownership_tracker::OwnershipTracker;
#[cfg(feature = "observability")]
use prometheus::{
    register_counter, register_counter_vec, register_histogram_vec, Counter, CounterVec, Encoder,
    HistogramVec,
};
use rtrb::{Consumer, PopError, Producer, RingBuffer};
#[cfg(feature = "telemetry")]
use telemetry;

#[cfg(feature = "object-caching")]
use crate::built_ins;
use crate::config::ExecutorConfig;

#[cfg(feature = "observability")]
lazy_static! {
    static ref SCHEDULED_TASK_COUNTER: CounterVec = register_counter_vec!(
        "executor_scheduled_tasks_total",
        "Number of activations scheduled on the current thread",
        &["thread_name"],
    )
    .unwrap();
}

#[derive(Default)]
struct Constraint {
    read_version: ObjectVersion,
    minimum_allowed_version: ObjectVersion,
    primary_idx: usize,
    secondary_idx: Option<usize>,
}

macro_rules! resolve_object_ref {
    (
        $activation:ident,
        $object_tracker:ident,
        $object_arg:ident,
        $constraints:ident,
        $mv_status:ident,
        #[cfg(feature = "object-caching")]
        $cached_objects:ident,
        #[cfg(feature = "object-caching")]
        $remote_caches_to_invalidate:ident,
        $object_references:ident,
        $idx:ident,
        $secondary_idx:expr
     ) => {{
        match $object_arg {
            ObjectArgument::UnresolvedObject((iptr, mode)) => match mode {
                ObjectResolutionMode::Ro => {
                    let ro_object = $object_tracker.get_latest_committed(iptr.object_id);

                    let Some(ro_object) = ro_object else {
                        eprintln!(
                            "Could not get read-only version of {} for {}",
                            iptr.object_id, $activation.activation_intent.name
                        );
                        return ResolutionResult::UnresolvableObject(iptr.object_id);
                    };

                    // set read version
                    $constraints.entry(ro_object.get_id()).and_modify(|t| {
                        t.read_version = ro_object.get_version();
                        // index of argument in resolved arg list
                        t.primary_idx = $idx;
                        t.secondary_idx = $secondary_idx;
                    });

                    // potentially update allowed versions of arguments
                    for reference in &$object_references {
                        match ro_object.get_version_constraint(*reference) {
                            None => {}
                            Some(constraint) => {
                                $constraints.entry(*reference).and_modify(|t| {
                                    t.minimum_allowed_version =
                                        std::cmp::max(constraint, t.minimum_allowed_version);
                                });
                            }
                        }
                    }

                    ResolvedNandoArgument::Object(ObjectArgument::ROObject(ro_object))
                }
                ObjectResolutionMode::Rw | ObjectResolutionMode::RwFirstLocalAccess => {
                    let target_object = $object_tracker.get(iptr.object_id);
                    let Some(target_object) = target_object else {
                        eprintln!(
                            "cannot resolve object {} for {}",
                            iptr.object_id, $activation.activation_intent.name
                        );
                        return ResolutionResult::UnresolvableObject(iptr.object_id);
                    };

                    let arg_mv_enabled = if mode.is_first_local_access() {
                        true
                    } else {
                        target_object.object_is_mv_enabled()
                    };

                    $mv_status.push((iptr.object_id, arg_mv_enabled));

                    #[cfg(feature = "object-caching")]
                    {
                        let is_cache_update_intent =
                            $activation.activation_intent.is_update_caches_intent()
                                || $activation
                                    .activation_intent
                                    .is_update_caches_internal_intent();
                        // We don't want to trigger any invalidations if all we're doing is
                        // spawning a new cache objects.
                        if !($activation.activation_intent.is_spawn_cache_intent()
                            || $activation.activation_intent.is_invalidation_spawn_intent()
                            || is_cache_update_intent)
                            && !$activation.should_mask_invalidations()
                        {
                            let maybe_caches = target_object.get_cached_versions();
                            if !maybe_caches.is_empty()
                                && target_object.get_invalidating_task().is_none()
                            {
                                #[cfg(debug_assertions)]
                                println!(
                                    "activation {} will cause invalidation of {:#?}",
                                    $activation.activation_intent.name, maybe_caches
                                );
                                let cached_object_version = target_object.get_version();
                                $cached_objects.push(ResolvedNandoArgument::Object(
                                    ObjectArgument::RWObject(target_object),
                                ));
                                $remote_caches_to_invalidate.extend(
                                    maybe_caches
                                        .iter()
                                        .map(|c| (iptr.object_id, *c, cached_object_version)),
                                );
                                continue;
                            }
                        }
                    }

                    // we are guaranteed to be running on the most recent version, so no reason
                    // to maintain a constraint on this object.
                    $constraints.remove(&target_object.id);

                    // potentially update allowed versions of arguments
                    for reference in &$object_references {
                        match target_object.get_version_constraint(*reference) {
                            None => {}
                            Some(constraint) => {
                                $constraints.entry(*reference).and_modify(|t| {
                                    t.minimum_allowed_version =
                                        std::cmp::max(constraint, t.minimum_allowed_version);
                                });
                            }
                        }
                    }

                    ResolvedNandoArgument::Object(ObjectArgument::RWObject(target_object))
                }
                _ => unreachable!("invalid mode for object argument"),
            },
            _ => unreachable!("resolved object argument found during resolution"),
        }
    }};
}

pub(crate) type ExecutorWorkerId = u16;

#[derive(Debug)]
pub(crate) enum ResolutionResult {
    Ok,
    UnresolvableObject(ObjectId),
    NeedInvalidation(
        Vec<ResolvedNandoArgument>,
        Vec<(ObjectId, ObjectId, ObjectVersion)>,
    ),
    NeedToWait(EcbId),
}

pub struct NandoExecutor {
    num_threads: u16,
    input_channel_capacity: usize,
}

impl NandoExecutor {
    pub fn new(config: ExecutorConfig, _object_tracker: Arc<ObjectTracker>) -> Self {
        let num_threads = config.num_worker_threads;

        Self {
            num_threads,
            input_channel_capacity: config.input_channel_capacity as usize,
        }
    }

    pub fn init(&self, object_tracker: Arc<ObjectTracker>) -> Vec<Producer<NandoActivation>> {
        let mut input_channels = Vec::with_capacity(self.num_threads as usize);
        for thread_id in 1..(self.num_threads + 1) {
            let object_tracker = Arc::clone(&object_tracker);
            let (work_queue_send, work_queue_recv) =
                RingBuffer::new(self.input_channel_capacity.into());
            let log_queue_send = LogManager::get_txn_log_manager(None).get_log_queue_sender();
            let core_id = core_affinity::CoreId {
                id: thread_id as usize,
            };

            thread::Builder::new()
                .name(format!("exec-thread-{}", thread_id))
                .spawn(move || {
                    if !core_affinity::set_for_current(core_id) {
                        eprintln!(
                            "failed to set core affinity for executor thread {}",
                            thread_id
                        );
                    } else {
                        println!(
                            "[{:?}] set core affinity to {:?}",
                            std::thread::current().name(),
                            core_id
                        );
                    }
                    nando_tls::init_txn_context();
                    object_tracker::object_tracker_tls::set_thread_local_object_tracker(
                        Arc::clone(&object_tracker),
                    );
                    Self::poll_and_execute(
                        work_queue_recv,
                        log_queue_send,
                        object_tracker,
                        thread_id,
                    );
                })
                .expect(&format!("failed to spawn executor thread {}", thread_id));

            input_channels.push(work_queue_send);
        }

        input_channels
    }

    pub fn get_nando_executor(
        config: Option<ExecutorConfig>,
        object_tracker: Option<Arc<ObjectTracker>>,
    ) -> &'static NandoExecutor {
        static mut INSTANCE: MaybeUninit<NandoExecutor> = MaybeUninit::uninit();
        static mut ONCE: Once = Once::new();

        unsafe {
            ONCE.call_once(|| {
                INSTANCE.as_mut_ptr().write(NandoExecutor::new(
                    config.expect("Cannot initialize executor without a valid configuration"),
                    object_tracker.expect(
                        "Cannot initialize executor without a valid object tracker instance",
                    ),
                ));
            });
        }

        unsafe { &*INSTANCE.as_ptr() }
    }

    /// Attempts to resolve target activation args.
    ///
    /// This method attempts to locally "resolve" the target activation's object-related arguments
    /// from [`IPtr`] instances into concrete objects or materialized read-only versions of the
    /// target objects. If this is an effectful nanotransaction and any of its data dependencies
    /// have been cached, it returns a list of objects that will have to be invalidated before this
    /// nanotransaction can execute.
    fn resolve_activation_args(
        constraints: &mut HashMap<ObjectId, Constraint>,
        mv_status: &mut Vec<(ObjectId, bool)>,
        activation: &NandoActivation,
        object_tracker: Arc<ObjectTracker>,
    ) -> ResolutionResult {
        let object_references = activation.get_object_references();
        let mut args = activation.get_resolved_args().borrow_mut();
        #[cfg(feature = "object-caching")]
        let mut cached_objects = vec![];
        #[cfg(feature = "object-caching")]
        let mut remote_caches_to_invalidate = vec![];
        #[cfg(feature = "object-caching")]
        let invalidation_trigger = None;

        // NOTE since we're reusing the constraints table across nanotransactions, we need to do
        // this before we start processing object args for this activation.
        constraints.clear();
        mv_status.clear();

        for reference in &object_references {
            constraints.insert(*reference, Constraint::default());
        }

        for (idx, intent_arg) in args.iter_mut().enumerate() {
            match intent_arg {
                ResolvedNandoArgument::Object(object_arg) => {
                    *intent_arg = resolve_object_ref!(
                        activation,
                        object_tracker,
                        object_arg,
                        constraints,
                        mv_status,
                        #[cfg(feature = "object-caching")]
                        cached_objects,
                        #[cfg(feature = "object-caching")]
                        remote_caches_to_invalidate,
                        object_references,
                        idx,
                        None
                    );
                }
                ResolvedNandoArgument::Objects(ref mut object_args) => {
                    for object_arg in object_args {
                        let r = resolve_object_ref!(
                            activation,
                            object_tracker,
                            object_arg,
                            constraints,
                            mv_status,
                            #[cfg(feature = "object-caching")]
                            cached_objects,
                            #[cfg(feature = "object-caching")]
                            remote_caches_to_invalidate,
                            object_references,
                            idx,
                            None
                        );

                        *object_arg = r.get_inner_object_argument().unwrap().clone();
                    }
                }
                _ => continue,
            }
        }

        #[cfg(feature = "object-caching")]
        {
            if !remote_caches_to_invalidate.is_empty() || invalidation_trigger.is_some() {
                #[cfg(debug_assertions)]
                println!(
                    "We'll try to undo our rewrites for intent {}: {:#?}",
                    activation.activation_intent.name, args,
                );
                for intent_arg in args.iter_mut() {
                    let new_arg = match intent_arg {
                        ResolvedNandoArgument::Object(ref mut oa) => match oa {
                            ObjectArgument::ROObject(ob) => ObjectArgument::UnresolvedObject((
                                ob.iptr_of(),
                                ObjectResolutionMode::Ro,
                            )),
                            ObjectArgument::RWObject(ob) => ObjectArgument::UnresolvedObject((
                                ob.iptr_of(),
                                ObjectResolutionMode::Rw,
                            )),
                            _ => continue,
                        },
                        _ => continue,
                    };

                    *intent_arg = ResolvedNandoArgument::Object(new_arg);
                }
            }

            if !remote_caches_to_invalidate.is_empty() {
                return ResolutionResult::NeedInvalidation(
                    cached_objects,
                    remote_caches_to_invalidate,
                );
            }

            if let Some(invalidation_trigger_id) = invalidation_trigger {
                // FIXME HACK this is just to expedite the cache invalidation feature, the args list
                // should not be abused like this.
                args.push(ResolvedNandoArgument::ControlBlock(invalidation_trigger_id));
                return ResolutionResult::NeedToWait(invalidation_trigger_id);
            }
        }

        for (argument_id, argument_info) in constraints {
            let read_version = argument_info.read_version;
            let minimum_allowed_version = argument_info.minimum_allowed_version;

            if read_version >= minimum_allowed_version {
                continue;
            }

            let ro_object = object_tracker.get_at(*argument_id, minimum_allowed_version);
            let Some(ro_object) = ro_object else {
                // FIXME don't panic here
                panic!(
                    "Could not get read-only version of {} at version {} (originally read at {})",
                    argument_id, minimum_allowed_version, read_version
                );
            };

            match argument_info.secondary_idx {
                None => {
                    args[argument_info.primary_idx] =
                        ResolvedNandoArgument::Object(ObjectArgument::ROObject(ro_object));
                }
                Some(secondary_idx) => {
                    if let ResolvedNandoArgument::Objects(ref mut os) =
                        args[argument_info.primary_idx]
                    {
                        os[secondary_idx] = ObjectArgument::ROObject(ro_object);
                    }
                }
            }
        }

        ResolutionResult::Ok
    }

    #[cfg(feature = "object-caching")]
    fn construct_cache_invalidation_subflow(
        a: &mut NandoActivation,
        maybe_cached_objects: &Vec<ResolvedNandoArgument>,
        maybe_caches: &Vec<(ObjectId, ObjectId, ObjectVersion)>,
        host_idx: HostIdx,
    ) -> NandoActivation {
        // FIXME avoid allocation
        let cached_object_arguments: Vec<NandoArgument> = maybe_cached_objects.iter().fold(
            Vec::with_capacity(maybe_cached_objects.len()),
            |mut acc, resolved_object_arg| {
                match resolved_object_arg {
                    ResolvedNandoArgument::Object(o) => acc.push(NandoArgument::Ref(o.get_iptr())),
                    ResolvedNandoArgument::Objects(ref os) => {
                        acc.extend(os.iter().map(|o| NandoArgument::Ref(o.get_iptr())))
                    }
                    _ => panic!("non-object argument passed in to invalidation epic setup"),
                };

                acc
            },
        );

        // Root activation will modify object headers to "under migration".
        let root_activation_intent =
            nando_support::NandoActivationIntent::new_for("set_under_invalidation".to_string());
        let root_activation_fn = built_ins::resolve_function(&root_activation_intent.name)
            .expect("failed to get invalidation built-in");
        let root_activation_meta = built_ins::get_nando_metadata(&root_activation_intent.name);
        let mut root_activation = {
            let mut activation = NandoActivation::new(
                root_activation_intent,
                a.activation_id.txn_id(),
                None,
                root_activation_fn,
                root_activation_meta,
            );

            // add cached object arg as invalidation targets.
            {
                let mut args = activation.get_resolved_args().borrow_mut();
                args.extend(maybe_cached_objects.clone());
            }

            if a.is_top_level() {
                activation.set_top_level();
                a.set_non_top_level();
            } else {
                activation.set_non_top_level();
            }

            match a.get_handle_state() {
                Some(hs) => {
                    activation.set_handle_state(hs);
                    a.handle_state = None;
                }
                None => {}
            }

            activation
        };

        let mut root_ecb = match a.get_task_control_info() {
            None => {
                // FIXME I think this case is wrong.
                epic_control::ECB::new_top_level(host_idx, root_activation.activation_id)
            }
            Some(ref task_info) => epic_control::ECB::new_from_dependency_info(task_info),
        };
        let root_ecb_id = root_ecb.id;

        let user_activation_updated_id = ActivationId::new_subtxn(&root_ecb_id.get_activation_id());
        let user_activation_ecb_id = EcbId::new(host_idx, user_activation_updated_id);
        {
            // Necessary for the `set_under_invalidation` intent to store the
            // invalidating task id in the header of the object whose caches are to be
            // invalidated.
            let mut args = root_activation.get_resolved_args().borrow_mut();
            args.insert(
                0,
                ResolvedNandoArgument::ControlBlock(user_activation_ecb_id),
            );
        }

        let root_ecb_parent_dep =
            epic_control::DownstreamTaskDependency::parent_dependency_from(root_ecb_id);
        let user_task = match a.get_task_control_info() {
            None => epic_control::SpawnedTask {
                id: user_activation_ecb_id,
                intent: a.activation_intent.clone(),
                parent_task: root_ecb_parent_dep.clone(),
                downstream_dependents: Vec::default(),
                upstream_control_dependencies: HashSet::new(),
                mask_invalidations: false,
                planning_context: epic_control::PlanningContext::default(),
                combinator_context: epic_control::CombinatorContext::NoCombinator,
            },
            Some(mut info) => {
                info.id = user_activation_ecb_id;
                info.parent_task = root_ecb_parent_dep.clone();

                info.downstream_dependents.truncate(0);

                info
            }
        };
        root_ecb.set_result_task(user_activation_ecb_id);

        a.activation_id = user_activation_updated_id;

        let mut sink_activation_intent =
            nando_support::NandoActivationIntent::new_for("set_caching_permissible".to_string());
        sink_activation_intent.args.extend(cached_object_arguments);
        let sink_activation_id = ActivationId::new_subtxn(&root_ecb_id.get_activation_id());
        let sink_activation_ecb_id = EcbId::new(host_idx, sink_activation_id);

        let mut sink_task = epic_control::SpawnedTask {
            id: sink_activation_ecb_id,
            intent: sink_activation_intent,
            parent_task: root_ecb_parent_dep.clone(),
            downstream_dependents: Vec::default(),
            upstream_control_dependencies: HashSet::new(),
            mask_invalidations: false,
            planning_context: epic_control::PlanningContext::default(),
            combinator_context: epic_control::CombinatorContext::NoCombinator,
        };
        let sink_task_downstream_dep =
            epic_control::DownstreamTaskDependency::control_dependency_from(sink_activation_ecb_id);

        // Create remote cache invalidation tasks
        for (original_object_id, cached_object_to_invalidate, cached_version) in maybe_caches {
            let mut invalidation_intent =
                nando_support::NandoActivationIntent::new_for("invalidate".to_string());

            invalidation_intent.args.extend_from_slice(&[
                NandoArgument::Ref(IPtr::new(*original_object_id, 0, 0)),
                NandoArgument::Ref(IPtr::new(*cached_object_to_invalidate, 0, 0)),
                <u64 as Into<NandoArgument>>::into(*cached_version),
            ]);

            let invalidation_activation_id =
                ActivationId::new_subtxn(&root_ecb_id.get_activation_id());
            let sub_ecb_id = EcbId::new(host_idx, invalidation_activation_id);

            let invalidation_task = epic_control::SpawnedTask {
                id: sub_ecb_id,
                intent: invalidation_intent,
                parent_task: root_ecb_parent_dep.clone(),
                downstream_dependents: vec![sink_task_downstream_dep.clone()],
                upstream_control_dependencies: HashSet::new(),
                mask_invalidations: false,
                planning_context: epic_control::PlanningContext::default(),
                combinator_context: epic_control::CombinatorContext::NoCombinator,
            };

            root_ecb.spawned_tasks.push(invalidation_task);
            sink_task.upstream_control_dependencies.insert(sub_ecb_id);
        }

        root_ecb.spawned_tasks.push(sink_task);
        root_ecb.notifying_tasks.insert(user_task.id);
        root_ecb.spawned_tasks.push(user_task);

        root_activation.set_ecb(root_ecb);

        root_activation
    }

    #[cfg(feature = "telemetry")]
    #[inline(always)]
    fn submit_telemetry_event(
        telemetry_handle: &telemetry::TelemetryEventSender,
        event: telemetry::TelemetryEvent,
    ) {
        telemetry::submit_telemetry_event(telemetry_handle, event);
    }

    fn poll_and_execute(
        mut work_queue_recv: Consumer<NandoActivation>,
        log_queue_send: crossbeam_channel::Sender<NandoActivation>,
        object_tracker: Arc<ObjectTracker>,
        worker_id: ExecutorWorkerId,
    ) {
        let host_idx = OwnershipTracker::get_host_idx_static(None)
            .expect("cannot spawn an epic without a valid host idx");
        #[cfg(feature = "observability")]
        let exec_thread_name = {
            let current_thread = thread::current();
            current_thread.name().unwrap().to_string()
        };

        #[cfg(feature = "telemetry")]
        let telemetry_handle = telemetry::get_telemetry_handle();

        #[cfg(feature = "telemetry")]
        let mut num_tasks_dequeued: usize = 0;

        // NOTE initial capacity doesn't really matter, but it should be big enough that we're
        // guaranteed not to have to resize the map during normal operations.
        // values are tuples where the first element is the read version, the second argument is
        // the minimum allowed version (based on constraints), adn the third is the argument's
        // index in the args list (to help with overriding the initially resolved value if need
        // be).
        // FIXME maybe a struct instead of a tuple?
        let mut constraints: HashMap<ObjectId, Constraint> = HashMap::with_capacity(32);

        let mut mv_status: Vec<(ObjectId, bool)> = Vec::with_capacity(16);

        loop {
            match work_queue_recv.pop() {
                Ok(mut a) => {
                    #[cfg(feature = "timing")]
                    {
                        let timestamp = Instant::now();
                        a.set_timing_entity("sched_to_exec_queue_end", timestamp);
                        a.set_timing_entity("execution_start", timestamp);
                    }

                    #[cfg(any(feature = "timing-exec", feature = "telemetry"))]
                    let start = Instant::now();

                    #[cfg(feature = "observability")]
                    SCHEDULED_TASK_COUNTER
                        .with_label_values(&[&exec_thread_name])
                        .inc();

                    #[cfg(feature = "telemetry")]
                    {
                        match a.is_executor_stats_activation() {
                            false => {
                                num_tasks_dequeued += 1;
                            }
                            true => {
                                let telemetry_ts = telemetry::zoned_timestamp_now();
                                Self::submit_telemetry_event(
                                    &telemetry_handle,
                                    telemetry::TelemetryEvent::new_executor_metrics(
                                        worker_id,
                                        num_tasks_dequeued,
                                        telemetry_ts,
                                    ),
                                );

                                continue;
                            }
                        }
                    }

                    let resolution_result = Self::resolve_activation_args(
                        &mut constraints,
                        &mut mv_status,
                        &a,
                        Arc::clone(&object_tracker),
                    );

                    #[cfg(feature = "telemetry")]
                    {
                        match a.is_part_of_epic() {
                            false => {}
                            true => {
                                let telemetry_ts = telemetry::zoned_timestamp_now();
                                let resolution_duration_ns = start.elapsed().as_nanos();
                                Self::submit_telemetry_event(
                                    &telemetry_handle,
                                    telemetry::TelemetryEvent::new_executor_resolution(
                                        a.get_task_control_id().unwrap(),
                                        resolution_duration_ns,
                                        telemetry_ts,
                                    ),
                                );
                            }
                        }
                    }

                    #[cfg(debug_assertions)]
                    println!(
                        "Resolution result for {} ('{}'): {:?}",
                        a.activation_id, a.activation_intent.name, resolution_result
                    );
                    if let ResolutionResult::NeedToWait(_task_id) = resolution_result {
                        log_queue_send
                            .send(a)
                            .expect("executor thread failed to push to txn log manager");

                        continue;
                    }

                    if let ResolutionResult::UnresolvableObject(object_id) = resolution_result {
                        let error = ExecutionError::UnresolvableObject(object_id.to_string());
                        a.set_status_failed(error);
                        log_queue_send
                            .send(a)
                            .expect("executor thread failed to push to txn log manager");

                        continue;
                    }

                    #[cfg(not(feature = "object-caching"))]
                    let mut activation_to_execute = {
                        match a.is_part_of_epic() {
                            false => a,
                            true => match a.is_top_level() {
                                true => {
                                    a.set_top_level_ecb(host_idx);
                                    a
                                }
                                false => a.set_ecb_from_dependency_info(),
                            },
                        }
                    };

                    #[cfg(feature = "object-caching")]
                    let mut activation_to_execute = {
                        let (maybe_cached_objects, maybe_caches) = match resolution_result {
                            ResolutionResult::Ok => (vec![], vec![]),
                            ResolutionResult::NeedInvalidation(
                                maybe_cached_objects,
                                maybe_caches,
                            ) => (maybe_cached_objects, maybe_caches),
                            ResolutionResult::UnresolvableObject(_) => {
                                unreachable!("unresolvable object")
                            }
                            ResolutionResult::NeedToWait(_) => unreachable!("need to wait"),
                        };

                        let is_invalidation_spawn_intent =
                            a.activation_intent.is_invalidation_spawn_intent();

                        let is_cache_update_intent = a.activation_intent.is_update_caches_intent()
                            || a.activation_intent.is_update_caches_internal_intent();

                        match maybe_cached_objects.is_empty()
                            || a.should_mask_invalidations()
                            || is_invalidation_spawn_intent
                            || is_cache_update_intent
                        {
                            true => {
                                #[cfg(debug_assertions)]
                                println!(
                                    "[DEBUG] will NOT construct cache invalidation flow for {}: {}",
                                    a.activation_intent.name,
                                    a.should_mask_invalidations(),
                                );
                                match a.is_part_of_epic() {
                                    false => {
                                        #[cfg(debug_assertions)]
                                        println!(
                                            "Activation of {:#?} not part of epic",
                                            a.activation_intent
                                        );
                                    }
                                    true => {
                                        // FIXME this is unnecessary since we already have an ecb
                                        // generated in the control registry
                                        let ecb = match a.is_top_level() {
                                            true => {
                                                let mut ecb = epic_control::ECB::new_top_level(
                                                    host_idx,
                                                    a.activation_id,
                                                );

                                                if !is_invalidation_spawn_intent {
                                                    ecb.set_mask_invalidations(
                                                        a.meta.invalidate_on_completion.is_some(),
                                                    );
                                                }

                                                match a.get_task_control_info() {
                                                    None => {}
                                                    Some(st) => {
                                                        ecb.planning_context =
                                                            st.planning_context.clone();
                                                    }
                                                }

                                                ecb
                                            }
                                            false => epic_control::ECB::new_from_dependency_info(
                                                &a.get_task_control_info().expect(&format!(
                                                    "no control info for sub-ecb of '{}'",
                                                    a.activation_intent.name
                                                )),
                                            ),
                                        };

                                        #[cfg(debug_assertions)]
                                        println!("setting ecb of {}", a.activation_id);
                                        a.set_ecb(ecb);
                                    }
                                }

                                a
                            }
                            false => {
                                #[cfg(debug_assertions)]
                                println!(
                                    "[DEBUG] will construct cache invalidation flow for {}",
                                    a.activation_intent.name
                                );
                                Self::construct_cache_invalidation_subflow(
                                    &mut a,
                                    &maybe_cached_objects,
                                    &maybe_caches,
                                    host_idx,
                                )
                            }
                        }
                    };

                    #[cfg(feature = "telemetry")]
                    {
                        match activation_to_execute.is_part_of_epic() {
                            false => {}
                            true => {
                                let telemetry_ts = telemetry::zoned_timestamp_now();
                                Self::submit_telemetry_event(
                                    &telemetry_handle,
                                    telemetry::TelemetryEvent::new_executor_dequeue(
                                        activation_to_execute.get_task_control_id().unwrap(),
                                        telemetry_ts,
                                    ),
                                );
                            }
                        }
                    }

                    #[cfg(debug_assertions)]
                    println!(
                        "[{:?}] about to execute '{}': {:#?}",
                        std::thread::current().name(),
                        activation_to_execute.activation_intent.name,
                        activation_to_execute.get_resolved_args(),
                    );

                    #[cfg(feature = "telemetry")]
                    let activation_ecb_id = activation_to_execute.get_task_control_id();

                    {
                        // FIXME can we do this without `AssertUnwindSafe`?
                        // TODO handle signals like SIGSEGV (e.g. using `signal-hook(-registry)`)
                        match activation_to_execute.call(object_tracker.clone()) {
                            Ok(_) => (),
                            Err(_) => {
                                eprintln!(
                                    "[{:?}] Failed while executing '{}', will undo effects",
                                    std::thread::current().name(),
                                    activation_to_execute.activation_intent.name,
                                );

                                // undo partial effects, if any
                                if !activation_to_execute.is_built_in() {
                                    let log_entry =
                                        activation_to_execute.activation_log_entry.borrow();
                                    for (ov_pair, allocated) in &log_entry.write_set {
                                        let object_id = ov_pair.get_id();
                                        if *allocated {
                                            object_tracker.delete_object(object_id);
                                            continue;
                                        }

                                        let object_first_ptr = object_tracker
                                            .get_source_address_mut(object_id)
                                            .expect("object in write set not local");
                                        for image in log_entry.images.get(&object_id).unwrap() {
                                            if image.get_post_value().is_empty() {
                                                continue;
                                            }

                                            let iptr = image.get_field();
                                            let pre_value = &image.get_pre_value();
                                            let start_offset = iptr.offset as usize;
                                            let size = iptr.size as usize;
                                            unsafe {
                                                let region_start = object_first_ptr
                                                    .byte_offset(start_offset as isize);
                                                copy_nonoverlapping(
                                                    pre_value.data.as_ptr(),
                                                    region_start,
                                                    size,
                                                );
                                            }
                                        }
                                    }
                                }

                                activation_to_execute
                                    .set_status_failed(ExecutionError::UnknownError());
                            }
                        };
                    }

                    if !activation_to_execute.status.is_failed() {
                        #[cfg(feature = "object-caching")]
                        {
                            if let Some(arg_indices_to_invalidate) =
                                activation_to_execute.meta.invalidate_on_completion
                            {
                                let resolved_args =
                                    activation_to_execute.get_resolved_args().borrow();
                                let ecb = activation_to_execute
                                    .get_ecb()
                                    .expect("missing control block for epic task");
                                let mut ecb = ecb.borrow_mut();
                                // FIXME turn this into an MRef
                                for arg_index_to_invalidate in arg_indices_to_invalidate {
                                    let arg_iptr = resolved_args[*arg_index_to_invalidate]
                                        .get_inner_object_argument()
                                        .unwrap()
                                        .get_iptr();
                                    ecb.insert_invalidation_spawn_sink_task(arg_iptr);
                                }
                            }
                        }

                        // Figure out how much work we need to do to maintain object versions (if any).
                        {
                            let resolved_args = activation_to_execute.get_resolved_args().borrow();
                            for arg in resolved_args.iter() {
                                for object_arg in arg.get_inner_object_arguments() {
                                    match object_arg {
                                        ObjectArgument::RWObject(o) => {
                                            #[cfg(debug_assertions)]
                                            println!("executor examining {}", o.get_id());
                                            let arg_object_id = o.get_id();
                                            let mv_enabled_post_call = o.object_is_mv_enabled();
                                            for (object_id, mv_enabled) in mv_status.iter_mut() {
                                                if arg_object_id != *object_id {
                                                    #[cfg(debug_assertions)]
                                                    println!("executor ignoring {object_id}");
                                                    continue;
                                                }

                                                #[cfg(debug_assertions)]
                                                println!(
                                                    "executor considering {object_id}: post call {} vs {}",
                                                    mv_enabled_post_call, mv_enabled
                                                );
                                                // Check if we crossed the threshold because of the executed
                                                // nando.
                                                if mv_enabled_post_call != *mv_enabled {
                                                    #[cfg(debug_assertions)]
                                                    println!(
                                                        "about to mark {} as non-mv-enabled",
                                                        arg_object_id
                                                    );
                                                    activation_to_execute.add_mv_update(
                                                        arg_object_id,
                                                        mv_enabled_post_call,
                                                    );
                                                }
                                            }
                                        }
                                        _ => continue,
                                    }
                                }
                            }

                            let log_entry = activation_to_execute.activation_log_entry.borrow();
                            // Iterate over objects that this nando allocated. As the executor thread
                            // is the de facto owner of the newly allocated objects (because they have
                            // not been published to the local scheduler yet) it is safe to resolve
                            // their IDs directly.
                            for (ov_pair, allocated) in log_entry.write_set.iter() {
                                if !allocated {
                                    continue;
                                }

                                let object_id = ov_pair.get_id();
                                let obj = object_tracker
                                    .get(object_id)
                                    .expect("failed to get newly allocated object");
                                if !obj.object_is_mv_enabled() {
                                    activation_to_execute.add_mv_update(object_id, false);
                                }
                            }
                        }

                        #[cfg(not(feature = "object-caching"))]
                        {
                            let mv_status = &mv_status;
                            match activation_to_execute.meta.kind {
                                NandoKind::ReadOnly => {}
                                _ => object_tracker.push_versions(
                                    &activation_to_execute.activation_log_entry.borrow(),
                                    Some(|e| {
                                        for (object_id, status) in mv_status {
                                            if e != *object_id {
                                                continue;
                                            }

                                            return !status;
                                        }

                                        true
                                    }),
                                ),
                            }
                        }

                        // FIXME the below is probably broken.
                        #[cfg(feature = "object-caching")]
                        {
                            let mut should_check_for_cacheable_objects = false;
                            let mv_status = &mv_status;
                            let predicate = |e| {
                                for (object_id, status) in mv_status {
                                    if e != *object_id {
                                        continue;
                                    }

                                    return !status;
                                }

                                true
                            };

                            match activation_to_execute.meta.kind {
                                NandoKind::ReadOnly => {
                                    should_check_for_cacheable_objects = true;
                                }
                                NandoKind::ReadWrite => {
                                    should_check_for_cacheable_objects = true;
                                    object_tracker.push_versions(
                                        &activation_to_execute.activation_log_entry.borrow(),
                                        Some(predicate),
                                    );
                                }
                                _ => object_tracker.push_versions(
                                    &activation_to_execute.activation_log_entry.borrow(),
                                    Some(predicate),
                                ),
                            }

                            if should_check_for_cacheable_objects {
                                let handle_state =
                                    activation_to_execute.get_handle_state().expect(&format!(
                                        "no handle state for {}",
                                        activation_to_execute.activation_intent.name
                                    ));
                                for arg in activation_to_execute.get_resolved_args().borrow().iter()
                                {
                                    let (is_cacheable, dependency_id, dependency_version) =
                                        match arg {
                                            ResolvedNandoArgument::Object(o) => match o {
                                                ObjectArgument::RWObject(o) => {
                                                    match o.object_is_cacheable() {
                                                        false => (false, 0, 0),
                                                        true => (true, o.id, o.get_version()),
                                                    }
                                                }
                                                ObjectArgument::ROObject(o) => match object_tracker
                                                    .read_object_is_cacheable(o.get_id())
                                                {
                                                    (false, _) => (false, 0, 0),
                                                    (true, Some(version)) => {
                                                        (true, o.get_id(), version)
                                                    }
                                                    _ => panic!("no version in cacheable result"),
                                                },
                                                _ => unreachable!(
                                                    "unresolved object arg in executor"
                                                ),
                                            },
                                            _ => continue,
                                        };

                                    if is_cacheable {
                                        let mut handle_state = loop {
                                            match handle_state.try_borrow_mut() {
                                                Ok(hs) => break hs,
                                                Err(_) => {}
                                            }
                                        };
                                        handle_state.append_cacheable_object(
                                            dependency_id,
                                            dependency_version,
                                        );
                                    }
                                }
                            }
                        }

                        #[cfg(feature = "telemetry")]
                        {
                            if let Some(task_id) = activation_ecb_id {
                                let execution_duration_ns = start.elapsed().as_nanos();
                                let telemetry_ts = telemetry::zoned_timestamp_now();
                                Self::submit_telemetry_event(
                                    &telemetry_handle,
                                    telemetry::TelemetryEvent::new_executed(
                                        task_id,
                                        execution_duration_ns,
                                        telemetry_ts,
                                    ),
                                );
                            }
                        }

                        #[cfg(feature = "timing-exec")]
                        {
                            let execution_duration = start.elapsed();
                            println!(
                                "[{:?}] done executing '{}', took {}s ({}ms, {}us)",
                                std::thread::current().name(),
                                activation_to_execute.activation_intent.name,
                                execution_duration.as_secs(),
                                execution_duration.as_millis(),
                                execution_duration.as_micros(),
                            );
                        }

                        #[cfg(feature = "timing")]
                        {
                            let timestamp = Instant::now();
                            activation_to_execute.set_timing_entity("execution_end", timestamp);
                            activation_to_execute
                                .set_timing_entity("exec_to_logger_queue_start", timestamp);
                        }

                        activation_to_execute.set_status_done();
                    }

                    log_queue_send
                        .send(activation_to_execute)
                        .expect("executor thread failed to push to txn log manager");
                }
                Err(PopError::Empty) => (),
            }
        }
    }

    pub(crate) fn get_input_channel_capacity(&self) -> usize {
        self.input_channel_capacity
    }
}
