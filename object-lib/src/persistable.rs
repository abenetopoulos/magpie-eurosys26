use std::mem;
use std::slice;

use nando_support::iptr;

pub trait Persistable {
    fn as_bytes(&self) -> &[u8]
    where
        Self: Sized,
    {
        // FIXME shallow copy
        unsafe { slice::from_raw_parts((self as *const Self) as *const u8, mem::size_of::<Self>()) }
    }

    fn from_bytes(src: *mut [u8]) -> *mut Self
    where
        Self: Sized,
    {
        let src_p = src as *mut Self;
        src_p
    }

    fn from_bytes_ref(src: *const [u8]) -> &'static Self
    where
        Self: Sized,
    {
        let src_p = src as *mut Self;
        unsafe { &*src_p }
    }

    /// Used for object moves.
    fn adjust_from(&mut self, _other: &Self) {}
}

macro_rules! simple_impl {
    ($t:ty) => {
        impl Persistable for $t {}
    };
}

simple_impl!(u8);
simple_impl!(u16);
simple_impl!(u32);
simple_impl!(u64);
simple_impl!(u128);
simple_impl!(usize);

simple_impl!(i8);
simple_impl!(i16);
simple_impl!(i32);
simple_impl!(i64);
simple_impl!(i128);
simple_impl!(isize);

simple_impl!(f32);
simple_impl!(f64);

simple_impl!(bool);
simple_impl!(char);

simple_impl!(iptr::IPtr);
impl<T> Persistable for iptr::TypedIPtr<T> where T: Persistable {}

impl<T> Persistable for (T, T) where T: Persistable {}
