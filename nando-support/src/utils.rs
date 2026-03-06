pub fn get_size_of_field<F, T, U>(_f: F) -> usize
where
    F: FnOnce(T) -> U,
{
    std::mem::size_of::<U>()
}
