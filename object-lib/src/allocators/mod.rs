//! A collection of object-aware allocators to be leveraged by upstream components that want to
//! define persistent data structures to be operated on through nanotransactions.
//!
//! These allocators should implement the [`std::alloc::Allocator`] trait to be compatible with the
//! standard library's allocator API so that, in the future, we could potentially leverage them in
//! standard library collections.
pub mod bump_allocator;
pub mod persistently_allocatable;
