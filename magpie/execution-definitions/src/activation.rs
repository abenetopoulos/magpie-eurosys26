use std::cell::RefCell;
use std::collections::HashSet;
use std::sync::Arc;
#[cfg(feature = "timing")]
use std::time::{Duration, Instant};

use atomic_refcell::AtomicRefCell;
use nando_support::{
    activation_id::ActivationId,
    ecb_id::EcbId,
    epic_control::{SpawnedTask, ECB},
    ObjectVersion,
};
use nando_support::{activation_intent, log_entry, nando_metadata, ObjectId, TxnId};
use object_lib::{
    tls::{IntoObjectMapping, ObjectMapping},
    IPtr, MaterializedObjectVersion, Object,
};
use object_tracker::ObjectTracker;
use ownership_tracker::OwnershipTracker;

use crate::txn_context::TxnContext;
use crate::{
    activation::activation_intent::{NandoArgument, NandoResult},
    nando_handle::{ActivationOutput, HandleCompletionState, SharedHandleState},
    nando_handle::{ExecutionError, TransactionStatus},
};

pub type ActivationFunction =
    dyn Fn(TxnContext, SharedHandleState, &Vec<ResolvedNandoArgument>) -> Result<(), ()>;

#[derive(Clone)]
pub enum ObjectResolutionMode {
    Na,
    Ro,
    Rw,
    // NOTE this is useful for correctly setting the MV-enabled flag for the given object in case
    // of first access on the current worker, since the executor executing the transaction will
    // have to notify the scheduler to disable mv.
    // All objects are assumed to be mv-enabled and the only way for mv to be disabled on the
    // object is for the executor to detect that the predicate to support mv (which currently
    // consists only of a check of the object's size) stopped being satisfied because of the
    // transaction it just executed. If the object in question was moved to the current worker
    // _and_ it already didn't satisfy the multiversioning predicate, then the object will be set
    // as mv-enabled on the scheduler's side and it will never be disabled. This mode allows us to
    // special-case the handling of these objects in the executor.
    RwFirstLocalAccess,
}

impl ObjectResolutionMode {
    pub fn is_first_local_access(&self) -> bool {
        match self {
            Self::RwFirstLocalAccess => true,
            _ => false,
        }
    }
}

#[derive(Clone)]
pub enum ObjectArgument {
    RWObject(Arc<Object>),
    ROObject(Arc<MaterializedObjectVersion>),
    UnresolvedObject((IPtr, ObjectResolutionMode)),
}

impl ObjectArgument {
    pub fn as_object_ref(&self) -> Option<&Object> {
        match self {
            Self::RWObject(o) => Some(o.as_ref()),
            _ => None,
        }
    }

    pub fn as_materialized_object_ref(&self) -> Option<&MaterializedObjectVersion> {
        match self {
            Self::ROObject(o) => Some(o.as_ref()),
            _ => None,
        }
    }

    pub fn get_iptr(&self) -> IPtr {
        match self {
            Self::RWObject(ref o) => IPtr::new(o.get_id(), 0, 0),
            Self::ROObject(ref o) => IPtr::new(o.get_id(), 0, 0),
            Self::UnresolvedObject((ref iptr, _)) => iptr.clone(),
        }
    }

    pub fn get_object_id(&self) -> ObjectId {
        match self {
            Self::RWObject(ref o) => o.get_id(),
            Self::ROObject(ref o) => o.get_id(),
            Self::UnresolvedObject((ref iptr, _)) => iptr.get_object_id(),
        }
    }

    pub fn set_rw_unresolved(&mut self, first_local_access: bool) {
        match self {
            Self::UnresolvedObject((_, ref mut mode)) => {
                *mode = match first_local_access {
                    true => ObjectResolutionMode::RwFirstLocalAccess,
                    false => ObjectResolutionMode::Rw,
                };
            }
            _ => (),
        }
    }

    pub fn set_ro_unresolved(&mut self) {
        match self {
            Self::UnresolvedObject((_, ref mut mode)) => *mode = ObjectResolutionMode::Ro,
            _ => (),
        }
    }

    pub fn offset_of(&self, field: *const ()) -> IPtr {
        match self {
            Self::RWObject(ref o) => o.offset_of(field),
            _ => unreachable!("requested offset_of for non-rw object"),
        }
    }

