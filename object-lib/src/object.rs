use std::sync::Arc;

use lazy_static::lazy_static;
use nando_support::iptr::IPtr;
use nando_support::{ecb_id::EcbId, ObjectId, ObjectVersion};
use parking_lot::RwLock;

#[cfg(feature = "object-caching")]
use crate::collections::pvec::{PVec, PVecIter};
use crate::{
    allocators::{
        bump_allocator::BumpAllocator, persistently_allocatable::PersistentlyAllocatable,
    },
    collections::pmap::PHashMap,
    error, files,
    persistable::Persistable,
    tls::{IntoObjectMapping, ObjectMapping},
};

// twizzler <3
pub static MAX_OBJECT_SIZE_BYTES: usize = 1_073_741_824;

#[cfg(not(feature = "no-persist"))]
lazy_static! {
    static ref MV_SIZE_THRESHOLD: usize = match std::env::var("MV_SIZE_THRESHOLD") {
        Err(_) => MAX_OBJECT_SIZE_BYTES,
        Ok(e) => e.parse().unwrap(),
    };
}

#[cfg(feature = "no-persist")]
lazy_static! {
    // If we are not using our persistence features, then we are also disabling image tracking
    // which means we cannot maintain versions, so allo objects needs to be non-mv.
    static ref MV_SIZE_THRESHOLD: usize = 0;
}

#[cfg(feature = "object-caching")]
lazy_static! {
    static ref CACHING_READ_THRESHOLD: u8 = match std::env::var("CACHING_READ_THRESHOLD") {
        Err(_) => 64,
        Ok(e) => e.parse().unwrap(),
    };
}

#[cfg(feature = "object-caching")]
enum CachingFitness {
    NotCacheable,
    #[allow(dead_code)]
    ProbablyCacheable,
    Cacheable,
}

#[derive(Debug, Copy, Clone)]
pub enum CachingPermitted {
    // TODO @cache verify that we can actually safely persist EcbId directly.
    UnderInvalidation(EcbId),
    PermissibleIfEffective,
}
impl Persistable for CachingPermitted {}

#[derive(Debug)]
pub(crate) struct ObjectHeader {
    pub(crate) id: ObjectId,
    pub(crate) version: ObjectVersion,
    access_history: u8,

    #[cfg(feature = "object-caching")]
    caching_permitted: CachingPermitted,
    #[cfg(feature = "object-caching")]
    caches: PVec<ObjectId>,

    pub(crate) object_version_constraints: PHashMap<ObjectId, ObjectVersion>,

    pub needs_allocator_reset: bool,
    is_mv_enabled: bool,
}
impl Persistable for ObjectHeader {}

impl PersistentlyAllocatable for ObjectHeader {
    fn set_allocator(&mut self, allocator: Arc<RwLock<BumpAllocator>>) {
        #[cfg(feature = "object-caching")]
        self.caches.set_allocator(Arc::clone(&allocator));
        self.object_version_constraints.set_allocator(allocator);
    }

    fn get_allocator(&self) -> Option<Arc<RwLock<BumpAllocator>>> {
        self.object_version_constraints.get_allocator()
    }
}

impl ObjectHeader {
    fn init(&mut self, id: ObjectId) {
        self.id = id;
        self.version = 0;

        #[cfg(feature = "object-caching")]
        {
            self.caches = PVec::new();
            self.caching_permitted = CachingPermitted::PermissibleIfEffective;
        }
        self.reset_access_history();

        self.object_version_constraints = PHashMap::new();

        self.is_mv_enabled = true;
    }

    fn bump_version(&mut self) -> Result<ObjectVersion, error::ObjectOperationError> {
        match self.version.overflowing_add(1) {
            (result, false) => {
                self.version = result;
                self.reset_access_history();
                Ok(self.version)
            }
            (_, true) => Err(error::ObjectOperationError::OperationUpdateError(
                error::UpdateError::VersionOverflowError(self.id, self.version),
            )),
        }
    }

