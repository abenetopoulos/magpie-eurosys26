use std::cell::RefCell;
use std::sync::Arc;

use nando_support::{activation_intent::NandoResult, epic_control::ECB, log_entry};
use object_tracker::ObjectTracker;
use ownership_tracker::OwnershipTracker;

pub struct TxnContext {
    namespace: String,
    log_entry: Arc<RefCell<log_entry::TransactionLogEntry>>,
    ecb: Option<Arc<RefCell<ECB>>>,
    object_tracker: Arc<ObjectTracker>,
    ownership_tracker: Arc<&'static OwnershipTracker>,
}

impl TxnContext {
    pub fn new(
        log_entry: Arc<RefCell<log_entry::TransactionLogEntry>>,
        object_tracker: Arc<ObjectTracker>,
        ownership_tracker: &'static OwnershipTracker,
        _num_args: usize,
    ) -> Self {
        Self {
            namespace: String::default(),
            log_entry: Arc::clone(&log_entry),
            ecb: None,
            object_tracker: Arc::clone(&object_tracker),
            ownership_tracker: Arc::new(ownership_tracker),
        }
    }

    pub fn get_ecb(&self) -> Option<Arc<RefCell<ECB>>> {
        match self.ecb {
            None => None,
            Some(ref e) => Some(Arc::clone(e)),
        }
    }

    pub fn set_ecb(&mut self, ecb: Option<Arc<RefCell<ECB>>>) {
        self.ecb = ecb;
    }

    pub fn set_namespace(&mut self, namespace: &str) {
        self.namespace = namespace.to_string();
    }

    pub fn get_namespace(&self) -> &str {
        &self.namespace
    }

    pub fn get_log_entry(&self) -> Arc<RefCell<log_entry::TransactionLogEntry>> {
        Arc::clone(&self.log_entry)
    }

    pub fn get_object_tracker(&self) -> Arc<ObjectTracker> {
        Arc::clone(&self.object_tracker)
    }

    pub fn get_ownership_tracker(&self) -> &'static OwnershipTracker {
        &self.ownership_tracker
    }

    pub fn is_part_of_epic(&self) -> bool {
        self.ecb.is_some()
    }

    pub fn set_ecb_result(&self, result: NandoResult) {
        let ecb = self.ecb.as_ref().unwrap();
        ecb.borrow_mut().set_result(result);
    }
}
