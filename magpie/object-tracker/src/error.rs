use std::fmt;
use std::path::PathBuf;

use crate::{ObjectId, ObjectVersion};

#[derive(Debug, Clone)]
pub enum AllocationError {
    InvalidSize(usize, usize),
    BackingStorageFull(),
}

impl fmt::Display for AllocationError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::InvalidSize(req, lim) => f.write_fmt(format_args!(
                "Requested size ({}) exceeds maximum ({})",
                req, lim
            )),
            _ => write!(f, "An unknown error occured"),
        }
    }
}

#[derive(Debug, Clone)]
pub enum AccessError {
    UnknownObject(String),
    InvalidAddress(),
}

impl fmt::Display for AccessError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::UnknownObject(object_id) => {
                f.write_fmt(format_args!("No object with id {} exists", object_id))
            }
            _ => write!(f, "An unknown error occured"),
        }
    }
}

#[derive(Debug, Clone)]
pub enum UpdateError {
    VersionOverflowError(ObjectId, ObjectVersion),
}

impl fmt::Display for UpdateError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::VersionOverflowError(object_id, initial_object_version) => {
                f.write_fmt(format_args!(
                    "Cannot update version {} of object {}, operation would lead to overflow",
                    initial_object_version, object_id
                ))
            }
        }
    }
}

#[derive(Debug, Clone)]
pub enum ObjectOperationError {
    OperationAllocationError(AllocationError),
    OperationAccessError(AccessError),
    OperationUpdateError(UpdateError),
    UnknownObject(ObjectId),
    UnknownError(),
}

impl fmt::Display for ObjectOperationError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::UnknownObject(oid) => {
                f.write_fmt(format_args!("Request for unknown object {oid}"))
            }
            Self::UnknownError() => write!(f, "An unknown error occured"),
            e => e.fmt(f),
        }
    }
}

#[derive(Debug, Clone)]
pub enum FileError {
    DirCreationError(PathBuf),
    UnknownError(),
}

impl fmt::Display for FileError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::DirCreationError(d) => write!(
                f,
                "Failed to set up target directory {}",
                d.to_str().unwrap()
            ),
            _ => write!(f, "An unknown error occured"),
        }
    }
}
