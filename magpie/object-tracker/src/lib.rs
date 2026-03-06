#![feature(allocator_api, sync_unsafe_cell)]

use std::sync::Arc;

use dashmap::DashMap;
use nando_support::log_entry::TransactionLogEntry;
pub use nando_support::{iptr::IPtr, ObjectId, ObjectVersion};
use object_lib::error::{AccessError, AllocationError, ObjectOperationError};
use object_lib::files;
pub use object_lib::object::Object;
use object_lib::object::MAX_OBJECT_SIZE_BYTES;
pub use object_lib::persistable::Persistable;
use object_lib::tls as object_lib_tls;
use object_lib::MaterializedObjectVersion;
use parking_lot::Mutex;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
pub use tls as object_tracker_tls;

use crate::version_manager::VersionManager;

pub mod error;
pub mod tls;
pub mod utils;
mod version_manager;

pub struct ObjectTracker {
    object_id_generator: Mutex<StdRng>,
    object_map: DashMap<ObjectId, Arc<Object>>,
    version_manager: VersionManager,
}

impl ObjectTracker {
    pub fn new(client_id: u16) -> Option<Self> {
        println!("in with client_id {client_id}");
        // FIXME the initial capacity of the worker's local maps should be in the same OoM as the
        // number of files under the object directory
        let initial_capacity = 8;
        let maybe_seed: Result<[u8; 32], _> = (0..32).collect::<Vec<_>>().try_into();

        match maybe_seed {
            Ok(mut seed) => {
                let places_to_rotate = client_id.into();
                // FIXME @hack
                seed.rotate_right(places_to_rotate);
                let rng: StdRng = SeedableRng::from_seed(seed);

                Some(Self {
                    object_id_generator: Mutex::new(rng),
                    object_map: DashMap::with_capacity(initial_capacity),
                    version_manager: VersionManager::new(initial_capacity),
                })
            }
            _ => None,
        }
    }

    /// Generates an object ID for an application object. We currently reserve the lower 32 bits of
    /// the global address space for system-level objects and for objects that need to be
    /// "well-known" even before a subocomponent's first initialization (like the KVS root object).
    fn generate_safe_object_id(&self) -> Option<ObjectId> {
        let mut id_generator = self.object_id_generator.lock();
        let object_id = loop {
            let candidate_id = id_generator
                .gen::<ObjectId>()
                .wrapping_shl(nando_support::SYSTEM_ADDR_SPACE_WIDTH);

            if !self.object_map.contains_key(&candidate_id) {
                break candidate_id;
            }
        };

        Some(object_id)
    }

    pub fn allocate(
        &self,
        size: usize,
        object_id: Option<ObjectId>,
    ) -> Result<Arc<Object>, ObjectOperationError> {
        if size > MAX_OBJECT_SIZE_BYTES {
            return Err(ObjectOperationError::OperationAllocationError(
                AllocationError::InvalidSize(size, MAX_OBJECT_SIZE_BYTES),
            ));
        }

        let new_object_id: ObjectId = match object_id {
            Some(o) => o,
            None => self
                .generate_safe_object_id()
                .expect("failed to allocate object id"),
        };
        // FIXME given that this is `allocate()`, we should probably have a flag that tells the
        // underlying object structure to reset the underlying storage as opposed to simply doing
        // an `open()` (as we should in `ObjectTracker::open()`).
        let new_object = match Object::new(new_object_id, size) {
            Some(o) => Arc::new(o),
            None => return Err(ObjectOperationError::UnknownError()),
        };

        if !new_object.is_initialized() {
            object_lib_tls::add_allocated_object_to_write_set(new_object.id);
        }
        self.object_map.insert(new_object_id.clone(), new_object);

        match self.get(new_object_id) {
            Some(o) => Ok(o),
            None => Err(ObjectOperationError::UnknownError()),
        }
    }

