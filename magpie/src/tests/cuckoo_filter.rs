use std::mem::size_of as mem_size_of;
use std::sync::Arc;

use nando_support::allocate_and_init;
use object_lib::collections::pcuckoo::{self, PCuckooFilter};
use rstest::*;

use super::fixtures::*;
use crate::ObjectTracker;

#[rstest]
fn simple_init(object_tracker: Arc<ObjectTracker>) {
    let filter_object = allocate_and_init!(object_tracker, PCuckooFilter);
    let filter_object_data = filter_object.read_into_mut::<PCuckooFilter>(None).unwrap();
    *filter_object_data = PCuckooFilter::new(128, 5);
    filter_object.set_inner_allocator(filter_object_data);

    filter_object_data.allocate_table();
}

#[rstest]
fn empty_not_contains(object_tracker: Arc<ObjectTracker>) {
    let filter_object = allocate_and_init!(object_tracker, PCuckooFilter);
    let filter_object_data = filter_object.read_into_mut::<PCuckooFilter>(None).unwrap();
    *filter_object_data = PCuckooFilter::new(128, 5);
    filter_object.set_inner_allocator(filter_object_data);

    filter_object_data.allocate_table();

    assert_eq!(filter_object_data.contains(42), false);
}

#[rstest]
fn simple_membership_check(object_tracker: Arc<ObjectTracker>) {
    let filter_object = allocate_and_init!(object_tracker, PCuckooFilter);
    let filter_object_data = filter_object.read_into_mut::<PCuckooFilter>(None).unwrap();
    *filter_object_data = PCuckooFilter::new(128, 5);
    filter_object.set_inner_allocator(filter_object_data);

    filter_object_data.allocate_table();

    assert_eq!(filter_object_data.add(42), pcuckoo::Status::Ok);
    assert_eq!(filter_object_data.contains(42), true);
}

#[rstest]
fn simple_deletion(object_tracker: Arc<ObjectTracker>) {
    let filter_object = allocate_and_init!(object_tracker, PCuckooFilter);
    let filter_object_data = filter_object.read_into_mut::<PCuckooFilter>(None).unwrap();
    *filter_object_data = PCuckooFilter::new(128, 5);
    filter_object.set_inner_allocator(filter_object_data);

    filter_object_data.allocate_table();

    assert_eq!(filter_object_data.contains(42), false);
    assert_eq!(filter_object_data.add(42), pcuckoo::Status::Ok);
    assert_eq!(filter_object_data.contains(42), true);

    assert_eq!(filter_object_data.delete(42), pcuckoo::Status::Ok);
    assert_eq!(filter_object_data.contains(42), false);
}
