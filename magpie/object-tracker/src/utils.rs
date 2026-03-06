#[macro_export]
macro_rules! unit_ptr_of {
    ($field:expr) => {
        $field as *const _ as *const ()
    };
}