    pub fn copy_underlying_object(
        &self,
        object: &Object,
        into_object: Option<ObjectId>,
    ) -> Option<ObjectId> {
        let copy_id = match into_object {
            Some(oid) => {
                let _ = self.close(oid).expect("failed to close");
                oid
            }
            None => match self.generate_safe_object_id() {
                None => {
                    eprintln!("failed to allocate object id for copy of {}", object.id);

                    return None;
                }
                Some(oid) => oid,
            },
        };
        object.copy_as(copy_id);

        // TODO @cache do we also need to load it into our local object map?
        Some(copy_id)
    }

    pub fn open(&self, object_id: ObjectId) -> Result<Arc<Object>, ObjectOperationError> {
        if self.object_map.contains_key(&object_id) {
            return Ok(self.get(object_id).unwrap());
        }

        let new_object = match Object::new(object_id, 8) {
            Some(o) => Arc::new(o),
            None => {
                return Err(ObjectOperationError::OperationAccessError(
                    AccessError::UnknownObject(object_id.to_string()),
                ))
            }
        };
        self.object_map.insert(object_id.clone(), new_object);

        match self.get(object_id) {
            Some(o) => Ok(o),
            None => Err(ObjectOperationError::UnknownError()),
        }
    }

    pub fn close(&self, object_id: ObjectId) -> Result<(), AccessError> {
        match self.object_map.remove(&object_id) {
            Some((_k, v)) => {
                v.flush();
            }
            _ => {}
        }
        Ok(())
    }

    pub fn reload(&self, object_id: ObjectId) -> Result<(), ObjectOperationError> {
        match self.get(object_id) {
            Some(o) => o.reload(),
            None => {
                return Err(ObjectOperationError::UnknownObject(object_id));
            }
        }

        Ok(())
    }

    pub fn reset_header(&self, object_id: ObjectId) -> Result<(), ObjectOperationError> {
        match self.get(object_id) {
            Some(o) => o.reset_header(),
            None => {
                return Err(ObjectOperationError::UnknownObject(object_id));
            }
        }

        Ok(())
    }

    pub fn get(&self, object_id: ObjectId) -> Option<Arc<Object>> {
        self.object_map
            .get(&object_id)
            .and_then(|o| Some(Arc::clone(&o)))
    }

    pub fn get_as_bytes(&self, object_id: ObjectId) -> Result<Vec<u8>, AccessError> {
        match self.get(object_id) {
            Some(o) => Ok(unsafe { o.as_bytes().as_ref() }.unwrap().to_vec()),
            None => return Err(AccessError::UnknownObject(object_id.to_string())),
        }
    }

    pub fn get_source_address_mut(&self, object_id: ObjectId) -> Result<*mut u8, AccessError> {
        match self.get(object_id) {
            Some(o) => Ok(o.get_source_address_mut()),
            None => return Err(AccessError::UnknownObject(object_id.to_string())),
        }
    }

    pub fn flush(&self, object_id: ObjectId) -> Result<(), AccessError> {
        match self.get(object_id) {
            Some(o) => {
                o.flush();
                Ok(())
            }
            None => return Err(AccessError::UnknownObject(object_id.to_string())),
        }
    }

    pub fn get_backing_storage_path(
        &self,
        object_id: ObjectId,
    ) -> Result<std::path::PathBuf, AccessError> {
        match self.get(object_id) {
            Some(o) => Ok(o.get_backing_storage_path()),
            None => Err(AccessError::UnknownObject(object_id.to_string())),
        }
    }

    pub fn get_keys(&self) -> Vec<ObjectId> {
        let ro_object_map = self.object_map.clone().into_read_only();
        ro_object_map.keys().map(|e| *e).collect()
    }

    // FIXME rename
    pub fn object_is_local(&self, object_id: &ObjectId) -> bool {
        self.object_map.contains_key(object_id)
    }

    pub fn shutdown(&self) {
        for object in self.object_map.iter_mut() {
            object.flush();
            object.trim_to_true_allocation_size();
        }
    }

    pub fn drop_object(&self, object_id: ObjectId) {
        let Some(object_to_drop) = self.object_map.remove(&object_id) else {
            return;
        };
        object_to_drop.1.trim_to_true_allocation_size();
        drop(object_to_drop);
    }

