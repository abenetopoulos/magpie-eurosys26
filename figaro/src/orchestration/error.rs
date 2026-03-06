use std::fmt;

use crate::orchestration::{HostIdx, Hostname};

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub(crate) enum OwnershipTransferError {
    HostUnreachable(Hostname),
    UnknownError(),
}

impl fmt::Display for OwnershipTransferError {
    fn fmt(&self, f: &mut fmt::Formatter) -> std::fmt::Result {
        match self {
            Self::HostUnreachable(h) => f.write_fmt(format_args!("Host {} is unreachable", h)),
            _ => write!(f, "An unknown error occured"),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) enum InternalError {
    UnknownError(),
}

impl fmt::Display for InternalError {
    fn fmt(&self, f: &mut fmt::Formatter) -> std::fmt::Result {
        match self {
            _ => write!(f, "An unknown error occured"),
        }
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub enum HostManagementError {
    HostAlreadyRegistered(Hostname, HostIdx),
    IndexUnavailable(HostIdx),
    UnknownError(),
}

impl fmt::Display for HostManagementError {
    fn fmt(&self, f: &mut fmt::Formatter) -> std::fmt::Result {
        match self {
            Self::HostAlreadyRegistered(hostname, host_idx) => f.write_fmt(format_args!(
                "Host '{}' is already registered as ID {}",
                hostname, host_idx,
            )),
            Self::IndexUnavailable(host_idx) => {
                f.write_fmt(format_args!("Host index '{}' cannot be claimed.", host_idx))
            }
            _ => write!(f, "An unknown error occured"),
        }
    }
}
