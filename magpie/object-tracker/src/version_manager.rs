#![allow(dead_code)]
use std::cell::SyncUnsafeCell;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::sync::Arc;

use arrayvec::ArrayVec;
use dashmap::DashMap;
#[cfg(feature = "object-caching")]
use lazy_static::lazy_static;
use nando_support::ImageValue;
use object_lib::{IPtr, MaterializedObjectVersion, ObjectId, ObjectVersion};

#[cfg(feature = "object-caching")]
lazy_static! {
    static ref CACHING_READ_THRESHOLD: u8 = match std::env::var("CACHING_READ_THRESHOLD") {
        Err(_) => 64,
        Ok(e) => e.parse().unwrap(),
    };
}

struct TSCircularArray<T, const CAP: usize> {
    read_idx: AtomicUsize,
    write_idx: AtomicUsize,
    elems: SyncUnsafeCell<ArrayVec<T, CAP>>,
}

impl<T: Clone, const CAP: usize> TSCircularArray<T, CAP> {
    fn new() -> Self {
        Self {
            read_idx: AtomicUsize::new(0),
            write_idx: AtomicUsize::new(0),
            elems: SyncUnsafeCell::new(ArrayVec::new()),
        }
    }

    fn get_len(&self) -> usize {
        let elems = unsafe { &*self.elems.get() };
        elems.len()
    }

    fn push(&self, elem: T) {
        let elems = unsafe { &mut *self.elems.get() };
        let write_idx = self.write_idx.fetch_add(1, Ordering::Relaxed);
        let read_idx_tentative = self.read_idx.load(Ordering::Relaxed);
        if elems.remaining_capacity() > 0 {
            elems.push(elem);
        } else {
            elems[write_idx % CAP] = elem;
        }

        let _ = self.read_idx.compare_exchange(
            read_idx_tentative,
            write_idx,
            Ordering::Acquire,
            Ordering::Relaxed,
        );
    }

    fn get_latest(&self) -> Option<T> {
        let read_idx = self.read_idx.load(Ordering::Relaxed);
        if read_idx == 0 && self.write_idx.load(Ordering::Relaxed) == 0 {
            return None;
        }

        let elems = unsafe { &*self.elems.get() };
        Some(elems[read_idx % CAP].clone())
    }

    fn get_with_key<F>(&self, pred: F) -> Option<T>
    where
        F: Fn(&T) -> bool,
    {
        let elems = unsafe { &*self.elems.get() };
        for i in 0..CAP {
            let elem = elems[i].clone();
            if !(pred)(&elems[i]) {
                continue;
            }

            return Some(elem);
        }

        None
    }
}

pub(crate) struct VersionManager {
    object_version_map:
        DashMap<ObjectId, Arc<(TSCircularArray<Arc<MaterializedObjectVersion>, 5>, AtomicU8)>>,
}

impl VersionManager {
    pub fn new(initial_object_capacity: usize) -> Self {
        Self {
            object_version_map: DashMap::with_capacity(initial_object_capacity),
        }
    }

    // FIXME should return result type
    pub fn insert_initial_version(
        &self,
        object_id: ObjectId,
        initial_version: MaterializedObjectVersion,
    ) {
        let object_version_map = &self.object_version_map;
        if object_version_map.contains_key(&object_id) {
            object_version_map.entry(object_id).and_modify(|e| {
                e.0.push(Arc::new(initial_version));
            });

            return;
        }

        let version_cache = TSCircularArray::new();
        version_cache.push(Arc::new(initial_version));
        object_version_map.insert(object_id, Arc::new((version_cache, AtomicU8::default())));
    }

    pub fn get_latest(&self, object_id: ObjectId) -> Option<Arc<MaterializedObjectVersion>> {
        let object_version_map = &self.object_version_map;

        loop {
            let entries = match object_version_map.get(&object_id) {
                None => {
                    println!("No entry found for {object_id}");
                    return None;
                }
                Some(ref e) => Arc::clone(e),
            };

            match entries.0.get_latest() {
                None => return None,
                Some(ref lt) => {
                    let current_value = entries.1.load(Ordering::Relaxed);
                    if current_value < u8::MAX {
                        match entries.1.compare_exchange(
                            current_value,
                            current_value + 1,
                            Ordering::Acquire,
                            Ordering::Relaxed,
                        ) {
                            Ok(_current_value) => return Some(Arc::clone(lt)),
                            Err(_other_value) => { /* iterate */ }
                        }
                    } else {
                        return Some(Arc::clone(lt));
                    }
                }
            }
        }
    }

    // attempt to get the object at the supplied version. Used for self-consistent read sets.
    pub fn get_at(
        &self,
        object_id: ObjectId,
        target_version: ObjectVersion,
    ) -> Option<Arc<MaterializedObjectVersion>> {
        let object_version_map = &self.object_version_map;
        let entries = match object_version_map.get(&object_id) {
            None => return None,
            Some(ref e) => Arc::clone(e),
        };

        entries
            .0
            .get_with_key(|e| e.get_version() == target_version)
    }

    pub fn push_version(
        &self,
        object_id: ObjectId,
        object_version: ObjectVersion,
        changes: &Vec<(IPtr, &ImageValue)>,
    ) {
        let object_version_map = &self.object_version_map;
        if !object_version_map.contains_key(&object_id) {
            return;
        }

        let entries = object_version_map.get(&object_id).unwrap().clone();

        // reset access history of "latest" version to 0
        entries.1.store(0, Ordering::Relaxed);

        let latest_entry_clone = entries.0.get_latest().unwrap().clone();

        let mut latest_entry = MaterializedObjectVersion::from_version(&latest_entry_clone);
        latest_entry.apply_changes(object_version, changes);

        entries.0.push(Arc::new(latest_entry));
    }

    #[cfg(feature = "object-caching")]
    pub fn object_is_cacheable(&self, object_id: ObjectId) -> (bool, Option<ObjectVersion>) {
        let object_version_map = &self.object_version_map;
        if !object_version_map.contains_key(&object_id) {
            return (false, None);
        }

        let entries = object_version_map.get(&object_id).unwrap().clone();
        let read_counter = entries.1.load(Ordering::Relaxed);

        if read_counter < *CACHING_READ_THRESHOLD {
            return (false, None);
        }

        (
            true,
            Some(
                entries
                    .0
                    .get_latest()
                    .expect("could not get latest version")
                    .get_version(),
            ),
        )
    }

    pub fn delete_object(&self, object_to_delete: ObjectId) {
        // NOTE this is safe to do without any checks, as readers who already have a reference will
        // not be affected,
        self.object_version_map.remove(&object_to_delete);
    }
}