    pub fn delete_object(&self, object_id: ObjectId) {
        let Some(object_to_delete) = self.object_map.remove(&object_id) else {
            return;
        };

        object_to_delete.1.delete_storage();
        self.version_manager.delete_object(object_id);
    }

    pub fn push_initial_version(&self, object_id: ObjectId, object: MaterializedObjectVersion) {
        self.version_manager
            .insert_initial_version(object_id, object)
    }

    pub fn push_initial_version_by_id(&self, object_id: ObjectId) {
        let object = self
            .object_map
            .get(&object_id)
            .expect("copy object not found in map");
        self.push_initial_version(object_id, object.value().into());
    }

    pub fn create_materialized_ro_version(&self, object_id: ObjectId) {
        let obj = self
            .get(object_id)
            .expect("cannot find object to create materialized version");
        let materialized_object = MaterializedObjectVersion::from(obj.as_ref());
        self.version_manager
            .insert_initial_version(object_id, materialized_object)
    }

    pub fn get_latest_committed(
        &self,
        object_id: ObjectId,
    ) -> Option<Arc<MaterializedObjectVersion>> {
        self.version_manager.get_latest(object_id)
    }

    pub fn get_at(
        &self,
        object_id: ObjectId,
        target_version: ObjectVersion,
    ) -> Option<Arc<MaterializedObjectVersion>> {
        self.version_manager.get_at(object_id, target_version)
    }

    pub fn push_versions<F>(&self, log_entry: &TransactionLogEntry, should_ignore: Option<F>)
    where
        F: Fn(ObjectId) -> bool,
    {
        for (object_id, images) in &log_entry.images {
            if let Some(ref p) = should_ignore {
                if (p)(*object_id) {
                    continue;
                }
            }

            let mut object_updates = Vec::with_capacity(images.len());
            for image in images {
                let post_value = image.get_post_value();
                if post_value.len() == 0 {
                    continue;
                }
                let iptr = image.get_field();
                object_updates.push((iptr, post_value));
            }

            if object_updates.is_empty() {
                continue;
            }

            #[cfg(feature = "object-caching")]
            let (version, _is_cacheable) = {
                let obj = match self.get(*object_id) {
                    None => continue,
                    Some(o) => o,
                };

                (obj.get_version(), obj.object_is_cacheable())
            };

            #[cfg(not(feature = "object-caching"))]
            let version = {
                let obj = match self.get(*object_id) {
                    None => continue,
                    Some(o) => o,
                };

                obj.get_version()
            };

            self.version_manager
                .push_version(*object_id, version, &object_updates);
        }
    }

    #[cfg(feature = "object-caching")]
    pub fn read_object_is_cacheable(&self, object_id: ObjectId) -> (bool, Option<ObjectVersion>) {
        self.version_manager.object_is_cacheable(object_id)
    }

    pub fn reset_object_directory(&self) {
        files::clear_allocation_dir();
        files::set_up_allocation_dir().expect("failed to set up allocation directory post removal");
    }

    pub fn read_object_directory(&self) {
        let dir_iterator = files::get_object_directory_iterator();

        for entry in dir_iterator {
            let Ok(ref entry) = entry else {
                eprintln!("Error for entry, skipping: {}", entry.err().unwrap());
                continue;
            };

            match entry.file_type() {
                Ok(ft) => {
                    if !ft.is_file() {
                        continue;
                    }
                }
                Err(e) => {
                    eprintln!(
                        "Could not get file type for entry {:?}, skipping: {}",
                        entry.file_name(),
                        e
                    );
                    continue;
                }
            }

            let object_id: ObjectId = entry
                .file_name()
                .to_str()
                .expect("failed to convert filename to string")
                .parse()
                .expect(&format!(
                    "failed to convert '{:?}' to object id",
                    entry.file_name()
                ));

            let object = self.open(object_id).expect(&format!(
                "failed to load object {} from object directory",
                object_id
            ));

            self.push_initial_version(object_id, (&*object).into());
        }
    }
}
