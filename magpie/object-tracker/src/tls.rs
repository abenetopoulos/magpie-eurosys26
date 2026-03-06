// NOTE given that this is expected to be called from application/library code, it should be moved
// outside of the main `magpie` workspace.

use std::cell::RefCell;
use std::sync::Arc;

use crate::ObjectTracker;

thread_local! {
    // NOTE initial value does not matter, we run `set()` as part of the executor's construction
    static OBJECT_TRACKER: RefCell<Arc<ObjectTracker>> = panic!("!");
}

pub fn set_thread_local_object_tracker(object_tracker: Arc<ObjectTracker>) {
    OBJECT_TRACKER.set(Arc::clone(&object_tracker));
}

pub fn get_local_object_tracker_instance() -> Arc<ObjectTracker> {
    OBJECT_TRACKER.with_borrow(|ot| Arc::clone(&ot))
}
