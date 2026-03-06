#![feature(
    allocator_api,
    alloc_layout_extra,
    offset_of,
    pointer_is_aligned,
    portable_simd,
    slice_from_ptr_range,
    type_name_of_val
)]

pub use materialized_object::MaterializedObjectVersion;
pub use nando_support::iptr::IPtr;
pub use nando_support::{ObjectId, ObjectVersion};

pub use object::Object;
pub use persistable::Persistable;

pub mod allocators;
pub mod collections;
pub mod error;
pub mod files;
pub mod materialized_object;
pub mod object;
pub mod persistable;
pub mod pstring;
#[macro_use]
pub mod utils;
pub mod tls;
