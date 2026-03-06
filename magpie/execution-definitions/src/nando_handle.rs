use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::{
    atomic::{AtomicU8, Ordering},
    Arc,
};
use std::task::{Context, Poll, Waker};

use atomic_refcell::{AtomicRef, AtomicRefCell};
use nando_support::{
    activation_intent::{NandoResult, NandoResultSerializable, ScalarValue},
    ecb_id::EcbId,
    ObjectId, ObjectVersion,
};

// TODO different error type, this has been duplicated from nando_lib
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum ExecutionError {
    TransactionNotFound(String),
    UnresolvableObject(String),
    UnknownError(),
}

impl fmt::Display for ExecutionError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            ExecutionError::TransactionNotFound(name) => f.write_fmt(format_args!(
                "No nanotransaction with the name `{}` has been registered.",
                name,
            )),
            ExecutionError::UnresolvableObject(obj_id) => {
                f.write_fmt(format_args!("Could not resolve object {}", obj_id,))
            }
            _ => write!(f, "An unknown error occured"),
        }
    }
}

#[derive(Clone, Debug)]
pub enum ActivationOutput {
    Result(NandoResult),
    Epic(EcbId),
}

impl ActivationOutput {
    pub fn get_inner_result(&self) -> Option<NandoResult> {
        match self {
            ActivationOutput::Result(ref r) => Some(r.clone()),
            _ => None,
        }
    }
}

impl From<NandoResult> for ActivationOutput {
    fn from(value: NandoResult) -> Self {
        Self::Result(value)
    }
}

impl From<EcbId> for ActivationOutput {
    fn from(value: EcbId) -> Self {
        Self::Epic(value)
    }
}

impl From<&ActivationOutput> for NandoResultSerializable {
    fn from(value: &ActivationOutput) -> Self {
        match value {
            ActivationOutput::Result(ref r) => r.into(),
            ActivationOutput::Epic(ecb_id) => NandoResultSerializable::Epic(*ecb_id),
        }
    }
}

#[derive(PartialEq, Eq, Clone)]
pub enum HandleTransactionType {
    Nando,
    Epic(EcbId),
}

#[derive(Debug)]
pub enum TransactionStatus {
    Pending = 0,
    EpicTriggered = 1,
    Failed = 2,
    Done = 3,
}

impl TransactionStatus {
    pub fn is_pending(&self) -> bool {
        match self {
            Self::Pending => true,
            _ => false,
        }
    }

    pub fn is_failed(&self) -> bool {
        match self {
            Self::Failed => true,
            _ => false,
        }
    }

    pub fn is_done(&self) -> bool {
        match self {
            Self::Done => true,
            _ => false,
        }
    }
}

impl Into<u8> for TransactionStatus {
    fn into(self) -> u8 {
        match self {
            Self::Pending => 0,
            Self::EpicTriggered => 1,
            Self::Failed => 2,
            Self::Done => 3,
        }
    }
}

impl TryInto<TransactionStatus> for u8 {
    type Error = &'static str;

    fn try_into(self) -> Result<TransactionStatus, Self::Error> {
        let txn_status = match self {
            0 => TransactionStatus::Pending,
            1 => TransactionStatus::EpicTriggered,
            2 => TransactionStatus::Failed,
            3 => TransactionStatus::Done,
            _ => return Err("failed to convert u8 to TransactionStatus, value out of range"),
        };

        Ok(txn_status)
    }
}

pub struct HandleCompletionState {
    waker: AtomicRefCell<Option<Waker>>,
    spinlock: AtomicU8,
    txn_status: AtomicU8,
    execution_error: Option<ExecutionError>,
    output: AtomicRefCell<Vec<ActivationOutput>>,
    cacheable_dependencies: Vec<(ObjectId, ObjectVersion)>,
}

impl HandleCompletionState {
    pub fn new() -> Self {
        Self {
            waker: AtomicRefCell::new(None),
            spinlock: AtomicU8::new(0),
            txn_status: AtomicU8::new(TransactionStatus::Pending.into()),
            execution_error: None,
            output: AtomicRefCell::new(vec![]),
            cacheable_dependencies: vec![],
        }
    }

    pub fn wake(&self) {
        let Ok(waker) = self.waker.try_borrow() else {
            return;
        };

        match waker.as_ref() {
            Some(w) => w.wake_by_ref(),
            // NOTE If no waker has been assigned, then whoever polls the associated future will
            // just read the done status, so this is fine.
            None => (),
        }
    }

    pub fn mark_triggered_and_wake(&self) {
        while self
            .spinlock
            .compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {}

        self.txn_status
            .store(TransactionStatus::EpicTriggered.into(), Ordering::Relaxed);
        self.wake();

        self.spinlock.store(0, Ordering::Release);
    }

    pub fn mark_done_and_wake(&self) {
        while self
            .spinlock
            .compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {}

        self.txn_status
            .store(TransactionStatus::Done.into(), Ordering::Relaxed);
        self.wake();

        self.spinlock.store(0, Ordering::Release);
    }

