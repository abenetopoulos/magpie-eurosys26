//! A minimal wrapper around persistable dynamic arrays to provide fixed-capacity semantics.
use std::ops::{Index, IndexMut};
use std::sync::Arc;

use parking_lot::RwLock;

use crate::allocators::{
    bump_allocator::BumpAllocator, persistently_allocatable::PersistentlyAllocatable,
};
use crate::{
    collections::pvec::{PVec, PVecIterMut},
    Persistable,
};

pub enum OperationStatus {
    Ok,
    CapacityReached,
}

#[repr(C)]
pub struct FixedPVec<T> {
    size: usize,
    buf: PVec<T>,
}

impl<T> Persistable for FixedPVec<T> where T: Persistable {}

impl<T> FixedPVec<T>
where
    T: Persistable,
{
    pub fn new(size: usize) -> Self {
        Self {
            size,
            buf: PVec::new(),
        }
    }

    pub fn allocate(&mut self, allocator: Arc<RwLock<BumpAllocator>>) {
        self.set_allocator(allocator);
        self.buf.resize_to_capacity(self.size);
    }

    pub fn push(&mut self, e: T) -> OperationStatus {
        if self.buf.len() == self.size {
            return OperationStatus::CapacityReached;
        }

        self.buf.push(e);

        OperationStatus::Ok
    }

    pub fn pop(&mut self) -> Option<T> {
        self.buf.pop()
    }

    pub fn capacity(&self) -> usize {
        self.size
    }

    pub fn iter_mut(&mut self) -> PVecIterMut<T> {
        self.buf.iter_mut()
    }

    pub fn copy_from_vec(&mut self, other: &Vec<T>) -> bool {
        unsafe { self.buf.copy_from_vec(other) }
    }
}

impl<T> PersistentlyAllocatable for FixedPVec<T>
where
    T: Persistable,
{
    fn set_allocator(&mut self, allocator: Arc<RwLock<BumpAllocator>>) {
        self.buf.set_allocator(allocator);
    }

    fn get_allocator(&self) -> Option<Arc<RwLock<BumpAllocator>>> {
        self.buf.get_allocator()
    }
}

impl<T> Index<usize> for FixedPVec<T>
where
    T: Persistable,
{
    type Output = T;

    fn index(&self, index: usize) -> &Self::Output {
        self.buf.index(index)
    }
}

impl<T> IndexMut<usize> for FixedPVec<T>
where
    T: Persistable,
{
    fn index_mut(&mut self, index: usize) -> &mut Self::Output {
        self.buf.index_mut(index)
    }
}