    pub fn get_version(&self) -> Option<ObjectVersion> {
        match self {
            Self::RWObject(ref o) => Some(o.get_version()),
            Self::ROObject(ref o) => Some(o.get_version()),
            _ => None,
        }
    }
}

impl std::fmt::Debug for ObjectArgument {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ObjectArgument::RWObject(o) => {
                f.write_fmt(format_args!("Object {{ {} }}", o.get_id(),))
            }
            ObjectArgument::ROObject(o) => {
                f.write_fmt(format_args!("RO Object {{ {} }}", o.get_id()))
            }
            ObjectArgument::UnresolvedObject((i, _)) => i.fmt(f),
        }
    }
}

impl Into<NandoResult> for &ObjectArgument {
    fn into(self) -> NandoResult {
        match self {
            ObjectArgument::RWObject(o) => NandoResult::Ref(o.iptr_of()),
            ObjectArgument::ROObject(o) => NandoResult::Ref(o.iptr_of()),
            ObjectArgument::UnresolvedObject((i, _)) => NandoResult::Ref(*i),
        }
    }
}

#[derive(Clone)]
pub enum ResolvedNandoArgument {
    Object(ObjectArgument),
    Objects(Vec<ObjectArgument>),
    Value(activation_intent::ScalarValue),
    ControlBlock(EcbId),
}

impl ResolvedNandoArgument {
    pub fn get_inner_object_argument(&self) -> Option<&ObjectArgument> {
        match self {
            Self::Object(ref oa) => Some(oa),
            _ => None,
        }
    }

    // FIXME should return some iterator instead.
    pub fn get_inner_object_arguments(&self) -> Vec<&ObjectArgument> {
        match self {
            Self::Object(ref oa) => vec![oa],
            Self::Objects(os) => os.iter().collect(),
            _ => Vec::default(),
        }
    }

    pub fn get_inner_object_argument_mut(&mut self) -> Option<&mut ObjectArgument> {
        match self {
            Self::Object(ref mut oa) => Some(oa),
            _ => None,
        }
    }
}

impl std::fmt::Debug for ResolvedNandoArgument {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Object(o) => o.fmt(f),
            Self::Objects(ref os) => {
                f.write_str("Refs {")
                    .expect("failed to fmt refs ResolvedNandoArgument");
                for o in os {
                    o.fmt(f).expect("failed to fmt object ref");
                    f.write_str(",").expect("failed to write list separator");
                }
                f.write_str("}")
            }
            Self::Value(v) => v.fmt(f),
            Self::ControlBlock(ecb_id) => {
                f.write_fmt(format_args!("Control Block {{ {} }}", ecb_id))
            }
        }
    }
}

impl Into<NandoResult> for &ResolvedNandoArgument {
    fn into(self) -> NandoResult {
        match self {
            ResolvedNandoArgument::Object(o) => o.into(),
            o @ ResolvedNandoArgument::Objects(_) => o.into(),
            ResolvedNandoArgument::Value(ref v) => v.into(),
            ResolvedNandoArgument::ControlBlock(_) => {
                panic!("cannot convert control block into result")
            }
        }
    }
}

#[cfg(feature = "timing")]
pub struct TimingInfo {
    transaction_start: Instant,
    transaction_end: Instant,

    // This captures the moment the activation was placed on a processor queue.
    scheduled_at: Instant,
    execution_start: Instant,
    execution_end: Instant,

    logging_start: Instant,
    logging_end: Instant,

    // queueing times
    tm_to_sched_queue_start: Instant,
    tm_to_sched_queue_end: Instant,
    sched_to_exec_queue_start: Instant,
    sched_to_exec_queue_end: Instant,
    exec_to_logger_queue_start: Instant,
    exec_to_logger_queue_end: Instant,
    logger_to_sched_queue_start: Instant,
    logger_to_sched_queue_end: Instant,
}

#[cfg(feature = "timing")]
impl TimingInfo {
    fn new() -> Self {
        let init = Instant::now();
        Self {
            transaction_start: init,
            transaction_end: init,
            scheduled_at: init,
            execution_start: init,
            execution_end: init,
            logging_start: init,
            logging_end: init,
            tm_to_sched_queue_start: init,
            tm_to_sched_queue_end: init,
            sched_to_exec_queue_start: init,
            sched_to_exec_queue_end: init,
            exec_to_logger_queue_start: init,
            exec_to_logger_queue_end: init,
            logger_to_sched_queue_start: init,
            logger_to_sched_queue_end: init,
        }
    }
}

