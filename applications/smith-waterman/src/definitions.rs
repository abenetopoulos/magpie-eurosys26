use std::sync::Arc;

use nandoize::PersistableDeriveLib;
use object_lib::{
    allocators::{
        bump_allocator::BumpAllocator, persistently_allocatable::PersistentlyAllocatable,
    },
    collections::pvec::PVec,
    pstring::PString,
    Persistable,
};

use parking_lot::RwLock;

pub enum Mode {
    Top,
    Left,
    TopLeft,
    Regular,
}

#[repr(C)]
#[derive(PersistableDeriveLib)]
pub struct State {
    pub horizontal_string: PString,
    pub vertical_string: PString,

    pub chunk_size: usize,

    pub match_score: i32,
    pub mismatch_score: i32,
    pub insertion_score: i32,
    pub deletion_score: i32,

    pub bottom_right_value: i32,
}

impl PersistentlyAllocatable for State {
    fn set_allocator(&mut self, allocator: Arc<RwLock<BumpAllocator>>) {
        self.horizontal_string.set_allocator(Arc::clone(&allocator));
        self.vertical_string.set_allocator(allocator);
    }

    fn get_allocator(&self) -> Option<Arc<RwLock<BumpAllocator>>> {
        self.horizontal_string.get_allocator()
    }
}

#[repr(C)]
#[derive(PersistableDeriveLib)]
pub struct BlockResult {
    pub right_halo: PVec<i32>,
    pub bottom_halo: PVec<i32>,

    pub bottom_right: i32,
}

impl PersistentlyAllocatable for BlockResult {
    fn set_allocator(&mut self, allocator: Arc<RwLock<BumpAllocator>>) {
        self.right_halo.set_allocator(Arc::clone(&allocator));
        self.bottom_halo.set_allocator(allocator);
    }

    fn get_allocator(&self) -> Option<Arc<RwLock<BumpAllocator>>> {
        self.right_halo.get_allocator()
    }
}
