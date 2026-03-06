use std::fmt;

use object_lib::ObjectId;

use crate::HostId;

#[derive(Debug, Clone)]
pub enum MoveError {
    TransportError(HostId, String),
    // FIXME consider adding the invoked verb to the below enum variant.
    ServiceError(ObjectId, String),
}

unsafe impl Send for MoveError {}

impl fmt::Display for MoveError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::TransportError(host_id, transport_error) => f.write_fmt(format_args!(
                "Could not connect to host {}: {}",
                host_id, transport_error,
            )),
            Self::ServiceError(object_id, service_error_msg) => f.write_fmt(format_args!(
                "Could not move object {}: {}",
                object_id, service_error_msg,
            )),
        }
    }
}

#[derive(Debug, Clone)]
pub enum InternalError {
    UnknownError(),
}

unsafe impl Send for InternalError {}

impl fmt::Display for InternalError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            _ => write!(f, "An unknown error occured"),
        }
    }
}