pub enum EpicTaskInfo {
    NonEpic,
    TaskDependency(SpawnedTask),
    ControlBlock(Arc<RefCell<ECB>>),
}

impl EpicTaskInfo {
    fn is_epic(&self) -> bool {
        match self {
            Self::NonEpic => false,
            _ => true,
        }
    }

    fn is_spawned_task(&self) -> bool {
        match self {
            Self::TaskDependency(_) => true,
            _ => false,
        }
    }

    fn get_ecb_id(&self) -> Option<EcbId> {
        match self {
            Self::ControlBlock(ecb) => Some(ecb.borrow().id),
            _ => None,
        }
    }
}

pub enum ActivationEpicInformation {
    NotPartOfEpic,
    TopLevel,
    NonTopLevel,
}

#[allow(dead_code)]
pub struct NandoActivation {
    pub activation_id: ActivationId,
    pub activation_intent: activation_intent::NandoActivationIntent,
    pub activation_log_entry: Arc<RefCell<log_entry::TransactionLogEntry>>,

    // NOTE this is shared with the handle the local scheduler returns to the transaction manager
    pub handle_state: Option<SharedHandleState>,

    pub executor: usize,

    resolved_args: RefCell<Vec<ResolvedNandoArgument>>,
    target: Arc<ActivationFunction>,

    pub meta: nando_metadata::NandoMetadata,
    // pub is_top_level: bool,
    pub epic_information: ActivationEpicInformation,

    pub ecb: EpicTaskInfo,
    pub status: TransactionStatus,
    pub execution_error: Option<ExecutionError>,

    pub mv_updates: RefCell<Vec<(ObjectId, bool)>>,

    #[cfg(feature = "timing")]
    timing_info: RefCell<TimingInfo>,
}

unsafe impl Send for NandoActivation {}

impl NandoActivation {
    pub fn new(
        mut activation_intent: activation_intent::NandoActivationIntent,
        txn_id: TxnId,
        handle_state: Option<SharedHandleState>,
        nando_closure: Arc<ActivationFunction>,
        meta: nando_metadata::NandoMetadata,
    ) -> Self {
        let resolved_args: Vec<_> = activation_intent
            .args
            .drain(..)
            .map(|a| match a {
                NandoArgument::Value(v) => ResolvedNandoArgument::Value(v),
                NandoArgument::Ref(i) => ResolvedNandoArgument::Object(
                    ObjectArgument::UnresolvedObject((i, ObjectResolutionMode::Na)),
                ),
                NandoArgument::MRef(ref is) => {
                    let objects = is
                        .get_inner()
                        .iter()
                        .map(|i| ObjectArgument::UnresolvedObject((*i, ObjectResolutionMode::Na)))
                        .collect();

                    ResolvedNandoArgument::Objects(objects)
                }
                _ => unreachable!(),
            })
            .collect();
        let num_args = resolved_args.len();

        Self {
            activation_intent,
            activation_id: ActivationId::new_for_txn(txn_id),
            activation_log_entry: Arc::new(RefCell::new(log_entry::TransactionLogEntry::new(
                txn_id, None,
            ))),
            handle_state,
            target: nando_closure,
            resolved_args: RefCell::new(resolved_args),

            meta,
            executor: 0,

            epic_information: ActivationEpicInformation::NotPartOfEpic,
            ecb: EpicTaskInfo::NonEpic,

            status: TransactionStatus::Pending,
            execution_error: None,

            mv_updates: RefCell::new(Vec::with_capacity(num_args)),

            #[cfg(feature = "timing")]
            timing_info: RefCell::new(TimingInfo::new()),
        }
    }