    pub fn mark_failed_and_wake(&mut self, execution_error: ExecutionError) {
        while self
            .spinlock
            .compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {}

        self.execution_error = Some(execution_error);
        self.txn_status
            .store(TransactionStatus::Failed.into(), Ordering::Relaxed);
        self.wake();

        self.spinlock.store(0, Ordering::Release);
    }

    pub fn append_result(&self, result: NandoResult) {
        match self.output.try_borrow_mut() {
            Ok(mut output) => {
                output.push(result.into());
            }
            Err(e) => {
                panic!("Failed to get output mutable reference in handle: {}", e);
            }
        }
    }

    pub fn append_spawn(&self, ecb_id: EcbId) {
        let mut output = self.output.borrow_mut();
        output.push(ecb_id.into());
    }

    pub fn append_cacheable_object(&mut self, object_id: ObjectId, version: ObjectVersion) {
        self.cacheable_dependencies.push((object_id, version));
    }

    pub fn get_result(&self) -> ActivationOutput {
        match self.output.borrow().last() {
            None => ActivationOutput::Result(NandoResult::Value(ScalarValue::Nil)),
            Some(e) => e.clone(),
        }
    }

    pub fn get_output(&self) -> AtomicRef<'_, Vec<ActivationOutput>> {
        self.output.borrow()
    }

    pub fn is_dummy(&self) -> bool {
        self.waker.borrow().is_none()
    }
}

pub type SharedHandleState = Arc<AtomicRefCell<HandleCompletionState>>;

#[derive(Clone)]
pub struct NandoHandle {
    txn_kind: HandleTransactionType,
    completion_state: SharedHandleState,
}

impl NandoHandle {
    pub fn new_nando_handle() -> Self {
        Self {
            txn_kind: HandleTransactionType::Nando,
            completion_state: Arc::new(AtomicRefCell::new(HandleCompletionState::new())),
        }
    }

    pub fn new_epic_top_level_handle(root_ecb_id: EcbId) -> Self {
        Self {
            txn_kind: HandleTransactionType::Epic(root_ecb_id),
            completion_state: Arc::new(AtomicRefCell::new(HandleCompletionState::new())),
        }
    }

    pub fn set_completion_state(
        &mut self,
        completion_state: Arc<AtomicRefCell<HandleCompletionState>>,
    ) {
        self.completion_state = completion_state;
    }

    pub fn get_completion_state(&self) -> Arc<AtomicRefCell<HandleCompletionState>> {
        Arc::clone(&self.completion_state)
    }

    pub fn get_ecb_id(&self) -> Option<EcbId> {
        match self.txn_kind {
            HandleTransactionType::Nando => None,
            HandleTransactionType::Epic(ecb_id) => Some(ecb_id),
        }
    }
}

pub enum NandoHandleOutputType {
    // Processing(EcbId),
    Processing(EcbId, Vec<(ObjectId, ObjectVersion)>),
    Result(Vec<ActivationOutput>, Vec<(ObjectId, ObjectVersion)>),
}

impl Future for NandoHandle {
    type Output = Result<NandoHandleOutputType, ExecutionError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let completion_state = loop {
            match self.completion_state.try_borrow() {
                Err(_) => {}
                Ok(txn_state) => break txn_state,
            }
        };
        let mut txn_status = {
            while completion_state.spinlock.load(Ordering::Acquire) != 0 {}
            completion_state
                .txn_status
                .load(Ordering::Relaxed)
                .try_into()
                .unwrap()
        };

        loop {
            match txn_status {
                TransactionStatus::Pending => {
                    while completion_state
                        .spinlock
                        .compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed)
                        .is_err()
                    {}
                    txn_status = completion_state
                        .txn_status
                        .load(Ordering::Relaxed)
                        .try_into()
                        .unwrap();

                    if !txn_status.is_pending() {
                        completion_state.spinlock.store(0, Ordering::Release);
                        continue;
                    }

                    match completion_state.waker.try_borrow_mut() {
                        Ok(mut waker) => {
                            *waker = Some(cx.waker().clone());
                        }
                        Err(_) => {}
                    }
                    completion_state.spinlock.store(0, Ordering::Release);

                    return Poll::Pending;
                }
                TransactionStatus::Failed => {
                    let Some(ref error) = self.completion_state.borrow().execution_error else {
                        panic!("no error found for failed transaction");
                    };
                    return Poll::Ready(Err(error.clone()));
                }
                TransactionStatus::EpicTriggered => {
                    let ecb_id = match self.txn_kind {
                        HandleTransactionType::Nando => panic!("should not happen"),
                        HandleTransactionType::Epic(ecb_id) => ecb_id,
                    };

                    return Poll::Ready(Ok(NandoHandleOutputType::Processing(
                        ecb_id,
                        self.completion_state
                            .borrow()
                            .cacheable_dependencies
                            .clone(),
                    )));
                }
                TransactionStatus::Done => {
                    let (output, cacheable_dependencies) = {
                        let completion_state = self.completion_state.borrow();
                        let output = completion_state.output.borrow();
                        (
                            output.clone(),
                            completion_state.cacheable_dependencies.clone(),
                        )
                    };

                    return Poll::Ready(Ok(NandoHandleOutputType::Result(
                        output,
                        cacheable_dependencies,
                    )));
                }
            }
        }
    }
}
