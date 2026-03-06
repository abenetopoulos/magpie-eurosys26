use std::cell::RefCell;
use std::sync::Arc;

use nando_support::ObjectId;

use crate::OwnershipTracker;

thread_local! {
    // NOTE initial value does not matter, we run `set()` as part of the executor's construction
    static OWNERSHIP_TRACKER: RefCell<Arc<&'static OwnershipTracker>> = panic!("!");
}

pub fn set_thread_local_ownership_tracker(ownership_tracker: &'static OwnershipTracker) {
    OWNERSHIP_TRACKER.set(Arc::new(ownership_tracker));
}

pub fn get_local_ownership_tracker_instance() -> Arc<&'static OwnershipTracker> {
    OWNERSHIP_TRACKER.with_borrow(|ot| Arc::clone(&ot))
}

pub fn mark_object_owned(object_id: ObjectId) {
    OWNERSHIP_TRACKER.with_borrow(|ot| ot.mark_owned(object_id));
}