    pub fn from_intent(
        mut intent: activation_intent::NandoActivationIntent,
        activation_id: ActivationId,
        nando_closure: Arc<ActivationFunction>,
        meta: nando_metadata::NandoMetadata,
    ) -> Self {
        let resolved_args: Vec<_> = intent
            .args
            .drain(..)
            .map(|a| match a {
                NandoArgument::Value(v) => ResolvedNandoArgument::Value(v),
                NandoArgument::Ref(i) => ResolvedNandoArgument::Object(
                    ObjectArgument::UnresolvedObject((i, ObjectResolutionMode::Na)),
                ),
                NandoArgument::MRef(ref is) => {
                    let objects = is
                        .get_inner()
                        .iter()
                        .map(|i| ObjectArgument::UnresolvedObject((*i, ObjectResolutionMode::Na)))
                        .collect();

                    ResolvedNandoArgument::Objects(objects)
                }
                _ => unreachable!(),
            })
            .collect();
        let num_args = resolved_args.len();

        Self {
            activation_id,
            activation_intent: intent,
            activation_log_entry: Arc::new(RefCell::new(log_entry::TransactionLogEntry::new(
                0, None,
            ))),
            handle_state: None,
            executor: 0,
            resolved_args: RefCell::new(resolved_args),
            target: nando_closure,
            meta,

            // is_top_level: false,
            epic_information: ActivationEpicInformation::NonTopLevel,
            ecb: EpicTaskInfo::NonEpic,
            status: TransactionStatus::Pending,
            execution_error: None,

            mv_updates: RefCell::new(Vec::with_capacity(num_args)),

            #[cfg(feature = "timing")]
            timing_info: RefCell::new(TimingInfo::new()),
        }
    }

    pub fn new_executor_stats_activation(
        handle_state: Option<SharedHandleState>,
        nando_closure: Arc<ActivationFunction>,
        meta: nando_metadata::NandoMetadata,
    ) -> Self {
        Self {
            activation_intent: activation_intent::NandoActivationIntent::new_executor_stats_intent(
            ),
            // ignored
            activation_id: ActivationId::default(),
            activation_log_entry: Arc::new(RefCell::new(log_entry::TransactionLogEntry::new(
                0, None,
            ))),
            handle_state,
            target: nando_closure,
            resolved_args: RefCell::new(Vec::default()),

            meta,
            executor: 0,

            epic_information: ActivationEpicInformation::NotPartOfEpic,
            ecb: EpicTaskInfo::NonEpic,

            status: TransactionStatus::Pending,
            execution_error: None,

            mv_updates: RefCell::new(Vec::default()),

            #[cfg(feature = "timing")]
            timing_info: RefCell::new(TimingInfo::new()),
        }
    }

    pub fn is_executor_stats_activation(&self) -> bool {
        self.activation_intent.is_executor_stats_intent()
    }

    pub fn set_top_level(&mut self) {
        self.epic_information = ActivationEpicInformation::TopLevel;
    }

    pub fn set_non_top_level(&mut self) {
        self.epic_information = ActivationEpicInformation::NonTopLevel;
    }

    pub fn handle_is_dummy(&self) -> bool {
        self.handle_state.is_none() || self.handle_state.as_ref().unwrap().borrow().is_dummy()
    }

    pub fn get_result(&self) -> ActivationOutput {
        self.handle_state.as_ref().unwrap().borrow().get_result()
    }

    pub fn get_object_references(&self) -> Vec<ObjectId> {
        self.resolved_args
            .borrow()
            .iter()
            .fold(HashSet::new(), |mut acc, arg| {
                match arg {
                    ResolvedNandoArgument::Object(o) => {
                        acc.insert(o.get_object_id());
                    }
                    ResolvedNandoArgument::Objects(ref os) => {
                        for o in os {
                            acc.insert(o.get_object_id());
                        }
                    }
                    _ => {}
                };

                acc
            })
            .into_iter()
            .collect()
    }

    pub fn mark_done_and_wake(&self) {
        if self.handle_state.is_none() {
            #[cfg(debug_assertions)]
            println!("no handle state for activation, cannot wake");
            return;
        }

        let completion_handle = self.handle_state.as_ref();
        let completion_state = completion_handle.unwrap().borrow();
        completion_state.mark_done_and_wake();
    }

    pub fn mark_triggered_and_wake(&self) {
        if self.handle_state.is_none() {
            return;
        }

        let completion_handle = self.handle_state.as_ref();
        let completion_state = completion_handle.unwrap().borrow();
        completion_state.mark_triggered_and_wake();
    }

    // FIXME add reason for failure
    pub fn mark_failed_and_wake(&self) {
        if self.handle_state.is_none() {
            return;
        }

        let completion_handle = self.handle_state.as_ref();
        let mut completion_state = completion_handle.unwrap().borrow_mut();
        completion_state.mark_failed_and_wake(
            self.execution_error
                .clone()
                .expect("no error set for failed activation"),
        );
    }