    #[inline]
    fn reset_access_history(&mut self) {
        self.access_history = 0;
    }

    #[inline]
    #[cfg(feature = "object-caching")]
    fn add_read(&mut self) {
        if self.access_history == u8::MAX {
            return;
        }

        self.access_history += 1;
    }

    #[cfg(feature = "object-caching")]
    fn add_cache(&mut self, cache_id: ObjectId) {
        for cache in &self.caches {
            if *cache == cache_id {
                return;
            }
        }

        self.caches.push(cache_id);
    }

    #[cfg(feature = "object-caching")]
    fn iter_caches(&self) -> PVecIter<'_, u128> {
        self.caches.iter()
    }

    #[cfg(feature = "object-caching")]
    fn is_cached(&self) -> bool {
        !self.caches.is_empty()
    }

    #[cfg(feature = "object-caching")]
    fn reset_caches(&mut self) {
        self.caches.truncate(0);
    }

    #[cfg(feature = "object-caching")]
    fn get_caching_fitness(&self) -> CachingFitness {
        if let CachingPermitted::UnderInvalidation(_) = self.caching_permitted {
            return CachingFitness::NotCacheable;
        }

        if self.access_history < *CACHING_READ_THRESHOLD {
            return CachingFitness::NotCacheable;
        }

        CachingFitness::Cacheable
    }

    #[cfg(feature = "object-caching")]
    fn set_caching_permitted(&mut self, permitted: CachingPermitted) {
        self.caching_permitted = permitted;
    }

    #[cfg(feature = "object-caching")]
    fn get_caching_permitted(&self) -> CachingPermitted {
        self.caching_permitted.clone()
    }

    // FIXME change return type to result
    fn upsert_version_constraint(
        &mut self,
        foreign_object: ObjectId,
        observed_version: ObjectVersion,
    ) -> Option<ObjectVersion> {
        match self.object_version_constraints.get(&foreign_object) {
            None => {}
            Some(previously_observed_version) => {
                if *previously_observed_version >= observed_version {
                    return None;
                }
            }
        }

        self.object_version_constraints
            .insert(foreign_object, observed_version)
    }

    fn get_version_constraint(&self, foreign_object: ObjectId) -> Option<ObjectVersion> {
        self.object_version_constraints
            .get(&foreign_object)
            .copied()
    }

    fn set_mv_enabled(&mut self, enabled: bool) {
        self.is_mv_enabled = enabled;
    }

    fn is_mv_enabled(&self) -> bool {
        self.is_mv_enabled
    }
}

pub struct Object {
    pub id: ObjectId,
    pub size: usize,

    backing_storage: Arc<RwLock<files::FileHandle>>,

    // FIXME @question does this need to be wrapped in Arc<RwLock<>>?
    pub backing_allocator: Arc<RwLock<BumpAllocator>>,
}

impl Object {
    pub fn new(id: ObjectId, size: usize) -> Option<Self> {
        let file_handle = match files::open_for_id(id, size) {
            Ok(fh) => fh,
            Err(e) => {
                eprintln!("Could not acquire backing storage for object {id}: {:?}", e);
                return None;
            }
        };
        let size = file_handle.file_size;
        let backing_storage = Arc::new(RwLock::new(file_handle));
        let allocator = BumpAllocator::new(Arc::clone(&backing_storage));

        Some(Self {
            id,
            size,
            backing_storage,
            backing_allocator: Arc::new(RwLock::new(allocator)),
        })
    }

    pub fn is_initialized(&self) -> bool {
        let header = self.get_header();
        header.version > 0
    }

