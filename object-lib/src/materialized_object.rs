use std::sync::Arc;
use std::{mem, ptr::copy_nonoverlapping};

use nando_support::{ImageValue, ObjectVersion};

use crate::{
    collections::pmap::PHashMap,
    object::ObjectHeader,
    tls::{IntoObjectMapping, ObjectMapping},
};
use crate::{IPtr, Object, ObjectId, Persistable};

#[derive(Clone, Debug)]
pub struct MaterializedObjectVersion {
    id: ObjectId,
    version: ObjectVersion,
    is_cacheable: bool,
    source_bytes: Vec<u8>,
}

impl MaterializedObjectVersion {
    fn get_constraint_set(&self) -> &'static PHashMap<ObjectId, ObjectVersion> {
        let map_offset = mem::offset_of!(ObjectHeader, object_version_constraints);
        let pmap_size = mem::size_of::<PHashMap<ObjectId, ObjectVersion>>();
        let bytes = &self.source_bytes.as_slice()[map_offset..(map_offset + pmap_size)];
        PHashMap::from_bytes_ref(bytes as *const [u8])
    }

    pub fn read_into<T: Persistable>(&self) -> &'static T {
        let bytes = &self.source_bytes.as_slice()[Object::header_size()..];
        return T::from_bytes_ref(bytes as *const [u8]);
    }

    pub fn apply_changes(
        &mut self,
        new_version: ObjectVersion,
        changes: &Vec<(IPtr, &ImageValue)>,
    ) {
        self.version = new_version;

        for (iptr, change) in changes {
            let offset: usize = iptr.offset.try_into().unwrap();
            let count = iptr.size.try_into().unwrap();
            if offset + count > self.source_bytes.len() {
                let extra_len = offset + count - self.source_bytes.len();
                self.source_bytes
                    .resize(self.source_bytes.len() + extra_len, 0);
            }

            let source_bytes = self.source_bytes.as_mut_ptr();
            unsafe {
                let target_slice = source_bytes.byte_offset(offset.try_into().unwrap());
                copy_nonoverlapping(change.data.as_ptr(), target_slice as *mut u8, count);
            }
        }
    }

    pub fn set_cacheable(&mut self, cacheable: bool) {
        self.is_cacheable = cacheable;
    }

    pub fn is_cacheable(&self) -> bool {
        self.is_cacheable
    }

    pub fn get_id(&self) -> ObjectId {
        self.id
    }

    pub fn get_version(&self) -> ObjectVersion {
        self.version
    }

    pub fn iptr_of(&self) -> IPtr {
        IPtr {
            object_id: self.id,
            offset: 0,
            size: 0,
        }
    }

    pub fn get_mapping_bounds(&self) -> (usize, usize) {
        let slice = &self.source_bytes.as_slice()[Object::header_size()..];
        let start = slice as *const _ as *const () as usize;
        (start, start + self.source_bytes.len())
    }

    pub fn get_version_constraint(&self, foreign_object: ObjectId) -> Option<ObjectVersion> {
        self.get_constraint_set().get(&foreign_object).copied()
    }

    pub fn from_version(other: &MaterializedObjectVersion) -> Self {
        let original_len = other.source_bytes.len();
        let mut source_bytes = Vec::with_capacity(original_len);

        unsafe {
            other
                .source_bytes
                .as_ptr()
                .copy_to_nonoverlapping(source_bytes.as_mut_ptr(), original_len);
            source_bytes.set_len(original_len);
        }

        Self {
            id: other.id,
            version: other.version,
            is_cacheable: other.is_cacheable,
            source_bytes,
        }
    }
}

impl From<&Arc<Object>> for MaterializedObjectVersion {
    fn from(obj: &Arc<Object>) -> Self {
        MaterializedObjectVersion {
            id: obj.id,
            version: obj.get_version(),
            is_cacheable: obj.object_is_cacheable(),
            source_bytes: unsafe { (&*obj.as_bytes()).to_vec() },
        }
    }
}

impl From<&Object> for MaterializedObjectVersion {
    fn from(obj: &Object) -> Self {
        MaterializedObjectVersion {
            id: obj.id,
            version: obj.get_version(),
            is_cacheable: obj.object_is_cacheable(),
            source_bytes: unsafe { (&*obj.as_bytes()).to_vec() },
        }
    }
}

impl IntoObjectMapping for Arc<MaterializedObjectVersion> {
    fn into_mapping(&self) -> ObjectMapping {
        let (start, end) = self.get_mapping_bounds();

        ObjectMapping {
            id: self.id,
            start,
            end,
        }
    }
}
