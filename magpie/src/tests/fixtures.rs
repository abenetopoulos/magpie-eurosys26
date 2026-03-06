use std::fs;
use std::path::Path;
use std::sync::Arc;

use object_lib;
use rstest::fixture;

use crate::ObjectTracker;

#[fixture]
pub fn object_tracker() -> Arc<ObjectTracker> {
    object_lib::files::clear_allocation_dir();
    object_lib::files::set_up_allocation_dir().expect("failed to set up allocation dir for test");

    match ObjectTracker::new(0) {
        None => panic!("failed to instantiate test object tracker"),
        Some(ot) => Arc::new(ot),
    }
}
