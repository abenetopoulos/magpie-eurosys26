use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::activation_id::ActivationId;

#[derive(Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct EcbId {
    // FIXME we should extract the scheduler's HostIdx type into the common definitions
    host_idx: u64,
    activation_id: ActivationId,
}

impl EcbId {
    pub fn new(host_idx: u64, activation_id: ActivationId) -> Self {
        Self {
            host_idx,
            activation_id,
        }
    }

    pub fn get_host_idx(&self) -> u64 {
        self.host_idx
    }

    pub fn get_activation_id(&self) -> ActivationId {
        self.activation_id
    }

    pub fn get_root_id(&self) -> Self {
        Self {
            host_idx: self.host_idx,
            activation_id: ActivationId::new_for_txn(self.activation_id.txn_id()),
        }
    }
}

impl fmt::Debug for EcbId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_fmt(format_args!("{}:{}", self.host_idx, self.activation_id))
    }
}

impl fmt::Display for EcbId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_fmt(format_args!("{}:{}", self.host_idx, self.activation_id))
    }
}

impl FromStr for EcbId {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.split_once(':') {
            Some((host_idx_str, activation_id_str)) => Ok(Self {
                host_idx: host_idx_str
                    .parse()
                    .expect("failed to parse host idx in ecb id"),
                activation_id: ActivationId::from_str(activation_id_str)
                    .expect("failed to parse activation id"),
            }),
            None => Err(format!("invalid string passed for ecb id: {}", s)),
        }
    }
}