    pub fn allocate<T: Persistable>(&self) -> Result<&mut T, error::AllocationError> {
        let header_size = std::mem::size_of::<ObjectHeader>();
        let read_size = std::mem::size_of::<T>();
        let allocation_size = header_size + read_size;

        {
            let mut backing_storage = self.backing_storage.write();
            #[cfg(not(feature = "no-persist"))]
            files::resize(&mut backing_storage, allocation_size)
                .expect("failed to resize on allocate");
            backing_storage.allocation_marker = allocation_size;
        }

        {
            // init meta information
            let header = self.get_header();
            header.init(self.id);
            header.set_allocator(Arc::clone(&self.backing_allocator));
            header.object_version_constraints.with_capacity(8);

            #[cfg(feature = "object-caching")]
            header.caches.resize_to_capacity(8);

            header.needs_allocator_reset = true;
        }

        let backing_storage = self.backing_storage.read();
        let source_bytes: *mut [u8] = files::read_from(
            &backing_storage,
            header_size.try_into().unwrap(),
            read_size.try_into().unwrap(),
        );

        let res = unsafe { T::from_bytes(source_bytes).as_mut() }.unwrap();
        Ok(res)
    }

    pub fn maybe_set_inner_allocator<T>(&self, inner: &mut T)
    where
        T: Persistable + PersistentlyAllocatable,
    {
        {
            let header = self.get_header();
            if !header.needs_allocator_reset {
                return;
            }
            header.set_allocator(Arc::clone(&self.backing_allocator));
            header.needs_allocator_reset = false;
        }
        inner.set_allocator(Arc::clone(&self.backing_allocator));
    }

    pub fn set_inner_allocator<T>(&self, inner: &mut T)
    where
        T: Persistable + PersistentlyAllocatable,
    {
        inner.set_allocator(Arc::clone(&self.backing_allocator));
    }

    pub fn read_into<T: Persistable>(
        &self,
        ptr: Option<&IPtr>,
    ) -> Result<*mut T, error::AccessError> {
        // FIXME speed I think we can cache results in check the size has not changed since the
        // last load.
        let backing_storage = self.backing_storage.read();
        let (read_size, read_offset) = match ptr {
            None => (
                backing_storage.file_size.try_into().unwrap(),
                std::mem::size_of::<ObjectHeader>().try_into().unwrap(),
            ),
            Some(IPtr {
                object_id,
                size,
                offset,
            }) => {
                assert_eq!(self.id, *object_id);
                (*size, *offset)
            }
        };

        let source_bytes: *mut [u8] = files::read_from(&backing_storage, read_offset, read_size);

        return Ok(T::from_bytes(source_bytes));
    }

    pub fn read_into_mut<T: Persistable>(
        &self,
        ptr: Option<&IPtr>,
    ) -> Result<&mut T, error::AccessError> {
        // FIXME speed I think we can cache results in check the size has not changed since the
        // last load.
        let backing_storage = self.backing_storage.read();
        let (read_size, read_offset) = match ptr {
            None => (
                backing_storage.file_size.try_into().unwrap(),
                std::mem::size_of::<ObjectHeader>().try_into().unwrap(),
            ),
            Some(IPtr {
                object_id,
                size,
                offset,
            }) => {
                assert_eq!(self.id, *object_id);
                (*size, *offset)
            }
        };

        let source_bytes: *mut [u8] = files::read_from(&backing_storage, read_offset, read_size);

        return Ok(unsafe { T::from_bytes(source_bytes).as_mut().unwrap() });
    }

    fn get_source_address(&self) -> *const u8 {
        // FIXME this should probably be replaced with a simple call to `as_ptr`
        let file_handle = self.backing_storage.read();
        let mapped_file = file_handle.mapped_file.read().unwrap();
        let region_start = mapped_file.first().unwrap();
        std::ptr::addr_of!(*region_start)
    }

    pub fn get_source_address_mut(&self) -> *mut u8 {
        // FIXME this should probably be replaced with a simple call to `as_ptr`
        let file_handle = self.backing_storage.read();
        let mut mapped_file = file_handle.mapped_file.write().unwrap();
        let region_start = mapped_file.first_mut().unwrap();
        std::ptr::addr_of_mut!(*region_start)
    }

    pub fn offset_of_raw(&self, field: *const ()) -> u32 {
        let source_address = self.get_source_address();
        unsafe { field.byte_offset_from(source_address as *const ()) }
            .try_into()
            .unwrap()
    }