    pub fn set_handle_state(&mut self, handle_state: SharedHandleState) {
        if self.handle_state.is_some() {
            // FIXME no need to panic
            panic!("attempt to override activation handle state");
        }

        self.handle_state = Some(handle_state);
    }

    pub fn get_handle_state(&self) -> Option<SharedHandleState> {
        match &self.handle_state {
            None => None,
            Some(ref hs) => Some(Arc::clone(hs)),
        }
    }

    pub fn resolve_intent_arg(
        &mut self,
        idx: usize,
        resolved_arg: NandoArgument,
        resolving_task: Option<EcbId>,
    ) {
        self.activation_intent
            .resolve_intent_arg(idx, resolved_arg, resolving_task);
    }

    pub fn call(&self, object_tracker: Arc<ObjectTracker>) -> Result<(), ()> {
        // FIXME eliminate the need for this dummy handle state
        let handle_state = match self.handle_state {
            Some(ref hs) => Arc::clone(hs),
            None => Arc::new(AtomicRefCell::new(HandleCompletionState::new())),
        };

        // TODO panic and signal handling.
        let ownership_tracker = OwnershipTracker::get_ownership_tracker(None);
        let resolved_args = self.resolved_args.borrow();

        let mut ctx = TxnContext::new(
            Arc::clone(&self.activation_log_entry),
            Arc::clone(&object_tracker),
            ownership_tracker,
            resolved_args.len(),
        );
        ctx.set_ecb(self.get_ecb());

        (self.target)(ctx, Arc::clone(&handle_state), &resolved_args)
    }

    pub fn get_resolved_args(&self) -> &RefCell<Vec<ResolvedNandoArgument>> {
        &self.resolved_args
    }

    pub fn get_resolved_object_arg_mappings(&self) -> Vec<ObjectMapping> {
        self.resolved_args
            .borrow()
            .iter()
            .fold(vec![], |mut acc, ra| {
                if let Some(oa) = ra.get_inner_object_argument() {
                    match oa {
                        ObjectArgument::RWObject(o) => acc.push(o.into_mapping()),
                        ObjectArgument::ROObject(o) => acc.push(o.into_mapping()),
                        ObjectArgument::UnresolvedObject(_) => unreachable!(),
                    }
                }

                acc
            })
    }

    pub fn is_part_of_epic(&self) -> bool {
        if self.meta.spawns_nandos {
            return true;
        }

        match self.epic_information {
            ActivationEpicInformation::NotPartOfEpic => false,
            _ => true,
        }
    }

    pub fn set_ecb(&mut self, ecb: ECB) {
        if self.ecb.is_epic() && !self.ecb.is_spawned_task() {
            panic!(
                "attempt to overwrite ecb with id {:?}",
                self.ecb.get_ecb_id()
            );
        }
        self.ecb = EpicTaskInfo::ControlBlock(Arc::new(RefCell::new(ecb)));
    }

    pub fn get_ecb(&self) -> Option<Arc<RefCell<ECB>>> {
        match self.ecb {
            EpicTaskInfo::ControlBlock(ref e) => Some(Arc::clone(e)),
            _ => None,
        }
    }

    pub fn set_top_level_ecb(&mut self, host_idx: u64) {
        let ecb = {
            let mut ecb = ECB::new_top_level(host_idx, self.activation_id);
            match &self.ecb {
                EpicTaskInfo::TaskDependency(st) => {
                    // FIXME avoid clone
                    ecb.planning_context = st.planning_context.clone();
                }
                _ => unreachable!(),
            }
            ecb
        };

        self.set_ecb(ecb);
    }

    pub fn set_ecb_from_dependency_info(mut self) -> Self {
        self.ecb = match self.ecb {
            EpicTaskInfo::TaskDependency(task) => {
                EpicTaskInfo::ControlBlock(Arc::new(RefCell::new(task.into_ecb())))
            }
            _ => panic!("not allowed"),
        };

        self
    }

    #[cfg(feature = "object-caching")]
    pub fn should_mask_invalidations(&self) -> bool {
        match self.ecb {
            EpicTaskInfo::TaskDependency(ref task) => task.should_mask_invalidations(),
            EpicTaskInfo::ControlBlock(ref block) => {
                let block = block.borrow();
                block.should_mask_invalidations()
            }
            EpicTaskInfo::NonEpic => false,
        }
    }

