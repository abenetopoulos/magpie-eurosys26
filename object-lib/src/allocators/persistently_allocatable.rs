use std::mem;
use std::slice;
use std::sync::Arc;

use parking_lot::RwLock;

use crate::allocators::bump_allocator::BumpAllocator;
use crate::tls as nando_tls;

pub trait PersistentlyAllocatable {
    fn set_allocator(&mut self, _allocator: Arc<RwLock<BumpAllocator>>) {}
    fn get_allocator(&self) -> Option<Arc<RwLock<BumpAllocator>>> {
        None
    }
}

macro_rules! simple_pa_impl {
    ($t:ty) => {
        impl PersistentlyAllocatable for $t {}
    };
}

simple_pa_impl!(u8);
simple_pa_impl!(u16);
simple_pa_impl!(u32);
simple_pa_impl!(u64);
simple_pa_impl!(u128);
simple_pa_impl!(usize);

simple_pa_impl!(i8);
simple_pa_impl!(i16);
simple_pa_impl!(i32);
simple_pa_impl!(i64);
simple_pa_impl!(i128);
simple_pa_impl!(isize);

simple_pa_impl!(char);

pub trait CopyFrom {
    type From;

    fn from(&mut self, source: Self::From);
}

macro_rules! simple_cf_impl {
    ($from:ty) => {
        impl CopyFrom for $from {
            type From = $from;

            fn from(&mut self, source: Self::From) {
                let ptr = std::ptr::addr_of!(*self);
                let self_bytes = unsafe {
                    slice::from_raw_parts(
                        (self as *const Self) as *const u8,
                        mem::size_of::<Self>(),
                    )
                };
                nando_tls::add_new_pre_image(ptr as *const (), self_bytes);

                *self = source;

                let self_bytes = unsafe {
                    slice::from_raw_parts(
                        (self as *const Self) as *const u8,
                        mem::size_of::<Self>(),
                    )
                };
                nando_tls::add_new_post_image_if_changed(ptr as *const (), self_bytes);
            }
        }
    };
}

simple_cf_impl!(u32);
simple_cf_impl!(u128);
simple_cf_impl!(usize);