    pub fn iptr_of(&self) -> IPtr {
        IPtr {
            object_id: self.id,
            offset: 0,
            size: 0,
        }
    }

    pub fn offset_of(&self, field: *const ()) -> IPtr {
        IPtr {
            object_id: self.id,
            offset: self.offset_of_raw(field).into(),
            size: 0,
        }
    }

    pub fn flush(&self) -> () {
        let backing_storage = self.backing_storage.read();
        files::sync(&backing_storage);
    }

    pub fn flush_range(&self, offset: u64, size: u64) -> () {
        let backing_storage = self.backing_storage.read();
        files::sync_range(&backing_storage, offset, size);
    }

    pub fn as_bytes(&self) -> *const [u8] {
        let backing_storage = self.backing_storage.read();
        backing_storage.as_bytes()
    }

    pub fn as_bytes_mut(&self) -> *mut [u8] {
        let backing_storage = self.backing_storage.read();
        let read_size = backing_storage.file_len();
        files::read_from(&backing_storage, 0, read_size)
    }

    pub fn content_as_bytes(&self) -> *const [u8] {
        // Similar to `as_bytes()`, but ignores the object header.
        let backing_storage = self.backing_storage.read();
        let header_len: u64 = std::mem::size_of::<ObjectHeader>().try_into().unwrap();
        let (read_size, read_offset) = (backing_storage.file_len() - header_len, header_len);

        files::read_from(&backing_storage, read_offset, read_size)
    }

    pub fn get_backing_storage_path(&self) -> std::path::PathBuf {
        let backing_storage = self.backing_storage.read();
        backing_storage.file_path.clone()
    }

    pub fn advise(&self) {
        let backing_storage = self.backing_storage.read();
        backing_storage.advise();
    }

    pub fn reload(&self) {
        {
            let mut backing_storage = self.backing_storage.write();
            files::remap(&mut backing_storage);
        }
        self.reset_header();
    }

    pub fn reset_header(&self) {
        let header = self.get_header();
        header.set_allocator(Arc::clone(&self.backing_allocator));
        header.needs_allocator_reset = true;
    }

    fn get_header(&self) -> &mut ObjectHeader {
        let header_size = std::mem::size_of::<ObjectHeader>();
        let backing_storage = self.backing_storage.read();
        let header_source_bytes: *mut [u8] =
            files::read_from(&backing_storage, 0, header_size.try_into().unwrap());

        unsafe { ObjectHeader::from_bytes(header_source_bytes).as_mut() }.unwrap()
    }

    pub fn bump_version(&self) -> Result<ObjectVersion, error::ObjectOperationError> {
        self.get_header().bump_version()
    }

    pub fn mark_read_access(&self) {
        #[cfg(feature = "object-caching")]
        {
            let header = self.get_header();
            header.add_read();
        }
    }

    pub fn get_id(&self) -> ObjectId {
        self.id
    }

    pub fn get_version(&self) -> ObjectVersion {
        self.get_header().version
    }

    pub fn object_is_cacheable(&self) -> bool {
        #[cfg(feature = "object-caching")]
        match self.get_header().get_caching_fitness() {
            CachingFitness::Cacheable => true,
            _ => false,
        }

        #[cfg(not(feature = "object-caching"))]
        false
    }

    #[cfg(feature = "object-caching")]
    pub fn get_cached_versions(&self) -> Vec<ObjectId> {
        self.get_header().iter_caches().map(|e| *e).collect()
    }

    #[cfg(feature = "object-caching")]
    pub fn reset_caches(&self) {
        self.get_header().reset_caches();
    }

    pub fn header_size() -> usize {
        std::mem::size_of::<ObjectHeader>()
    }

    pub fn set_mv_enabled(&self, enabled: bool) {
        self.get_header().set_mv_enabled(enabled);
    }

    fn is_mv_enabled(&self) -> bool {
        self.get_header().is_mv_enabled()
    }

