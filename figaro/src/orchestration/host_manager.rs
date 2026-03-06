use std::collections::HashMap;
use std::mem::MaybeUninit;
use std::sync::atomic::AtomicU64;
use std::sync::{atomic::Ordering, Arc, Once};

use ownership_support::{HostIdx, Hostname};
use parking_lot::RwLock;

use crate::orchestration::error::HostManagementError;

// NOTE so, for now, each host that registers with the scheduler receives a unique 64-bit index
// number. Host indices are *never* recycled -- if a host goes away, its old index should still be
// considered valid (because it is also part of ownership information), but its associated metadata
// in the manager's state should reflect that the host is no longer alive, and also contain enough
// informatino for a fixup operation on data that should be updated (like fwd'ing information if
// the host was replaced by an identical replica, for instance).
struct Host {
    /// Numerical idx of registered host.
    idx: HostIdx,

    hostname: Hostname,
    version: u16,
}

impl Host {
    fn new(idx: HostIdx, hostname: String) -> Self {
        Self {
            idx,
            hostname,
            version: 0,
        }
    }

    #[allow(dead_code)]
    fn from(from: Host) -> Self {
        Self {
            idx: from.idx,
            hostname: from.hostname,
            version: from.version + 1,
        }
    }
}

impl PartialEq for Host {
    fn eq(&self, other: &Self) -> bool {
        self.idx == other.idx && self.version == other.version && self.hostname == other.hostname
    }
}

// TODO this probably needs to do some status tracking for hosts (under repair, stable, etc.) when
// we start working on fault tolerance and recovery.
pub(crate) struct HostManager {
    // FIXME in the future we should probably check this for overflow.
    idx_generator: AtomicU64,
    host_map: RwLock<HashMap<HostIdx, Arc<Host>>>,
    hostname_list: RwLock<Vec<Hostname>>,
}

impl HostManager {
    fn new() -> Self {
        Self {
            idx_generator: AtomicU64::new(0),
            host_map: RwLock::new(HashMap::new()),
            hostname_list: RwLock::new(Vec::with_capacity(32)),
        }
    }

    pub(crate) fn get_host_manager() -> &'static Arc<HostManager> {
        static mut INSTANCE: MaybeUninit<Arc<HostManager>> = MaybeUninit::uninit();
        static mut ONCE: Once = Once::new();

        unsafe {
            ONCE.call_once(|| {
                INSTANCE.as_mut_ptr().write(Arc::new(HostManager::new()));
            });
        }

        unsafe { &*INSTANCE.as_ptr() }
    }

    pub(crate) fn number_of_registered_hosts(&self) -> usize {
        self.host_map.read().len()
    }

    pub fn insert_host_with_idx(
        &self,
        host_idx: HostIdx,
        hostname: String,
    ) -> Result<HostIdx, HostManagementError> {
        {
            let host_map = self.host_map.read();
            if let Some(host) = host_map.get(&host_idx) {
                if host.hostname != hostname {
                    return Err(HostManagementError::HostAlreadyRegistered(
                        host.hostname.clone(),
                        host.idx,
                    ));
                }

                // previously registered, host just re-registered with the same idx, so this is a
                // noop for the manager.
                return Ok(host_idx);
            }
        }

        let host_entry = Arc::new(Host::new(host_idx as HostIdx, hostname.clone()));
        {
            let mut hostname_list = self.hostname_list.write();
            hostname_list.push(hostname);
        }
        {
            let mut host_map = self.host_map.write();
            host_map.insert(host_idx, host_entry);
        }

        Ok(host_idx)
    }

    pub fn insert_host(&self, hostname: Hostname) -> Result<HostIdx, HostManagementError> {
        let host_len = self.hostname_list.read().len().try_into().unwrap();
        let tentative_idx = self.idx_generator.fetch_add(1, Ordering::Relaxed);
        // FIXME at the moment this should be fine, as we don't expect to have hosts attempting to
        // register by explicitly specifying an ID. However, in the future, this might lead to
        // holes in the host id space.
        let host_idx = if tentative_idx >= host_len {
            tentative_idx
        } else {
            // FIXME this blind write is actually wrong in the presence of multiple threads running
            // `insert_host()` concurerntly
            self.idx_generator.store(host_len, Ordering::Relaxed);
            host_len
        };

        self.insert_host_with_idx(host_idx, hostname)
    }

    pub fn get_hostname_by_idx(&self, host_idx: HostIdx) -> Option<Hostname> {
        let hostname_list = self.hostname_list.read();
        match hostname_list.get(host_idx as usize) {
            Some(h) => Some(h.clone()),
            None => None,
        }
    }

    pub fn get_hostname_projection(&self) -> HashMap<HostIdx, Hostname> {
        let hostname_mapping = self.host_map.read();
        let mut hostname_projection = HashMap::with_capacity(hostname_mapping.len());
        for (host_idx, host) in hostname_mapping.iter() {
            hostname_projection.insert(*host_idx, host.hostname.clone());
        }

        hostname_projection
    }

    pub fn get_num_workers(&self) -> usize {
        self.host_map.read().len()
    }

    pub fn reset_state(&self) {
        {
            let mut hostname_mapping = self.host_map.write();
            hostname_mapping.clear();
        }

        {
            let mut hostname_list = self.hostname_list.write();
            hostname_list.clear();
        }

        self.idx_generator.store(0, Ordering::Relaxed);
    }
}
