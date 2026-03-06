use std::fmt;

use location_manager::HostId;

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum InvocationError {
    // FIXME
    TransportError(HostId, String),
    ServiceError((String, String, String), String),
    UnknownError(),
}

impl fmt::Display for InvocationError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::TransportError(host_id, transport_error) => f.write_fmt(format_args!(
                "Could not connect to host {}: {}",
                host_id, transport_error,
            )),
            Self::ServiceError(set_ids, service_error_msg) => f.write_fmt(format_args!(
                "Could not write set difference of {} and {} to {}: {}",
                set_ids.0, set_ids.1, set_ids.2, service_error_msg,
            )),
            Self::UnknownError() => write!(f, "An unknown error occured"),
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
