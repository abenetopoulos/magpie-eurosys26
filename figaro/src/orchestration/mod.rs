use std::collections::HashMap;

use actix_web::rt;
use nando_support::ObjectId;
use registry::OwnershipRegistry;

use client::OrchestrationClientManager;
pub use error::HostManagementError;
use host_manager::HostManager;
use nando_support::{activation_intent, iptr::IPtr, ObjectVersion};
pub use ownership_support::{HostIdx, Hostname};

pub(crate) mod host_manager;
pub mod registry;

mod client;
mod error;

#[cfg(feature = "always_move")]
fn compute_location(objects: &Vec<ObjectId>) -> (Option<HostIdx>, HashMap<HostIdx, Vec<ObjectId>>) {
    let ownership_registry = OwnershipRegistry::get_ownership_registry().clone();
    let owning_hosts_map = ownership_registry.get_owning_hosts(objects);

    (None, owning_hosts_map)
}

#[cfg(not(feature = "always_move"))]
fn compute_location(objects: &Vec<ObjectId>) -> (Option<HostIdx>, HashMap<HostIdx, Vec<ObjectId>>) {
    let ownership_registry = OwnershipRegistry::get_ownership_registry().clone();
    let owning_hosts_map = ownership_registry.get_owning_hosts(objects);

    let host_manager = HostManager::get_host_manager().clone();
    let num_hosts = host_manager.number_of_registered_hosts();
    let mut min_moves = objects.len();
    let mut min_move_host_idx = 0;
    let mut host_preference = vec![min_moves; num_hosts];
    for (host_id_ref, objects_owned_by_hosts) in owning_hosts_map.iter() {
        let host_id = *host_id_ref;
        let new_value = {
            host_preference[host_id as usize] -= objects_owned_by_hosts.len();
            host_preference[host_id as usize]
        };

        if min_moves < new_value {
            min_moves = new_value;
            min_move_host_idx = host_id;
        }
    }

    (Some(min_move_host_idx), owning_hosts_map)
}

pub async fn request_ownership_change(
    objects_to_move: &Vec<ObjectId>,
    from: HostIdx,
    to: HostIdx,
    update_internal_mapping: bool,
) -> Vec<(ObjectId, ObjectVersion)> {
    let whomstone_versions = match OrchestrationClientManager::request_ownership_change(
        &objects_to_move,
        from,
        to,
    )
    .await
    {
        Ok(wv) => wv,
        Err(_) => return vec![],
    };

    if update_internal_mapping {
        let ownership_registry = OwnershipRegistry::get_ownership_registry().clone();
        for (object_id, whomstone_version) in whomstone_versions.iter() {
            #[cfg(debug_assertions)]
            println!(
                "will update object version mappings {}: {}",
                object_id, whomstone_version
            );
            ownership_registry.update_ownership(*object_id, *whomstone_version, from, to);
        }
    }

    whomstone_versions
}

pub async fn request_cache_spawn(object_to_copy: ObjectId, from: HostIdx, to: HostIdx) -> ObjectId {
    let caching_intent = activation_intent::NandoActivationIntentSerializable {
        host_idx: Some(to.clone()),
        name: "spawn_cache".to_string(),
        args: vec![
            activation_intent::NandoArgumentSerializable::Ref(IPtr::new(object_to_copy, 0, 0)),
            (&activation_intent::NandoArgument::Value(activation_intent::ScalarValue::Nil)).into(),
        ],
        with_plan: None,
    };

    OrchestrationClientManager::request_cache_spawn(&caching_intent, from)
        .await
        .unwrap()
}