    #[inline]
    pub fn object_is_mv_enabled(&self) -> bool {
        let backing_storage = self.backing_storage.read();
        self.is_mv_enabled() && backing_storage.file_size <= *MV_SIZE_THRESHOLD
    }

    pub fn get_mapping_bounds(&self) -> (usize, usize) {
        let source_address = self.get_source_address() as *const () as usize;
        (source_address, source_address + files::MAX_FILE_SIZE_BYTES)
    }

    #[cfg(feature = "object-caching")]
    pub fn set_under_invalidation(&self, invalidated_for: EcbId) {
        // The purpose of including the task ID of the task that triggered the cache invalidation
        // is so that nanotransactions that are scheduled between the invalidation being triggered
        // and the pending write (from the transaction that triggered the invalidation) will have
        // to be "parked" to be executed after that task's control dependencies have been
        // satisfied.
        self.get_header()
            .set_caching_permitted(CachingPermitted::UnderInvalidation(invalidated_for));
    }

    #[cfg(feature = "object-caching")]
    pub fn get_invalidating_task(&self) -> Option<EcbId> {
        match self.get_header().get_caching_permitted() {
            CachingPermitted::UnderInvalidation(invalidation_trigger_id) => {
                Some(invalidation_trigger_id)
            }
            _ => None,
        }
    }

    #[cfg(feature = "object-caching")]
    pub fn set_caching_permissible(&self) {
        self.get_header()
            .set_caching_permitted(CachingPermitted::PermissibleIfEffective);
    }

    pub fn copy_as(&self, copy_id: ObjectId) {
        files::create_object_copy(self.id, copy_id);
    }

    #[cfg(feature = "object-caching")]
    pub fn add_cache(&self, cache_id: ObjectId) {
        self.get_header().add_cache(cache_id)
    }

    #[cfg(feature = "object-caching")]
    pub fn is_cached(&self) -> bool {
        self.get_header().is_cached()
    }

    pub fn delete_storage(&self) {
        // NOTE should this receive a `mut self` instead?
        let backing_storage = self.backing_storage.write();
        match files::delete_storage(&backing_storage) {
            Ok(()) => (),
            Err(e) => {
                eprintln!(
                    "could not delete backing storage of object {}: {}",
                    self.id, e
                );
                todo!("handle object storage deletion failure");
            }
        }
    }

    #[cfg(not(feature = "no-persist"))]
    pub fn trim_to_true_allocation_size(&self) {
        let mut backing_storage = self.backing_storage.write();

        if backing_storage.file_size - backing_storage.allocation_marker == 0 {
            return;
        }

        let target_size = backing_storage.allocation_marker;
        match files::resize(&mut backing_storage, target_size) {
            Ok(()) => {}
            Err(e) => {
                eprintln!("failed while trimming object: {}", e);
                panic!("failed while trimming object");
            }
        }
    }

    #[cfg(feature = "no-persist")]
    pub fn trim_to_true_allocation_size(&self) {}

    pub fn get_constraint_set(&self) -> &PHashMap<ObjectId, ObjectVersion> {
        &self.get_header().object_version_constraints
    }

    pub fn get_version_constraint(&self, foreign_object: ObjectId) -> Option<ObjectVersion> {
        self.get_header().get_version_constraint(foreign_object)
    }

    pub fn upsert_version_constraint(
        &self,
        foreign_object: ObjectId,
        observed_version: ObjectVersion,
    ) {
        self.get_header()
            .upsert_version_constraint(foreign_object, observed_version);
    }

    pub fn is_under_storage_pressure(&self) -> bool {
        let backing_storage = self.backing_storage.read();
        backing_storage.is_under_pressure()
    }
}

impl IntoObjectMapping for Arc<Object> {
    fn into_mapping(&self) -> ObjectMapping {
        let (start, end) = self.get_mapping_bounds();

        ObjectMapping {
            id: self.id,
            start,
            end,
        }
    }
}
