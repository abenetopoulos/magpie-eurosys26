use std::collections::HashMap;
use std::mem::MaybeUninit;
use std::sync::{Arc, Once};

use nando_support::{activation_intent, ObjectId, ObjectVersion};
use ownership_support;
use parking_lot::RwLock;
use reqwest;

use crate::orchestration::host_manager::HostManager;
use crate::orchestration::{error as orchestration_error, HostIdx};

pub(crate) struct OrchestrationClientManager {
    host_clients: RwLock<HashMap<HostIdx, Arc<reqwest::Client>>>,
}

impl OrchestrationClientManager {
    pub fn new() -> Self {
        Self {
            host_clients: RwLock::new(HashMap::new()),
        }
    }

    fn get_client_manager() -> &'static Arc<OrchestrationClientManager> {
        static mut INSTANCE: MaybeUninit<Arc<OrchestrationClientManager>> = MaybeUninit::uninit();
        static mut ONCE: Once = Once::new();

        unsafe {
            ONCE.call_once(|| {
                INSTANCE
                    .as_mut_ptr()
                    .write(Arc::new(OrchestrationClientManager::new()));
            });
        }

        unsafe { &*INSTANCE.as_ptr() }
    }

    fn get_client(
        &self,
        host_idx: &HostIdx,
    ) -> Result<Arc<reqwest::Client>, orchestration_error::InternalError> {
        let cached_client = {
            let host_clients = self.host_clients.read();
            match host_clients.get(host_idx) {
                Some(c) => Some(c.clone()),
                None => None,
            }
        };

        if cached_client.is_some() {
            return Ok(cached_client.unwrap());
        }

        let client = Arc::new(reqwest::Client::new());
        {
            let mut host_clients = self.host_clients.write();
            host_clients.insert(*host_idx, client.clone());
        }

        Ok(client)
    }

    pub async fn request_ownership_change(
        objects_to_move: &Vec<ObjectId>,
        current_owner: HostIdx,
        new_owner: HostIdx,
    ) -> Result<Vec<(ObjectId, ObjectVersion)>, orchestration_error::OwnershipTransferError> {
        let client_manager = Self::get_client_manager();
        let client = client_manager.get_client(&new_owner).expect(&format!(
            "failed to get orchestration client for host {}",
            new_owner
        ));

        let host_manager = HostManager::get_host_manager().clone();
        let current_owner_hostname = host_manager
            .get_hostname_by_idx(current_owner)
            .expect(&format!("no host for idx {}", current_owner));
        let new_owner_hostname = host_manager
            .get_hostname_by_idx(new_owner)
            .expect(&format!("no host for idx {}", new_owner));

        let body = ownership_support::MoveOwnershipRequest {
            object_refs: objects_to_move.into_iter().map(|oid| *oid).collect(),
            new_host: new_owner_hostname,
        };
        match client
            .post(format!(
                "http://{}:52017/activation_router/move_ownership",
                current_owner_hostname,
            ))
            .json(&body)
            .send()
            .await
        {
            Ok(response_body) => Ok(response_body
                .json::<ownership_support::MoveOwnershipResponse>()
                .await
                .expect("failed to parse ownership move response")
                .whomstone_versions),
            Err(e) => {
                eprintln!(
                    "move ownership request to {} for {:?} failed: {}",
                    current_owner, objects_to_move, e
                );
                // FIXME actually check what the problem is and return appropriate error variant
                Err(orchestration_error::OwnershipTransferError::UnknownError())
            }
        }
    }

    pub async fn request_cache_spawn(
        cache_spawn_intent: &activation_intent::NandoActivationIntentSerializable,
        current_owner: HostIdx,
    ) -> Result<ObjectId, orchestration_error::InternalError> {
        let client_manager = Self::get_client_manager();
        let client = client_manager.get_client(&current_owner).expect(&format!(
            "failed to get orchestration client for host {}",
            current_owner
        ));

        let host_manager = HostManager::get_host_manager().clone();
        let current_owner_hostname = host_manager
            .get_hostname_by_idx(current_owner)
            .expect(&format!("no host for idx {}", current_owner));
        match client
            .post(format!(
                "http://{}:52017/activation_router/schedule",
                current_owner_hostname,
            ))
            .json(&cache_spawn_intent)
            .send()
            .await
        {
            Ok(response_body) => {
                let response = response_body
                    .json::<activation_intent::NandoActivationResolution>()
                    .await
                    .expect("failed to parse cache spawning response");
                let object_ref = &response.output[0];
                let activation_intent::NandoArgumentSerializable::Ref(iptr) = object_ref else {
                    panic!(
                        "unexpected response to cache spawning request: {:?}",
                        object_ref
                    );
                };

                Ok(iptr.get_object_id())
            }
            Err(e) => {
                eprintln!(
                    "cache spawning request {:?} to {} failed: {}",
                    cache_spawn_intent, current_owner, e
                );
                // FIXME actually check what the problem is and return appropriate error variant
                Err(orchestration_error::InternalError::UnknownError())
            }
        }
    }

    pub async fn notify_add_cache_mapping(
        to: HostIdx,
        original_object_id: ObjectId,
        cached_object_id: ObjectId,
        version: ObjectVersion,
        original_owner_idx: HostIdx,
    ) -> Result<(), orchestration_error::InternalError> {
        let client_manager = Self::get_client_manager();
        let client = client_manager.get_client(&to).expect(&format!(
            "failed to get orchestration client for host {}",
            to
        ));

        let host_manager = HostManager::get_host_manager().clone();
        let cache_owner_hostname = host_manager
            .get_hostname_by_idx(to)
            .expect(&format!("no host for idx {}", to));
        let notify_request = ownership_support::AddCacheMappingRequest {
            original_object_id,
            cached_object_id,
            first_version: version,
            original_owner_idx,
        };

        match client
            .post(format!(
                "http://{}:52017/activation_router/add_cache_mapping",
                cache_owner_hostname,
            ))
            .json(&notify_request)
            .send()
            .await
        {
            Ok(_response_body) => Ok(()),
            Err(e) => {
                eprintln!(
                    "cache mapping request to {} for {} / {} failed: {}",
                    to, original_object_id, cached_object_id, e
                );
                Err(orchestration_error::InternalError::UnknownError())
            }
        }
    }
}
