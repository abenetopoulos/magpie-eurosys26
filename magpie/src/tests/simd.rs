use std::mem::size_of as mem_size_of;
use std::sync::Arc;

use nando_support::allocate_and_init;
use object_lib::pstring::PString;
use rand::{self, Rng};
use rstest::*;

use super::fixtures::*;
use crate::ObjectTracker;

#[rstest]
fn non_simd_test(object_tracker: Arc<ObjectTracker>) {
    let string_object = allocate_and_init!(object_tracker, PString);
    let string_data = string_object.read_into_mut::<PString>(None).unwrap();

    let test_string = "string";
    string_data.from(&test_string);

    assert!(string_data == &test_string);
}

#[rstest]
#[case(8)]
#[case(20)]
#[case(100)]
#[case(1000)]
#[case(10000)]
fn simd_test(object_tracker: Arc<ObjectTracker>, #[case] str_len: usize) {
    for _ in 1..100 {
        let string_object = allocate_and_init!(object_tracker, PString);
        let string_data = string_object.read_into_mut::<PString>(None).unwrap();

        let test_string: String = rand::thread_rng()
            .sample_iter(&rand::distributions::Alphanumeric)
            .take(str_len)
            .map(char::from)
            .collect();
        string_data.resize_to_capacity(test_string.len());
        string_data.from(&test_string);

        assert!(string_data == &test_string);

        object_tracker.delete_object(string_object.id);
    }
}
