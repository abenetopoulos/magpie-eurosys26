use std::marker::PhantomData;

use serde::{Deserialize, Serialize};

use crate::ObjectId;

#[repr(C)]
#[derive(Clone, Copy, Debug, Hash, Eq, Serialize, Deserialize)]
pub struct IPtr {
    pub object_id: ObjectId,
    pub offset: u64,
    pub size: u64,
}

impl PartialEq for IPtr {
    fn eq(&self, other: &IPtr) -> bool {
        self.object_id == other.object_id && self.offset == other.offset && self.size == other.size
    }

    fn ne(&self, other: &IPtr) -> bool {
        self.object_id != other.object_id || self.offset != other.offset || self.size != other.size
    }
}

impl IPtr {
    pub fn new(object_id: ObjectId, offset: u64, size: u64) -> Self {
        Self {
            object_id,
            offset,
            size,
        }
    }

    pub fn as_byte_array(&self) -> [u8; 32] {
        let mut res = [0; 32];
        res[0..16].copy_from_slice(&self.object_id.to_ne_bytes());
        res[16..24].copy_from_slice(&self.offset.to_ne_bytes());
        res[24..32].copy_from_slice(&self.size.to_ne_bytes());

        res
    }

    pub fn get_object_id(&self) -> ObjectId {
        self.object_id
    }
}

impl Default for IPtr {
    fn default() -> Self {
        Self {
            object_id: 0,
            offset: 0,
            size: 0,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct TypedIPtr<T> {
    inner: IPtr,
    pointee_type: PhantomData<T>,
}

impl<T> TypedIPtr<T> {
    pub fn get_inner(&self) -> IPtr {
        self.inner
    }

    pub fn get_object_id(&self) -> ObjectId {
        self.inner.get_object_id()
    }
}

impl<T> From<IPtr> for TypedIPtr<T> {
    fn from(value: IPtr) -> Self {
        Self {
            inner: value,
            pointee_type: PhantomData,
        }
    }
}

impl<T> std::fmt::Debug for TypedIPtr<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_fmt(format_args!("{}", self.inner.object_id))
    }
}

impl<T> std::fmt::Display for TypedIPtr<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_fmt(format_args!("{}", self.inner.object_id))
    }
}