    pub fn set_task_control_info(&mut self, task_control_info: SpawnedTask) {
        self.ecb = EpicTaskInfo::TaskDependency(task_control_info);
    }

    pub fn get_task_control_info(&self) -> Option<SpawnedTask> {
        match self.ecb {
            EpicTaskInfo::TaskDependency(ref spawned_task) => Some(spawned_task.clone()),
            _ => None,
        }
    }

    pub fn get_task_control_id(&self) -> Option<EcbId> {
        match self.ecb {
            EpicTaskInfo::TaskDependency(ref spawned_task) => Some(spawned_task.id),
            EpicTaskInfo::ControlBlock(ref ecb) => Some(ecb.borrow().id),
            _ => None,
        }
    }

    pub fn is_top_level(&self) -> bool {
        match self.epic_information {
            ActivationEpicInformation::TopLevel => true,
            _ => false,
        }
    }

    pub fn set_status_done(&mut self) {
        self.status = TransactionStatus::Done;
    }

    pub fn set_status_failed(&mut self, err: ExecutionError) {
        self.status = TransactionStatus::Failed;
        self.execution_error = Some(err);
    }

    pub fn is_pending(&self) -> bool {
        match self.status {
            TransactionStatus::Pending => true,
            _ => false,
        }
    }

    pub fn is_failed(&self) -> bool {
        match self.status {
            TransactionStatus::Failed => true,
            _ => false,
        }
    }

    pub fn is_done(&self) -> bool {
        self.status.is_done()
    }

    pub fn add_mv_update(&self, object_id: ObjectId, mv_enabled: bool) {
        let mut mv_updates = self.mv_updates.borrow_mut();
        mv_updates.push((object_id, mv_enabled));
    }

    pub fn get_object_arg_at_idx(&self, arg_idx: usize) -> Option<ObjectId> {
        let resolved_args = self.resolved_args.borrow();

        match resolved_args.get(arg_idx) {
            None => None,
            Some(arg) => {
                let object_args = arg.get_inner_object_arguments();
                if object_args.is_empty() {
                    return None;
                }

                match object_args.len() > 1 {
                    false => Some(object_args[0].get_object_id()),
                    // NOTE this might be surprising, but there should be a separate function to
                    // get some id(s) in case of a ref vec.
                    true => None,
                }
            }
        }
    }

    pub fn get_object_args_at_idx(&self, arg_idx: usize) -> Vec<ObjectId> {
        let resolved_args = self.resolved_args.borrow();

        match resolved_args.get(arg_idx) {
            None => panic!("no resolved arg for idx {arg_idx}"),
            Some(arg) => {
                let object_args = arg.get_inner_object_arguments();
                object_args.iter().map(|oa| oa.get_object_id()).collect()
            }
        }
    }

    pub fn get_args_as_intent_args(&self) -> Vec<NandoArgument> {
        let args = self.resolved_args.borrow();
        args.iter()
            .map(|a| match a {
                ResolvedNandoArgument::Value(v) => NandoArgument::Value(v.clone()),
                ResolvedNandoArgument::Object(ObjectArgument::UnresolvedObject((o, _))) => {
                    NandoArgument::Ref(*o)
                }
                ResolvedNandoArgument::Objects(os) => {
                    NandoArgument::MRef(activation_intent::IPtrList::from_vec(
                        os.iter().map(|o| o.get_iptr()).collect(),
                    ))
                }
                _ => panic!(),
            })
            .collect()
    }

    pub fn is_built_in(&self) -> bool {
        self.activation_intent.is_built_in()
    }

