use crate::TxnId;

pub enum IntentKind {
    OwnershipChange,
    UserTransactionExecution,
}

pub enum IntentLogEntryStatus {
    // Used only for initialization
    Unknown,
    // set before sending top-level transaction to nando scheduler
    Start,
    // successful commit response from scheduler
    Success,
    // abort notification from scheduler, with reason for abort
    Aborted(String),
}

pub struct IntentLogEntry {
    kind: IntentKind,
    txn_id: TxnId,
    status: IntentLogEntryStatus,
    // TODO
    // timestamp: PersistableTimestamp,
}

impl IntentLogEntry {
    pub fn new(txn_id: TxnId, kind: IntentKind, status: IntentLogEntryStatus) -> Self {
        Self {
            txn_id,
            kind,
            status,
        }
    }
}
