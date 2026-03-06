use std::sync::{
    atomic::{AtomicU8, Ordering},
    Arc,
};

use nando_support::ecb_id::EcbId;
use parking_lot::RwLock;
use tokio::sync::Notify;

#[derive(Copy, Clone, Debug)]
pub enum EpicStatus {
    Triggered = 0,
    Completed = 1,
    Failed = 2,
}

impl EpicStatus {
    pub fn is_failed(&self) -> bool {
        match self {
            Self::Failed => true,
            _ => false,
        }
    }
}

impl Into<u8> for EpicStatus {
    fn into(self) -> u8 {
        match self {
            Self::Triggered => 0,
            Self::Completed => 1,
            Self::Failed => 2,
        }
    }
}

impl TryInto<EpicStatus> for u8 {
    type Error = &'static str;

    fn try_into(self) -> Result<EpicStatus, Self::Error> {
        let epic_status = match self {
            0 => EpicStatus::Triggered,
            1 => EpicStatus::Completed,
            2 => EpicStatus::Failed,
            _ => return Err("failed to convert u8 to EpicStatus, value out of range"),
        };

        Ok(epic_status)
    }
}

#[derive(Clone)]
pub struct EpicCompletionHandle {
    pub root_ecb_id: EcbId,
    epic_status: Arc<AtomicU8>,
    pub completion_timestamp: Arc<RwLock<Option<std::time::Instant>>>,

    epic_done_notify: Arc<Notify>,
}

impl EpicCompletionHandle {
    pub fn new_triggered(root_ecb_id: EcbId) -> Self {
        Self {
            root_ecb_id,
            epic_status: Arc::new(AtomicU8::new(EpicStatus::Triggered.into())),
            completion_timestamp: Arc::new(RwLock::new(None)),
            epic_done_notify: Arc::new(Notify::new()),
        }
    }

    pub fn new_completed(root_ecb_id: EcbId) -> Self {
        Self {
            root_ecb_id,
            epic_status: Arc::new(AtomicU8::new(EpicStatus::Completed.into())),
            completion_timestamp: Arc::new(RwLock::new(None)),
            epic_done_notify: Arc::new(Notify::new()),
        }
    }

    pub async fn await_completion(&self) {
        let status = self.epic_status.load(Ordering::Relaxed).try_into().unwrap();
        match status {
            EpicStatus::Triggered => self.epic_done_notify.notified().await,
            _ => {}
        }
    }

    pub fn mark_completed_and_notify(&self, completion_timestamp: Option<std::time::Instant>) {
        match completion_timestamp {
            None => {}
            Some(ct) => {
                let mut ts = self.completion_timestamp.write();
                *ts = Some(ct);
            }
        }

        self.mark_completed();
        self.notify_all();
    }

    pub fn mark_failed_and_notify(&self, completion_timestamp: Option<std::time::Instant>) {
        match completion_timestamp {
            None => {}
            Some(ct) => {
                let mut ts = self.completion_timestamp.write();
                *ts = Some(ct);
            }
        }

        self.mark_failed();
        self.notify_all();
    }

    pub fn mark_completed(&self) {
        self.epic_status
            .store(EpicStatus::Completed.into(), Ordering::Relaxed);
    }

    pub fn mark_failed(&self) {
        self.epic_status
            .store(EpicStatus::Failed.into(), Ordering::Relaxed);
    }

    pub fn notify_all(&self) {
        self.epic_done_notify.notify_waiters();
        // NOTE so the below is pretty important because, at the time of writing this, the
        // semantics of `notify_waiters()` is to notify all _already registered_ tasks waiting on
        // the `Notify` (https://docs.rs/tokio/1.37.0/tokio/sync/struct.Notify.html#method.notify_waiters),
        // which means that in the case where a task races with the completion method for a given epic,
        // it will sometimes miss the notification.
        // If that behavior changes in the future we can reimplement this.
        self.epic_done_notify.notify_one();
    }

    pub async fn get_status(&self) -> EpicStatus {
        self.epic_status.load(Ordering::Relaxed).try_into().unwrap()
    }
}