pub(crate) async fn consolidate(
    requesting_host_idx: Option<HostIdx>,
    objects: &Vec<ObjectId>,
) -> Option<(HostIdx, Vec<(ObjectId, ObjectVersion)>)> {
    // NOTE while this is running, we should probably have some locks on the objects we are
    // currently trying to move, or at least some way of signaling that an ownership transfer is
    // in progress
    let (activation_location, old_owner_map) = compute_location(&objects);
    let activation_location = match activation_location {
        Some(al) => al,
        None => {
            requesting_host_idx.expect("cannot schedule without knowing the originating host idx")
        }
    };
    let mut ownership_transfer_tasks = Vec::with_capacity(old_owner_map.len());
    for (owner, objects_to_move) in old_owner_map {
        if activation_location == owner {
            continue;
        }

        let activation_location = activation_location.clone();
        ownership_transfer_tasks.push(rt::spawn(async move {
            request_ownership_change(&objects_to_move, owner, activation_location, true).await
        }));
    }

    let mut aggregate_whomstone_versions = Vec::with_capacity(ownership_transfer_tasks.len());
    for task_join_handle in ownership_transfer_tasks {
        match task_join_handle.await {
            Ok(wv) => aggregate_whomstone_versions.extend_from_slice(&wv),
            Err(e) => {
                eprintln!("ownership change task ran into an error: {}", e);
                return None;
            }
        }
    }

    Some((activation_location, aggregate_whomstone_versions))
}

pub(crate) async fn force_consolidate(
    requesting_host_idx: HostIdx,
    objects: &Vec<ObjectId>,
) -> Option<(HostIdx, Vec<(ObjectId, ObjectVersion)>)> {
    // NOTE while this is running, we should probably have some locks on the objects we are
    // currently trying to move, or at least some way of signaling that an ownership transfer is
    // in progress
    let (_, old_owner_map) = compute_location(&objects);
    let activation_location = requesting_host_idx;
    let mut ownership_transfer_tasks = Vec::with_capacity(old_owner_map.len());
    for (owner, objects_to_move) in old_owner_map {
        if activation_location == owner {
            continue;
        }

        let activation_location = activation_location.clone();
        ownership_transfer_tasks.push(rt::spawn(async move {
            request_ownership_change(&objects_to_move, owner, activation_location, true).await
        }));
    }

    let mut aggregate_whomstone_versions = Vec::with_capacity(ownership_transfer_tasks.len());
    for task_join_handle in ownership_transfer_tasks {
        match task_join_handle.await {
            Ok(wv) => aggregate_whomstone_versions.extend_from_slice(&wv),
            Err(e) => {
                eprintln!("ownership change task ran into an error: {}", e);
                return None;
            }
        }
    }

    Some((activation_location, aggregate_whomstone_versions))
}

pub async fn notify_add_cache_mapping(
    to: HostIdx,
    original_object_id: ObjectId,
    cached_object_id: ObjectId,
    version: ObjectVersion,
    original_owner_idx: HostIdx,
) {
    match OrchestrationClientManager::notify_add_cache_mapping(
        to,
        original_object_id,
        cached_object_id,
        version,
        original_owner_idx,
    )
    .await
    {
        Ok(()) => {}
        Err(e) => {
            eprintln!("failed to notify host {} of a new cache mapping: {}", to, e);
        }
    }
}

// FIXME batch updates, also this should be ownership (not location) change
pub fn peer_location_change(to: HostIdx, moved_object: ObjectId, whomstone_version: ObjectVersion) {
    let ownership_registry = OwnershipRegistry::get_ownership_registry().clone();
    let current_owner = ownership_registry
        .get_owning_host(&moved_object)
        .expect("peer location change requested for unpublished object");
    ownership_registry.update_ownership(moved_object, whomstone_version, current_owner, to);
}

pub fn register_worker(
    host_idx: Option<HostIdx>,
    hostname: Hostname,
) -> Result<HostIdx, HostManagementError> {
    let host_manager = HostManager::get_host_manager().clone();
    match host_idx {
        Some(idx) => host_manager.insert_host_with_idx(idx, hostname),
        None => host_manager.insert_host(hostname),
    }
}

pub fn get_num_workers() -> usize {
    let host_manager = HostManager::get_host_manager().clone();
    host_manager.get_num_workers()
}

pub fn get_worker_hostname_by_idx(host_idx: HostIdx) -> Option<Hostname> {
    let host_manager = HostManager::get_host_manager().clone();
    host_manager.get_hostname_by_idx(host_idx)
}

pub fn get_hostname_projection() -> HashMap<HostIdx, Hostname> {
    let host_manager = HostManager::get_host_manager().clone();
    host_manager.get_hostname_projection()
}

pub fn reset_host_mgr_state() {
    let host_manager = HostManager::get_host_manager().clone();
    host_manager.reset_state();
}
