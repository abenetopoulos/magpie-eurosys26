use std::sync::Arc;

use nando_support::iptr::TypedIPtr;
use nandoize::PersistableDeriveLib;
use object_lib::{
    allocators::{
        bump_allocator::BumpAllocator, persistently_allocatable::PersistentlyAllocatable,
    },
    collections::{pmap::PHashMap, pvec::PVec},
    pstring::PString,
    ObjectId, Persistable,
};

use parking_lot::RwLock;

// The "root block" here is just an index of backing bucket object ids.
#[repr(C)]
#[derive(PersistableDeriveLib)]
pub struct RootBlock {
    pub(crate) bucket_object_ids: PVec<ObjectId>,
}

// Because our data type contains types that allocate storage within the object they reside in, we
// need to implement the below trait that initializes the inner fields' allocator. This
// initialization happens every time an object is "opened" on a host, so the first time it is
// loaded from disk either after a restart or after a move.
impl PersistentlyAllocatable for RootBlock {
    fn set_allocator(&mut self, allocator: Arc<RwLock<BumpAllocator>>) {
        self.bucket_object_ids.set_allocator(allocator);
    }

    fn get_allocator(&self) -> Option<Arc<RwLock<BumpAllocator>>> {
        self.bucket_object_ids.get_allocator()
    }
}

// The buckets that actually store the KVS' data -- string keys for now, but user-specified values.
#[repr(C)]
#[derive(PersistableDeriveLib)]
pub struct StorageBucket<V>
where
    V: Persistable,
{
    pub inner: PHashMap<PString, V>,
}

impl<V> StorageBucket<V>
where
    V: Persistable,
{
    pub fn new() -> Self {
        Self {
            inner: PHashMap::new(),
        }
    }
}

// Similar to `RootBlock`
impl<V> PersistentlyAllocatable for StorageBucket<V>
where
    V: Persistable,
{
    fn set_allocator(&mut self, allocator: Arc<RwLock<BumpAllocator>>) {
        self.inner.set_allocator(allocator);
    }

    fn get_allocator(&self) -> Option<Arc<RwLock<BumpAllocator>>> {
        self.inner.get_allocator()
    }
}

#[repr(C)]
#[derive(PersistableDeriveLib)]
pub struct MultiResultMetadata<V>
where
    V: Persistable,
{
    pub partial_results: PVec<TypedIPtr<MultiResultAccumulator<V>>>,
}

impl<V> PersistentlyAllocatable for MultiResultMetadata<V>
where
    V: Persistable,
{
    fn set_allocator(&mut self, allocator: Arc<RwLock<BumpAllocator>>) {
        self.partial_results.set_allocator(allocator);
    }

    fn get_allocator(&self) -> Option<Arc<RwLock<BumpAllocator>>> {
        self.partial_results.get_allocator()
    }
}

#[repr(C)]
#[derive(PersistableDeriveLib)]
pub enum LookupEntry<V>
where
    V: Persistable,
{
    NotFound,
    LookupIn(ObjectId),
    Found(V),
}

#[repr(C)]
#[derive(PersistableDeriveLib)]
pub struct MultiResultAccumulator<V>
where
    V: Persistable,
{
    pub results: PHashMap<PString, V>,
}

impl<V> PersistentlyAllocatable for MultiResultAccumulator<V>
where
    V: Persistable,
{
    fn set_allocator(&mut self, allocator: Arc<RwLock<BumpAllocator>>) {
        self.results.set_allocator(allocator);
    }

    fn get_allocator(&self) -> Option<Arc<RwLock<BumpAllocator>>> {
        self.results.get_allocator()
    }
}
