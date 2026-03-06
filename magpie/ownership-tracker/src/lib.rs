use std::collections::HashMap;
use std::mem::MaybeUninit;
use std::sync::{Arc, Once};

use dashmap::DashMap;
use nando_support::{activation_intent::NandoArgument, ecb_id::EcbId, epic_control, HostIdx};
use object_lib::{IPtr, ObjectId, ObjectVersion};
#[cfg(not(feature = "offline"))]
use ownership_support::WorkerMapping;
use ownership_support::{self, Hostname};
use parking_lot::{Mutex, RwLock};
#[cfg(not(feature = "offline"))]
use reqwest;
#[cfg(feature = "telemetry")]
use telemetry;
pub use tls as ownership_tracker_tls;

pub mod config;
pub mod error;
pub mod tls;

pub type HostId = String;

#[derive(Clone)]
pub enum OwnershipStatus {
    Strong,
    UnderMigration,
    Whomstoned(ObjectVersion),
    Incoming(ObjectVersion),
    Migrated(ObjectVersion),
}

#[derive(Debug)]
pub enum Schedulable {
    Immediately,
    AfterWaitingForArrival(Vec<ObjectId>),
    ArgsUnresolvableLocally,
}

#[derive(Debug)]
pub enum ObjectCacheEntry {
    Placeholder(Option<ObjectId>, ObjectVersion),
    Incoming(ObjectId, ObjectVersion),
    Cached(ObjectId, ObjectVersion),
    Invalidated(ObjectId, ObjectVersion),
}

#[allow(dead_code)]
impl ObjectCacheEntry {
    fn is_placeholder(&self) -> bool {
        match self {
            Self::Placeholder(_, _) => true,
            _ => false,
        }
    }

    fn get_version(&self) -> ObjectVersion {
        match self {
            Self::Placeholder(_, v) => *v,
            Self::Incoming(_, v) => *v,
            Self::Cached(_, v) => *v,
            Self::Invalidated(_, v) => *v,
        }
    }
}

pub enum CacheMappingEntry {
    Owned(HostId),
    Invalidated(HostId),
}

pub struct OwnershipTracker {
    #[allow(dead_code)]
    config: config::Config,
    // We need to track two states
    //  - our ownership state (hard state, source of truth) -- this should probably be a map whose
    //  values represent the current ownership state ('strong', 'under migration', etc)
    // second value in tuple tells us if published or not
    owned_objects: DashMap<ObjectId, Arc<RwLock<(OwnershipStatus, bool)>>>,

    // Contains a mapping from control block IDs to hosts that own them. This is used for discovery
    // of i) dependent tasks (for status/result propagation) and ii) parent tasks (for
    // status/completion/result propagation when a subtree finishes execution).
    control_block_ownership_map: DashMap<EcbId, HostId>,

    // Contains a mapping from "original" object IDs to potentially locally cached versions of that
    // object. Used when determining if a read-only transaction can be executed locally despite not
    // holding ownership of all object dependencies.
    owned_cache_map: DashMap<ObjectId, ObjectCacheEntry>,

    // Contains a mapping from IDs of the cached versions of objects to the hosts that they were
    // sent to (for local caching). This facilitates cache discovery during invalidation.
    // FIXME This should be merged into the ownership map below
    object_to_shared_cache_map: DashMap<ObjectId, Vec<(ObjectId, ObjectVersion)>>,
    shared_cache_map: DashMap<ObjectId, CacheMappingEntry>,

    //  - the set of owners as it's been shared with us by the scheduler (cache)
    ownership_map: DashMap<ObjectId, HostId>,
    #[cfg(not(feature = "offline"))]
    scheduler_client: Arc<reqwest::Client>,
    host_idx: Arc<Mutex<Option<u64>>>,

    host_mapping: RwLock<HashMap<HostIdx, Hostname>>,

    #[cfg(feature = "telemetry")]
    telemetry_handle: Option<telemetry::TelemetryEventSender>,
}

impl OwnershipTracker {
    // TODO include scheduler endpoint to config.
    pub fn new(config: &config::Config) -> Self {
        Self {
            config: config.clone(),
            // owned_objects: RwLock::new(HashSet::new()),
            // owned_objects: RwLock::new(HashMap::new()),
            owned_objects: DashMap::new(),
            ownership_map: DashMap::new(),
            #[cfg(not(feature = "offline"))]
            scheduler_client: Arc::new(reqwest::Client::new()),
            host_idx: Arc::new(Mutex::new(None)),
            control_block_ownership_map: DashMap::new(),
            owned_cache_map: DashMap::new(),
            shared_cache_map: DashMap::new(),
            object_to_shared_cache_map: DashMap::new(),
            host_mapping: RwLock::new(HashMap::new()),

            #[cfg(feature = "telemetry")]
            telemetry_handle: None,
        }
    }

    fn get_ownership_tracker_mut(
        maybe_config: Option<&config::Config>,
    ) -> &'static mut OwnershipTracker {
        static mut INSTANCE: MaybeUninit<OwnershipTracker> = MaybeUninit::uninit();
        static mut ONCE: Once = Once::new();