    #[cfg(feature = "timing")]
    pub fn set_timing_entity(&self, entity: &str, value: Instant) {
        let timing_info = &mut self.timing_info.borrow_mut();
        match entity {
            "transaction_start" => timing_info.transaction_start = value,
            "transaction_end" => timing_info.transaction_end = value,
            "scheduled_at" => timing_info.scheduled_at = value,
            "execution_start" => timing_info.execution_start = value,
            "execution_end" => timing_info.execution_end = value,
            "logging_start" => timing_info.logging_start = value,
            "logging_end" => timing_info.logging_end = value,
            "tm_to_sched_queue_start" => timing_info.tm_to_sched_queue_start = value,
            "tm_to_sched_queue_end" => timing_info.tm_to_sched_queue_end = value,
            "sched_to_exec_queue_start" => timing_info.sched_to_exec_queue_start = value,
            "sched_to_exec_queue_end" => timing_info.sched_to_exec_queue_end = value,
            "exec_to_logger_queue_start" => timing_info.exec_to_logger_queue_start = value,
            "exec_to_logger_queue_end" => timing_info.exec_to_logger_queue_end = value,
            "logger_to_sched_queue_start" => timing_info.logger_to_sched_queue_start = value,
            "logger_to_sched_queue_end" => timing_info.logger_to_sched_queue_end = value,
            _ => panic!("unrecognized timing field {}", entity),
        }
    }

    #[cfg(feature = "timing")]
    pub fn print_timing_stats(&self) {
        let mut timing_stats_string = String::new();

        let timing_info = &self.timing_info.borrow();
        let execution_duration = timing_info
            .execution_end
            .duration_since(timing_info.execution_start);
        timing_stats_string.push_str(&format!(
            "Execution: {}us ({}ns)",
            execution_duration.as_micros(),
            execution_duration.as_nanos()
        ));

        let logging_duration = timing_info
            .logging_end
            .duration_since(timing_info.logging_start);
        if logging_duration > Duration::default() {
            timing_stats_string.push_str(&format!(
                "\nLogging: {}us ({}ns)",
                logging_duration.as_micros(),
                logging_duration.as_nanos()
            ));
        }

        let transaction_duration = timing_info
            .transaction_end
            .duration_since(timing_info.transaction_start);
        if transaction_duration > Duration::default() {
            timing_stats_string.push_str(&format!(
                "\nE2E transaction latency: {}us ({}ns)",
                transaction_duration.as_micros(),
                transaction_duration.as_nanos()
            ));
        }

        let scheduling_latency = timing_info
            .scheduled_at
            .duration_since(timing_info.transaction_start);
        if scheduling_latency > Duration::default() {
            timing_stats_string.push_str(&format!(
                "\nScheduling latency: {}us ({}ns)",
                scheduling_latency.as_micros(),
                scheduling_latency.as_nanos()
            ));
        }

        let tm_to_sched_latency = timing_info
            .tm_to_sched_queue_end
            .duration_since(timing_info.tm_to_sched_queue_start);
        if tm_to_sched_latency > Duration::default() {
            timing_stats_string.push_str(&format!(
                "\nTM<->Scheduler queue latency: {}us ({}ns)",
                tm_to_sched_latency.as_micros(),
                tm_to_sched_latency.as_nanos()
            ));
        }

        let sched_to_exec_latency = timing_info
            .sched_to_exec_queue_end
            .duration_since(timing_info.sched_to_exec_queue_start);
        if sched_to_exec_latency > Duration::default() {
            timing_stats_string.push_str(&format!(
                "\nScheduler<->Executor queue latency: {}us ({}ns)",
                sched_to_exec_latency.as_micros(),
                sched_to_exec_latency.as_nanos()
            ));
        }

        let exec_to_logger_latency = timing_info
            .exec_to_logger_queue_end
            .duration_since(timing_info.exec_to_logger_queue_start);
        if exec_to_logger_latency > Duration::default() {
            timing_stats_string.push_str(&format!(
                "\nExecutor<->logger queue latency: {}us ({}ns)",
                exec_to_logger_latency.as_micros(),
                exec_to_logger_latency.as_nanos()
            ));
        }

        let logger_to_sched_latency = timing_info
            .logger_to_sched_queue_end
            .duration_since(timing_info.logger_to_sched_queue_start);
        if logger_to_sched_latency > Duration::default() {
            timing_stats_string.push_str(&format!(
                "\nLogger<->Scheduler queue latency: {}us ({}ns)",
                logger_to_sched_latency.as_micros(),
                logger_to_sched_latency.as_nanos()
            ));
        }

        println!("====\n{}\n====", timing_stats_string);
    }
}

impl std::fmt::Debug for NandoActivation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_fmt(format_args!(
            "NandoActivation {{
            id: {}, executor: {}, args: {:#?}, status: {:?}, error: {:?}, intent: {:#?}
        }}",
            self.activation_id,
            self.executor,
            self.resolved_args,
            self.status,
            self.execution_error,
            self.activation_intent
        ))
    }
}
