use std::collections::{HashMap, HashSet};
use std::mem::MaybeUninit;
use std::sync::Once;

use async_std::sync::Arc;
use nando_support::{ObjectId, ObjectVersion};
use parking_lot::RwLock;

use crate::orchestration::HostIdx;

#[derive(Clone)]
pub struct VersionRange {
    pub host_idx: HostIdx,
    pub start_version: ObjectVersion,
    pub end_version: ObjectVersion,
}

impl VersionRange {
    fn new(host_idx: HostIdx) -> Self {
        Self {
            host_idx,
            start_version: 1,
            end_version: 0,
        }
    }
}

pub struct OwnershipRegistry {
    past_ownership: RwLock<HashMap<ObjectId, RwLock<Vec<VersionRange>>>>,
    pub current_ownership: RwLock<HashMap<ObjectId, RwLock<VersionRange>>>,
}

impl OwnershipRegistry {
    fn new() -> Self {
        Self {
            past_ownership: RwLock::new(HashMap::new()),
            current_ownership: RwLock::new(HashMap::new()),
        }
    }

    pub(crate) fn get_ownership_registry() -> &'static Arc<OwnershipRegistry> {
        static mut INSTANCE: MaybeUninit<Arc<OwnershipRegistry>> = MaybeUninit::uninit();
        static mut ONCE: Once = Once::new();

        unsafe {
            ONCE.call_once(|| {
                INSTANCE
                    .as_mut_ptr()
                    .write(Arc::new(OwnershipRegistry::new()));
            });
        }

        unsafe { &*INSTANCE.as_ptr() }
    }

    pub fn get_owning_host(&self, object_id: &ObjectId) -> Option<HostIdx> {
        let current_ownership = self.current_ownership.read();
        let Some(vr_lock) = current_ownership.get(object_id) else {
            return None;
        };

        let vr = vr_lock.read();
        Some(vr.host_idx)
    }

    pub fn get_owning_hosts(&self, object_ids: &Vec<ObjectId>) -> HashMap<HostIdx, Vec<ObjectId>> {
        let mut res = HashMap::new();
        let current_ownership = self.current_ownership.read();
        for object_id in object_ids {
            let Some(vr_lock) = current_ownership.get(object_id) else {
                // FIXME
                panic!(
                    "attempting to get owning host of unpublished object {}",
                    object_id
                );
            };

            let vr = vr_lock.read();
            res.entry(vr.host_idx)
                .and_modify(|e: &mut Vec<ObjectId>| {
                    e.push(*object_id);
                })
                .or_insert({
                    let mut e = Vec::with_capacity(object_ids.len());
                    e.push(*object_id);
                    e
                });
        }

        res
    }

    /// Returns option with host id if objects are colocated, none otherwise.
    pub fn objects_are_colocated(&self, object_ids: &Vec<ObjectId>) -> Option<HostIdx> {
        // FIXME this is not atomic over the set of keys. If we used something like flurry we would
        // get a snapshot read through the iterator, which might be more appropriate for us.
        let mut present_ownership_information: HashSet<HostIdx> =
            HashSet::from_iter(object_ids.iter().map(|oid| {
                match self.current_ownership.read().get(oid) {
                    Some(e) => (*e.read()).host_idx.clone(),
                    // FIXME we should actually return a different kind of error here. The only
                    // way we might end up here is if we got a request to
                    None => todo!("request to schedule on unpublished object {}", oid),
                }
            }));

        match present_ownership_information.len() > 1 {
            true => None,
            false => Some(
                present_ownership_information
                    .drain()
                    .next()
                    .expect("failed to get owning host for colocated objects"),
            ),
        }
    }

    pub fn handle_publish(&self, published_object_id: ObjectId, owner: HostIdx) -> bool {
        // FIXME return type
        if let Some(e) = self.current_ownership.read().get(&published_object_id) {
            if e.read().host_idx != owner {
                return false;
            }

            return true;
        }

        let publication_vr = RwLock::new(VersionRange::new(owner));
        {
            let mut current_ownership = self.current_ownership.write();
            current_ownership.insert(published_object_id, publication_vr);
        }

        true
    }

    pub fn update_ownership(
        &self,
        target_object: ObjectId,
        whomstone_version: ObjectVersion,
        old_owner: HostIdx,
        new_owner: HostIdx,
    ) {
        // FIXME change return type to `Result`
        // FIXME we should store the timestamp of the last move in the current_ownership map, so
        // that we can use it in the cost model to weigh potential objects moves.
        // NOTE we can probably avoid locking the current ownership entry twice, as longa as we are
        // a bit careful on the reader's side.
        let current_ownership_range_clone = {
            let mut current_ownership = self.current_ownership.write();
            let current_ownership_range = match current_ownership.get(&target_object) {
                None => {
                    current_ownership
                        .insert(target_object, RwLock::new(VersionRange::new(old_owner)));
                    current_ownership.get(&target_object).unwrap()
                }
                Some(r) => r,
            };
            let mut current_ownership_range = current_ownership_range.write();
            current_ownership_range.end_version = whomstone_version;

            current_ownership_range.clone()
        };

        #[cfg(debug_assertions)]
        println!(
            "updating ownership of {}, whomstone version {}",
            target_object, whomstone_version
        );

        // if only we could upgrade a read lock to a write lock
        match {
            let past_ownership = self.past_ownership.read();
            match past_ownership.get(&target_object) {
                None => None,
                Some(e) => {
                    let mut version_ranges = e.write();
                    let last_range = version_ranges.last().unwrap();
                    let start_version = last_range.end_version + 1;
                    version_ranges.push(VersionRange {
                        host_idx: old_owner,
                        start_version,
                        end_version: whomstone_version,
                    });

                    Some(())
                }
            }
        } {
            None => {
                let mut ownership_range_vec = Vec::with_capacity(32);
                ownership_range_vec.push(current_ownership_range_clone);
                let mut past_ownership = self.past_ownership.write();
                past_ownership.insert(target_object, RwLock::new(ownership_range_vec));
            }
            _ => (),
        }

        let new_version = whomstone_version + 1;

        let current_ownership = self.current_ownership.read();
        let mut current_ownership_range = current_ownership.get(&target_object).unwrap().write();
        current_ownership_range.host_idx = new_owner;
        current_ownership_range.start_version = new_version;
        current_ownership_range.end_version = 0;
    }

    pub fn reset_state(&self) {
        {
            let mut past_ownership = self.past_ownership.write();
            past_ownership.clear();
        }

        {
            let mut current_ownership = self.current_ownership.write();
            current_ownership.clear();
        }
    }

    pub fn get_state_snapshot(&self) -> HashMap<ObjectId, HostIdx> {
        let current_ownership = self.current_ownership.read();
        current_ownership
            .iter()
            .map(|(object, version_range)| {
                let host_idx = {
                    let vr = version_range.read();
                    vr.host_idx
                };
                (*object, host_idx)
            })
            .collect()
    }
}