        unsafe {
            ONCE.call_once(|| {
                let config = maybe_config
                    .expect("cannot instantiate ownerhip tracker without a valid config");
                INSTANCE.as_mut_ptr().write(OwnershipTracker::new(config));
            });
        }

        unsafe { &mut *INSTANCE.as_mut_ptr() }
    }

    pub fn get_ownership_tracker(
        maybe_config: Option<&config::Config>,
    ) -> &'static OwnershipTracker {
        let ownership_tracker = Self::get_ownership_tracker_mut(maybe_config);
        ownership_tracker
    }

    #[cfg(feature = "telemetry")]
    pub fn initialize_telemetry_handle() {
        let ownership_tracker = Self::get_ownership_tracker_mut(None);
        ownership_tracker.telemetry_handle = Some(telemetry::get_telemetry_handle());
    }

    #[cfg(feature = "telemetry")]
    #[allow(dead_code)]
    #[inline(always)]
    fn submit_telemetry_event(&self, event: telemetry::TelemetryEvent) {
        match &self.telemetry_handle {
            None => {}
            Some(handle) => telemetry::submit_telemetry_event(handle, event),
        }
    }

    #[cfg(not(feature = "offline"))]
    pub fn get_ordered_host_list(&self) -> Vec<HostIdx> {
        let host_mapping = self.host_mapping.read();
        let mut res: Vec<_> = host_mapping.keys().map(|idx| *idx).collect();
        res.sort();

        res
    }

    #[cfg(feature = "offline")]
    pub fn get_ordered_host_list(&self) -> Vec<HostIdx> {
        vec![0]
    }

    pub fn get_host_idx(&self) -> Option<u64> {
        *self.host_idx.lock()
    }

    pub fn get_own_host_id(&self) -> Option<HostId> {
        let own_idx = { *self.host_idx.lock() };
        let Some(own_idx) = own_idx else {
            return None;
        };

        let host_mapping = self.host_mapping.read();
        host_mapping.get(&own_idx).cloned()
    }

    pub fn get_host_id_for_idx(&self, host_idx: HostIdx) -> Option<HostId> {
        let host_mapping = self.host_mapping.read();
        host_mapping.get(&host_idx).cloned()
    }

    pub fn get_num_hosts(&self) -> usize {
        self.host_mapping.read().len()
    }

    pub fn mark_owned(&self, object_id: ObjectId) -> bool {
        let entry = match self.owned_objects.get(&object_id) {
            None => {
                self.owned_objects.insert(
                    object_id,
                    Arc::new(RwLock::new((OwnershipStatus::Strong, false))),
                );
                return true;
            }
            Some(e) => Arc::clone(&e),
        };

        let mut entry = entry.write();
        entry.0 = OwnershipStatus::Strong;

        return true;
    }

    pub fn mark_under_migration(&self, object_id: ObjectId) {
        let entry = match self.owned_objects.get(&object_id) {
            None => {
                self.owned_objects.insert(
                    object_id,
                    Arc::new(RwLock::new((OwnershipStatus::UnderMigration, false))),
                );
                return;
            }
            Some(e) => Arc::clone(&e),
        };

        let mut entry = entry.write();
        entry.0 = OwnershipStatus::UnderMigration;
    }

    pub fn mark_whomstoned(&self, object_id: ObjectId, whomstone_version: ObjectVersion) {
        let entry = match self.owned_objects.get(&object_id) {
            None => {
                self.owned_objects.insert(
                    object_id,
                    Arc::new(RwLock::new((
                        OwnershipStatus::Whomstoned(whomstone_version),
                        false,
                    ))),
                );
                return;
            }
            Some(e) => Arc::clone(&e),
        };

        let mut entry = entry.write();
        entry.0 = OwnershipStatus::Whomstoned(whomstone_version);
    }

    pub fn mark_incoming(&self, object_id: ObjectId, first_version: ObjectVersion) {
        let entry = match self.owned_objects.get(&object_id) {
            None => {
                self.owned_objects.insert(
                    object_id,
                    Arc::new(RwLock::new((
                        OwnershipStatus::Incoming(first_version),
                        false,
                    ))),
                );
                return;
            }
            Some(e) => Arc::clone(&e),
        };

        let mut entry = entry.write();
        entry.0 = OwnershipStatus::Incoming(first_version);
    }

    pub fn mark_migrated(&self, object_id: ObjectId) {
        self.owned_objects.entry(object_id).and_modify(|e| {
            let mut e = e.write();
            match e.0 {
                OwnershipStatus::Whomstoned(v) => e.0 = OwnershipStatus::Migrated(v),
                // TODO anything else should be some kind of error.
                _ => (),
            }
        });
    }

    #[allow(dead_code)]
    fn get_object_ownership_status(&self, object_id: ObjectId) -> Option<OwnershipStatus> {
        match self.owned_objects.get(&object_id) {
            Some(ref os) => Some(os.read().0.clone()),
            _ => None,
        }
    }

    pub fn object_is_owned(&self, object_id: ObjectId) -> bool {
        match self.owned_objects.get(&object_id) {
            Some(ref os) => match os.read().0 {
                OwnershipStatus::Strong => true,
                _ => false,
            },
            _ => false,
        }
    }

    pub fn objects_are_owned(&self, object_ids: &Vec<ObjectId>) -> bool {
        object_ids.iter().all(|o| self.object_is_owned(*o))
    }

    pub fn object_is_incoming(&self, object_id: ObjectId) -> bool {
        match self.owned_objects.get(&object_id) {
            Some(ref os) => match os.read().0 {
                OwnershipStatus::Incoming(_) => true,
                _ => false,
            },
            _ => false,
        }
    }

    pub fn insert_mapping_in_ownership_map(&self, object_id: ObjectId, owning_host: HostId) {
        self.ownership_map.insert(object_id, owning_host);
    }

    pub fn insert_mapping_in_ownership_map_if_not_present(
        &self,
        object_id: ObjectId,
        owning_host: HostId,
    ) {
        if self.ownership_map.contains_key(&object_id) {
            return;
        }
        self.insert_mapping_in_ownership_map(object_id, owning_host);
    }

    pub fn compute_activation_site(&self, object_dependencies: &Vec<ObjectId>) -> Option<HostId> {
        if object_dependencies.len() == 1 {
            match self.ownership_map.get(object_dependencies.get(0).unwrap()) {
                None => return None,
                Some(ref e) => return Some(e.value().clone()),
            }
        }

        let mut maybe_activation_site = None;
        for object_dependency in object_dependencies {
            match self.ownership_map.get(&object_dependency) {
                None => return None,
                Some(ref site) => match maybe_activation_site {
                    Some(ref s) => {
                        if s != site.value() {
                            return None;
                        }
                    }
                    None => maybe_activation_site = Some(site.value().clone()),
                },
            }
        }

        maybe_activation_site
    }

    pub async fn register_worker(
        &self,
        hostname: HostId,
        host_idx: Option<u64>,
    ) -> Result<u64, String> {
        #[cfg(not(feature = "offline"))]
        let resp_host_idx = {
            let registration_request = ownership_support::RegisterWorkerRequest {
                host_idx,
                hostname: hostname.clone(),
            };
            let client = self.scheduler_client.clone();

            let resp = client
                .post(self.get_scheduler_url("register_worker"))
                .json(&registration_request)
                .send()
                .await
                .expect("failed to register worker");
            assert!(resp.status().is_success());

            let parsed_response = resp
                .json::<ownership_support::RegisterWorkerResponse>()
                .await
                .expect("failed to parse worker registration response from scheduler");

            parsed_response.host_idx
        };

        #[cfg(feature = "offline")]
        let resp_host_idx =
            { host_idx.expect("cannot launch offline instance without a dummy host index") };

        {
            let mut host_idx = self.host_idx.lock();
            *host_idx = Some(resp_host_idx);
            Self::get_host_idx_static(Some(resp_host_idx));
        }

        self.update_host_mapping(resp_host_idx, hostname);

        #[cfg(debug_assertions)]
        println!("[DEBUG] Registered as host {}", resp_host_idx);

        Ok(resp_host_idx)
    }

    /// Used for cheap retrieval of the current registered host's idx, to be used when constructing
    /// new ECBs (for unique ECB ID creation).
    pub fn get_host_idx_static(init_idx: Option<u64>) -> Option<u64> {
        static mut HOST_IDX: MaybeUninit<u64> = MaybeUninit::uninit();
        static mut ONCE: Once = Once::new();

        unsafe {
            ONCE.call_once(|| {
                let host_idx = init_idx.expect("cannot initialize host idx without an init value");
                HOST_IDX.as_mut_ptr().write(host_idx);
            });
        }

        Some(unsafe { HOST_IDX.assume_init() })
    }

    // FIXME return type
    pub async fn publish_object(&self, _iptr: &IPtr) {
        #[cfg(not(feature = "offline"))]
        {
            let iptr = _iptr;

            let client = self.scheduler_client.clone();
            let publish_request = ownership_support::PublishRequest {
                host_idx: self.get_host_idx().unwrap(),
                object: *iptr,
            };
            let response = client
                .post(self.get_scheduler_url("publish_object"))
                .json(&publish_request)
                .send()
                .await
                .expect(&format!("failed to publish object {}", iptr.object_id));
            assert!(response.status().is_success());

            let entry = match self.owned_objects.get(&iptr.object_id) {
                None => return,
                Some(e) => Arc::clone(&e),
            };
            let mut entry = entry.write();
            entry.1 = true;
        }
    }

    pub async fn publish_objects(&self, _iptrs: &Vec<IPtr>) {
        #[cfg(not(feature = "offline"))]
        {
            let iptrs = _iptrs;

            let client = self.scheduler_client.clone();
            let publish_request = ownership_support::MultiPublishRequest {
                host_idx: self.get_host_idx().unwrap(),
                objects: iptrs.clone(),
            };
            let response = client
                .post(self.get_scheduler_url("publish_objects"))
                .json(&publish_request)
                .send()
                .await
                .expect(&format!("failed to publish objects"));
            assert!(response.status().is_success());

            for iptr in iptrs {
                let entry = match self.owned_objects.get(&iptr.object_id) {
                    None => continue,
                    Some(e) => Arc::clone(&e),
                };
                let mut entry = entry.write();
                entry.1 = true;
            }
        }
    }

    pub async fn object_is_published(&self, object_id: ObjectId) -> bool {
        let entry = match self.owned_objects.get(&object_id) {
            None => panic!("cannot get published status of unowned object"),
            Some(e) => Arc::clone(&e),
        };

        let entry = entry.read();
        entry.1
    }

    #[cfg(not(feature = "offline"))]
    #[inline]
    fn get_scheduler_url(&self, endpoint: &str) -> String {
        format!(
            "http://{}:{}/{}",
            self.config.scheduler_config.host, self.config.scheduler_config.port, endpoint
        )
    }

    pub fn get_remote_owner(&self, object_id: ObjectId) -> Option<HostId> {
        match self.ownership_map.get(&object_id) {
            None => None,
            Some(e) => Some(e.value().clone()),
        }
    }

    pub fn get_non_owners(&self, object_id: ObjectId) -> Vec<HostId> {
        let owner = match self.object_is_owned(object_id) {
            true => self.get_own_host_id().unwrap(),
            false => self.get_remote_owner(object_id).unwrap(),
        };

        let host_mapping = self.host_mapping.read();
        host_mapping
            .values()
            .filter(|h| *h != &owner)
            .map(|h| h.clone())
            .collect()
    }

    pub fn get_remote_owner_with_idx(&self, object_id: ObjectId) -> (HostIdx, HostId) {
        let remote_owner = self.get_remote_owner(object_id).unwrap();
        let host_mapping = self.host_mapping.read();
        let remote_owner_idx = host_mapping
            .iter()
            .find_map(|(host_idx, host_id)| {
                if *host_id == remote_owner {
                    return Some(*host_idx);
                }

                None
            })
            .expect("no idx for host");

        (remote_owner_idx, remote_owner)
    }

    // FIXME @speed
    pub fn get_host_idx_for_id(&self, host_id: &HostId) -> Option<HostIdx> {
        let host_mapping = self.host_mapping.read();
        for (host_idx, stored_host_id) in host_mapping.iter() {
            if stored_host_id == host_id {
                return Some(*host_idx);
            }
        }

        None
    }

    pub fn insert_control_block_entry(&self, ecb_id: EcbId, owning_host_idx: HostIdx) {
        let owning_host = {
            let host_mapping = self.host_mapping.read();
            host_mapping
                .get(&owning_host_idx)
                .expect(&format!(
                    "failed to get hostname of host {}",
                    owning_host_idx
                ))
                .clone()
        };

        self.control_block_ownership_map
            .entry(ecb_id)
            .and_modify(|o| *o = owning_host.clone())
            .or_insert(owning_host);
    }

    pub fn insert_control_block_entries_with_id(
        &self,
        ecb_ids: &Vec<EcbId>,
        owning_host_id: HostId,
    ) {
        for ecb_id in ecb_ids.iter() {
            match self.control_block_ownership_map.get_mut(ecb_id) {
                Some(mut o) => *o = owning_host_id.clone(),
                None => {
                    self.control_block_ownership_map
                        .insert(*ecb_id, owning_host_id.clone());
                }
            }
        }
    }

    pub fn remove_control_block_entry(&self, ecb_id: EcbId) -> Option<(EcbId, HostId)> {
        self.control_block_ownership_map.remove(&ecb_id)
    }

    pub fn get_control_block_entry(&self, ecb_id: EcbId) -> Option<HostId> {
        match self.control_block_ownership_map.get(&ecb_id) {
            Some(e) => Some(e.value().clone()),
            None => None,
        }
    }

    pub fn get_cache_mapping_if_valid(&self, source_object_id: ObjectId) -> Option<ObjectId> {
        match self.owned_cache_map.get(&source_object_id) {
            None => None,
            Some(e) => match e.value() {
                ObjectCacheEntry::Cached(oid, _) => Some(*oid),
                _ => None,
            },
        }
    }

    // FIXME @hack return values for version are whatever
    pub fn was_cached(&self, source_object_id: ObjectId) -> Option<(ObjectId, ObjectVersion)> {
        match self.owned_cache_map.get(&source_object_id) {
            None => None,
            Some(entry) => match entry.value() {
                ObjectCacheEntry::Incoming(oid, v) => Some((*oid, *v)),
                ObjectCacheEntry::Cached(oid, v) => Some((*oid, *v)),
                ObjectCacheEntry::Invalidated(oid, v) => Some((*oid, *v + 1)),
                _ => None,
            },
        }
    }

    pub fn get_cache_mapping_if_incoming(&self, source_object_id: ObjectId) -> Option<ObjectId> {
        match self.owned_cache_map.get(&source_object_id) {
            None => None,
            Some(e) => match e.value() {
                ObjectCacheEntry::Incoming(oid, _) => Some(*oid),
                _ => None,
            },
        }
    }

    pub fn mark_incoming_if_valid(
        &self,
        original_object_id: ObjectId,
        cacheable_version: ObjectVersion,
    ) -> (bool, bool, Option<ObjectId>) {
        let mut present = false;
        let mut up_to_date = false;
        let mut previous_cache_id = None;

        self.owned_cache_map
            .entry(original_object_id)
            .and_modify(|e| {
                present = true;
                match e {
                    ObjectCacheEntry::Placeholder(maybe_placeholder_id, cached_version) => {
                        up_to_date = *cached_version >= cacheable_version;
                        previous_cache_id = *maybe_placeholder_id;
                    }
                    ObjectCacheEntry::Cached(oid, cached_version) => {
                        up_to_date = *cached_version >= cacheable_version;
                        previous_cache_id = Some(*oid);
                    }
                    ObjectCacheEntry::Invalidated(oid, invalidated_version) => {
                        // If a future version was invalidated, we consider our entry "up to date"
                        // in the sense that the present query is being evaluated for a past version
                        // that we should _not_ cache locally.
                        up_to_date = *invalidated_version >= cacheable_version;
                        previous_cache_id = Some(*oid);
                    }
                    ObjectCacheEntry::Incoming(_, incoming_version) => {
                        up_to_date = *incoming_version >= cacheable_version;
                    }
                }

                if !up_to_date {
                    *e = ObjectCacheEntry::Placeholder(previous_cache_id, cacheable_version);
                }
            })
            .or_insert_with(|| ObjectCacheEntry::Placeholder(None, cacheable_version));

        (present, up_to_date, previous_cache_id)
    }

    pub fn add_incoming_cache_mapping(
        &self,
        original_object_id: ObjectId,
        cached_version_object_id: ObjectId,
        cached_version_object_version: ObjectVersion,
    ) -> bool {
        if !self.owned_cache_map.contains_key(&original_object_id) {
            #[cfg(debug_assertions)]
            println!(
                "add_incoming_cache_mapping: Could not find {} / {} at {}",
                original_object_id, cached_version_object_id, cached_version_object_version
            );
            return false;
        }

        let mut inserted = false;
        self.owned_cache_map
            .entry(original_object_id)
            .and_modify(|e| {
                let mut should_update = true;
                match e {
                    ObjectCacheEntry::Placeholder(_maybe_placeholder_id, placeholder_version) => {
                        #[cfg(debug_assertions)]
                        println!("add_incoming_cache_mapping: Found placeholder for {} / {} at {}, placeholder version {}", original_object_id, cached_version_object_id, cached_version_object_version, placeholder_version);
                        if cached_version_object_version < *placeholder_version {
                            should_update = false;
                        }
                    }
                    ObjectCacheEntry::Cached(_, _cached_version) => {
                        #[cfg(debug_assertions)]
                        println!("add_incoming_cache_mapping: Found cached for {} / {} at {}, cached version {}", original_object_id, cached_version_object_id, cached_version_object_version, _cached_version);
                        should_update = false;
                    }
                    ObjectCacheEntry::Incoming(_, oversion) => {
                        #[cfg(debug_assertions)]
                        println!("add_incoming_cache_mapping: Found incoming for {} / {} at {}, incoming version {}", original_object_id, cached_version_object_id, cached_version_object_version, oversion);
                        if cached_version_object_version <= *oversion {
                            should_update = false;
                        }
                    }
                    ObjectCacheEntry::Invalidated(_, oversion) => {
                        #[cfg(debug_assertions)]
                        println!("add_incoming_cache_mapping: Found invalidated for {} / {} at {}, invalidated version {}", original_object_id, cached_version_object_id, cached_version_object_version, oversion);
                        if cached_version_object_version <= *oversion {
                            // If we already got an invalidation message for this version (or a
                            // more recent one), we should do nothing.
                            should_update = false;
                        }
                    }
                }

                if should_update {
                    #[cfg(debug_assertions)]
                    println!("add_incoming_cache_mapping: will update for {} / {} at {}", original_object_id, cached_version_object_id, cached_version_object_version);
                    inserted = true;
                    *e = ObjectCacheEntry::Incoming(
                        cached_version_object_id,
                        cached_version_object_version,
                    );
                }
            });

        inserted
    }

    pub fn mark_invalidated(
        &self,
        original_object_id: ObjectId,
        cached_object_id: ObjectId,
        invalidation_version: ObjectVersion,
    ) {
        self.owned_cache_map
            .entry(original_object_id)
            .and_modify(|e| {
                let mut should_mark_invalidated = true;
                let mut previous_cache_id = None;

                match e {
                    ObjectCacheEntry::Invalidated(_, oversion) => {
                        should_mark_invalidated = false;

                        if *oversion < invalidation_version {
                            *oversion = invalidation_version;
                        }
                    }
                    ObjectCacheEntry::Incoming(oid, incoming_version) => {
                        previous_cache_id = Some(*oid);

                        if *incoming_version > invalidation_version {
                            should_mark_invalidated = false;
                        }
                    }
                    ObjectCacheEntry::Placeholder(oid, placeholder_version) => {
                        if *placeholder_version > invalidation_version {
                            should_mark_invalidated = false;
                        }

                        if oid.is_none() {
                            previous_cache_id = Some(cached_object_id);
                        } else {
                            previous_cache_id = *oid;
                        }
                    }
                    ObjectCacheEntry::Cached(oid, cached_version) => {
                        previous_cache_id = Some(*oid);

                        if *cached_version > invalidation_version {
                            should_mark_invalidated = false;
                        }
                    }
                }

                if should_mark_invalidated {
                    *e = ObjectCacheEntry::Invalidated(
                        previous_cache_id.unwrap(),
                        invalidation_version,
                    );
                }
            });
    }

    pub fn add_owned_cache_mapping(
        &self,
        original_object_id: ObjectId,
        cached_version_object_id: ObjectId,
        cached_version_object_version: ObjectVersion,
    ) -> bool {
        if !self.owned_cache_map.contains_key(&original_object_id) {
            return false;
        }

        let mut marked_owned = false;
        self.owned_cache_map
            .entry(original_object_id)
            .and_modify(|e| {
                let mut should_mark_owned = true;

                match e {
                    ObjectCacheEntry::Placeholder(_, placeholder_version) => {
                        if *placeholder_version > cached_version_object_version {
                            should_mark_owned = false;
                        }
                    }
                    ObjectCacheEntry::Cached(oid, oversion) => {
                        if *oid == cached_version_object_id
                            && cached_version_object_version <= *oversion
                        {
                            // Don't do anything if we have already cached a more recent version.
                            should_mark_owned = false;
                        }

                        if *oid != cached_version_object_id
                            && cached_version_object_version <= *oversion
                        {
                            // Don't do anything if we have already cached a more recent version,
                            // even if the cached object's id is different to the new one.
                            should_mark_owned = false;
                        }
                    }
                    ObjectCacheEntry::Incoming(_, oversion) => {
                        if cached_version_object_version < *oversion {
                            should_mark_owned = false;
                        }
                    }
                    ObjectCacheEntry::Invalidated(_, oversion) => {
                        if cached_version_object_version <= *oversion {
                            // If we already got an invalidation message for this version (or a
                            // more recent one), we should do nothing.
                            should_mark_owned = false;
                        }
                    }
                }

                if should_mark_owned {
                    marked_owned = true;
                    *e = ObjectCacheEntry::Cached(
                        cached_version_object_id,
                        cached_version_object_version,
                    );
                }
            });

        marked_owned
    }

    pub fn remove_owned_cache_mapping(
        &self,
        original_object_id: ObjectId,
    ) -> Option<(ObjectId, ObjectCacheEntry)> {
        self.owned_cache_map.remove(&original_object_id)
    }

    pub fn invalidate_or_remove_shared_cache_entry(
        &self,
        original_object_id: ObjectId,
        cached_version_object_id: ObjectId,
    ) -> Option<(ObjectId, HostId)> {
        let mut should_remove = false;
        self.shared_cache_map
            .entry(cached_version_object_id)
            .and_modify(|e| match e {
                CacheMappingEntry::Owned(_) => should_remove = true,
                _ => {}
            });

        if !should_remove {
            return None;
        }

        self.object_to_shared_cache_map
            .entry(original_object_id)
            .and_modify(|e| {
                match e.iter().position(|(e, _)| *e == cached_version_object_id) {
                    None => {}
                    Some(p) => {
                        e.swap_remove(p);
                    }
                };
            });

        let cache_entry = self.shared_cache_map.remove(&cached_version_object_id);
        match cache_entry {
            None => panic!("should not happen"),
            Some((object_id, CacheMappingEntry::Owned(host_id))) => Some((object_id, host_id)),
            _ => panic!("should not happen"),
        }
    }

    pub fn add_shared_cache_entry(
        &self,
        original_object_id: ObjectId,
        cached_version_object_id: ObjectId,
        cache_version: ObjectVersion,
        owning_host: HostIdx,
    ) -> Result<(), ()> {
        let owning_host = {
            let host_mapping = self.host_mapping.read();
            host_mapping
                .get(&owning_host)
                .expect(&format!(
                    "failed to get owning host with idx {}",
                    owning_host
                ))
                .clone()
        };

        let mut already_invalidated = false;
        self.shared_cache_map
            .entry(cached_version_object_id)
            .and_modify(|e| match e {
                CacheMappingEntry::Invalidated(_) => already_invalidated = true,
                _ => {}
            })
            .or_insert(CacheMappingEntry::Owned(owning_host));

        if already_invalidated {
            return Err(());
        }

        self.object_to_shared_cache_map
            .entry(original_object_id)
            .and_modify(|e| {
                if e.iter()
                    .position(|(c, _)| *c == cached_version_object_id)
                    .is_some()
                {
                    return;
                }

                e.push((cached_version_object_id, cache_version));
            })
            .or_insert(vec![(cached_version_object_id, cache_version)]);

        Ok(())
    }

    pub fn get_shared_caches_for_object(
        &self,
        object_id: ObjectId,
    ) -> Vec<(ObjectId, ObjectVersion)> {
        match self.object_to_shared_cache_map.get(&object_id) {
            None => vec![],
            Some(e) => e.clone(),
        }
    }

    pub fn get_owner_of_shared_cache(&self, cache_object_id: ObjectId) -> Option<HostId> {
        match self.shared_cache_map.get(&cache_object_id) {
            Some(e) => match e.value() {
                CacheMappingEntry::Owned(host) => Some(host.clone()),
                _ => None,
            },
            _ => None,
        }
    }

    pub fn get_last_owner_of_shared_cache(&self, cache_object_id: ObjectId) -> Option<HostId> {
        match self.shared_cache_map.get(&cache_object_id) {
            Some(e) => match e.value() {
                CacheMappingEntry::Owned(host) => Some(host.clone()),
                CacheMappingEntry::Invalidated(host) => Some(host.clone()),
            },
            _ => None,
        }
    }

    pub fn get_cache_id_for_object_host_pair(
        &self,
        object_id: ObjectId,
        host: HostId,
    ) -> Option<ObjectId> {
        match self.object_to_shared_cache_map.get(&object_id) {
            None => {}
            Some(caches) => {
                for cache in caches.value() {
                    let cache_id = cache.0;
                    match self.shared_cache_map.get(&cache_id) {
                        Some(e) => match e.value() {
                            CacheMappingEntry::Owned(owning_host)
                            | CacheMappingEntry::Invalidated(owning_host) => {
                                if owning_host != &host {
                                    continue;
                                }

                                return Some(cache_id);
                            }
                        },
                        None => continue,
                    }
                }
            }
        }

        None
    }

    pub fn dependency_is_local(
        &self,
        _dependency: &epic_control::DownstreamTaskDependency,
    ) -> bool {
        #[cfg(not(feature = "offline"))]
        {
            let dependency = _dependency;
            let dependency_ecb_id = dependency.get_inner_ecb_id().unwrap();
            self.control_block_is_local(&dependency_ecb_id)
        }

        #[cfg(feature = "offline")]
        true
    }

    pub fn control_block_is_local(&self, ecb_id: &EcbId) -> bool {
        !self.control_block_ownership_map.contains_key(ecb_id)
    }

    pub fn get_in_flight_args(&self, args: &Vec<NandoArgument>) -> Vec<ObjectId> {
        let mut res = Vec::with_capacity(args.len());

        for arg in args {
            match arg {
                NandoArgument::Ref(dependency) => {
                    let dependency_object_id = dependency.get_object_id();
                    if self.object_is_incoming(dependency_object_id) {
                        res.push(dependency_object_id);
                    }
                }
                NandoArgument::MRef(ref dependencies) => {
                    res.extend(
                        dependencies
                            .get_inner()
                            .iter()
                            .filter(|d| {
                                let dependency_object_id = d.get_object_id();
                                self.object_is_incoming(dependency_object_id)
                            })
                            .map(|d| d.get_object_id()),
                    );
                }
                _ => continue,
            }
        }

        res
    }

    // FIXME this is too similar to `ActivationRouter::can_execute_locally()`, merge them?
    pub fn args_are_resolvable_locally(
        &self,
        _is_read_only: bool,
        _mutable_argument_indices: Option<&[usize]>,
        _intent_name: Option<&str>,
        _args: &mut Vec<NandoArgument>,
    ) -> Schedulable {
        #[cfg(not(feature = "offline"))]
        {
            let is_read_only = _is_read_only;
            let args = _args;
            let mut cache_rewrites = Vec::with_capacity(args.len());
            let mutable_argument_indices = match _mutable_argument_indices {
                Some(mai) => mai,
                _ => &[],
            };

            let mut args_to_wait_for = Vec::with_capacity(args.len());

            for (idx, arg) in args.iter().enumerate() {
                match arg {
                    NandoArgument::Ref(dependency) => {
                        let dependency = if !is_read_only && mutable_argument_indices.contains(&idx)
                        {
                            dependency.object_id
                        } else {
                            match self.get_cache_mapping_if_valid(dependency.object_id) {
                                None => dependency.object_id,
                                Some(c) => {
                                    cache_rewrites
                                        .push((idx, NandoArgument::Ref(IPtr::new(c, 0, 0))));
                                    c
                                }
                            }
                        };

                        match self.get_object_ownership_status(dependency) {
                            Some(OwnershipStatus::Strong) => {
                                #[cfg(debug_assertions)]
                                println!("[DEBUG] Object {} is owned", dependency);
                                continue;
                            }
                            Some(OwnershipStatus::Incoming(_)) => {
                                #[cfg(debug_assertions)]
                                println!(
                                    "[DEBUG] Object {} is incoming should wait somehow",
                                    dependency
                                );
                                args_to_wait_for.push(dependency);

                                continue;
                            }
                            _ => {}
                        }

                        #[cfg(feature = "telemetry")]
                        {
                            let intent_name = match _intent_name {
                                Some(n) => n,
                                None => "",
                            };
                            let telemetry_ts = telemetry::zoned_timestamp_now();
                            self.submit_telemetry_event(
                                telemetry::TelemetryEvent::new_unresolvable_arg(
                                    intent_name,
                                    idx,
                                    telemetry_ts,
                                ),
                            );
                        }

                        return Schedulable::ArgsUnresolvableLocally;
                    }
                    NandoArgument::MRef(ref dependencies) => {
                        let dependencies = if !is_read_only
                            && mutable_argument_indices.contains(&idx)
                        {
                            dependencies
                                .get_inner()
                                .iter()
                                .map(|dependency| dependency.object_id)
                                .collect()
                        } else {
                            dependencies
                                .get_inner()
                                .iter()
                                .map(|dependency| {
                                    match self.get_cache_mapping_if_valid(dependency.object_id) {
                                        None => dependency.object_id,
                                        Some(c) => {
                                            cache_rewrites.push((
                                                idx,
                                                NandoArgument::Ref(IPtr::new(c, 0, 0)),
                                            ));
                                            c
                                        }
                                    }
                                })
                                .collect()
                        };

                        #[cfg(debug_assertions)]
                        println!("Will inspect dependencies {:#?} of mref", dependencies);

                        if self.objects_are_owned(&dependencies) {
                            #[cfg(debug_assertions)]
                            println!("[DEBUG] Objects {:?} are owned", dependencies);
                            continue;
                        }

                        for dependency in &dependencies {
                            let dependency = *dependency;
                            if !self.object_is_incoming(dependency)
                                && !self.object_is_owned(dependency)
                            {
                                #[cfg(feature = "telemetry")]
                                {
                                    let intent_name = match _intent_name {
                                        Some(n) => n,
                                        None => "",
                                    };
                                    let telemetry_ts = telemetry::zoned_timestamp_now();
                                    self.submit_telemetry_event(
                                        telemetry::TelemetryEvent::new_unresolvable_arg(
                                            intent_name,
                                            idx,
                                            telemetry_ts,
                                        ),
                                    );
                                }

                                return Schedulable::ArgsUnresolvableLocally;
                            }
                            args_to_wait_for.push(dependency);
                        }

                        continue;
                    }
                    NandoArgument::UnresolvedArgument(_) => {
                        panic!("unresolved argument in resolvability checker")
                    }
                    NandoArgument::TaskedUnresolvedArgument(_) => {
                        panic!("tasked unresolved argument in resolvability checker")
                    }
                    _ => continue,
                }
            }

            // FIXME @hack love too side-effect
            for (rewrite_idx, iptr) in cache_rewrites {
                args[rewrite_idx] = iptr;
            }

            if !args_to_wait_for.is_empty() {
                return Schedulable::AfterWaitingForArrival(args_to_wait_for);
            }
        }

        Schedulable::Immediately
    }

    #[cfg(not(feature = "offline"))]
    pub fn get_next_host(&self) -> Option<HostId> {
        let own_idx = { *self.host_idx.lock() };
        let Some(own_idx) = own_idx else {
            return None;
        };

        let mapping = self.host_mapping.read();
        // FIXME speed
        let mut available_indices: Vec<HostIdx> = mapping.keys().map(|k| *k).collect();
        available_indices.sort();

        let target_host_idx = match available_indices.iter().find(|h| own_idx < **h) {
            None => available_indices[0],
            Some(i) => *i,
        };

        if target_host_idx == own_idx {
            return None;
        }

        mapping.get(&target_host_idx).cloned()
    }

    #[cfg(feature = "offline")]
    pub fn get_next_host(&self) -> Option<HostId> {
        None
    }

    // NOTE this needs to be called from every worker, once all of them have registered but before
    // we start running the actual workload
    pub async fn fetch_host_mapping(&self) {
        #[cfg(not(feature = "offline"))]
        {
            let client = self.scheduler_client.clone();
            let resp = client
                .get(self.get_scheduler_url("worker_mapping"))
                .send()
                .await
                .expect("failed to get worker mapping from global scheduler");
            assert!(resp.status().is_success());
            let parsed_response = resp
                .json::<WorkerMapping>()
                .await
                .expect("failed to parse worker mapping response from scheduler");

            let mut mapping = self.host_mapping.write();
            *mapping = parsed_response.mapping;
        }
    }

    fn update_host_mapping(&self, host_idx: HostIdx, hostname: HostId) {
        let mut host_mapping = self.host_mapping.write();
        host_mapping.insert(host_idx, hostname);
    }

    pub fn update_remote_ownership(&self, snapshot: &HashMap<ObjectId, HostIdx>) {
        for (object_id, host_idx) in snapshot {
            let host_id = self.get_host_id_for_idx(*host_idx).unwrap();
            self.ownership_map
                .entry(*object_id)
                .and_modify(|e| {
                    if e != &host_id {
                        *e = host_id.clone();
                    }
                })
                .or_insert(host_id.clone());
        }
    }
}
