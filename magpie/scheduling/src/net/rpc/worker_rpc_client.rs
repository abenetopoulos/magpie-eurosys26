use std::collections::HashMap;

use dashmap::DashMap;
use execution_definitions::nando_handle::ActivationOutput;
use location_manager::HostId;
use nando_lib::nando_scheduler::TaskCompletionNotification;
use nando_support::{activation_intent, ecb_id::EcbId, epic_control, iptr::IPtr};
use object_lib::{ObjectId, ObjectVersion};
use ownership_support as ownership;
use tonic::{transport::channel::Channel, Request};
use worker_rpc::worker_api_client::WorkerApiClient;
use worker_rpc::{
    CacheMapping, FaultCacheRequest, SpawnedTaskIdList, SpawnedTaskLocations, TaskGraph,
};

type ConcreteWorkerClient = WorkerApiClient<Channel>;

pub mod worker_rpc {
    tonic::include_proto!("ww");
}

#[derive(Debug)]
pub struct WorkerRpcClient {
    server_port: u16,
    host_clients: DashMap<HostId, ConcreteWorkerClient>,
}

// owned by the activation router instance.
impl WorkerRpcClient {
    pub fn new(server_port: u16) -> Self {
        Self {
            server_port,
            host_clients: DashMap::new(),
        }
    }

    async fn get_client(&self, host_id: &HostId) -> Result<ConcreteWorkerClient, String> {
        let key = match std::thread::current().name() {
            Some(ref thread_name) => {
                format!("{}:{}", thread_name.to_string(), host_id)
            }
            None => host_id.to_string(),
        };
        match self.host_clients.get(&key) {
            Some(c) => return Ok(c.value().clone()),
            None => {}
        }

        let server_addr = format!("http://{}:{}", host_id, self.server_port);
        match WorkerApiClient::connect(server_addr).await {
            Ok(client) => {
                let client = client
                    .max_decoding_message_size(512 * 1024 * 1024)
                    .max_encoding_message_size(512 * 1024 * 1024);
                self.host_clients.insert(key, client.clone());

                Ok(client)
            }
            Err(e) => {
                let err_msg = format!(
                    "Failed to establish connection with host {}: {}",
                    host_id, e,
                );
                eprintln!("{}", err_msg);

                Err(err_msg)
            }
        }
    }

    pub async fn forward_task_completion(
        &self,
        task_completion_notification: TaskCompletionNotification,
        host_idx: ownership::HostIdx,
        target_host: &HostId,
    ) -> Result<(), String> {
        let completed_task = task_completion_notification.completed_task_id;
        #[cfg(debug_assertions)]
        println!("About to fwd task completion of {completed_task} to {target_host}");

        let mut request: worker_rpc::TaskCompletion = (&task_completion_notification).into();
        request.notifying_host_idx = host_idx;
        request.subgraph_allocations = task_completion_notification
            .subgraph_allocations
            .iter()
            .map(|a| worker_rpc::Allocation {
                allocation_host_idx: host_idx,
                allocated_object: a.to_string(),
            })
            .collect();

        let request = Request::new(request);

        let mut target_client = self
            .get_client(target_host)
            .await
            .expect(&format!("failed to get rpc client for {}", target_host));

        match target_client.handle_task_completion(request).await {
            Ok(_) => Ok(()),
            Err(e) => {
                let err_msg = format!(
                    "could not forward task completion for {:?}: {}",
                    completed_task, e
                );
                eprintln!("{}", err_msg);
                Err(err_msg)
            }
        }
    }

    pub async fn schedule_nando(
        &self,
        activation_intent_request: activation_intent::NandoActivationIntent,
        target_host: &HostId,
    ) -> Result<(Vec<ActivationOutput>, Vec<(ObjectId, ObjectVersion)>), String> {
        let request = Request::new((&activation_intent_request).into());
        let mut target_client = self
            .get_client(target_host)
            .await
            .expect(&format!("failed to get rpc client for {}", target_host));

        match target_client.schedule_nando(request).await {
            Ok(resolution_response) => {
                let resolution = resolution_response.get_ref();
                let resolution_status = resolution
                    .status
                    .as_ref()
                    .expect("no status in activation resolution");
                match worker_rpc::NandoStatusKind::try_from(resolution_status.kind) {
                    Ok(worker_rpc::NandoStatusKind::Error) => {
                        Err(resolution_status.error_string.as_ref().unwrap().clone())
                    }
                    Ok(worker_rpc::NandoStatusKind::Success) => {
                        let result = match resolution.result.is_empty() {
                            true => vec![],
                            false => resolution
                                .result
                                .iter()
                                .map(|r| {
                                    let result: ActivationOutput = r.into();
                                    result.into()
                                })
                                .collect(),
                        };

                        Ok((
                            result,
                            resolution
                                .cacheable_objects
                                .iter()
                                .map(|pair| {
                                    (
                                        pair.object_id.parse().expect(&format!(
                                            "failed to parse object id {}",
                                            pair.object_id
                                        )),
                                        pair.version,
                                    )
                                })
                                .collect(),
                        ))
                    }
                    Ok(worker_rpc::NandoStatusKind::RecomputedSite) => {
                        todo!("intra-worker intent relocation")
                    }
                    _ => panic!("unsupported status kind"),
                }
            }
            Err(e) => {
                let err_msg = format!(
                    "could not forward nando for {} to {}: {}",
                    activation_intent_request.name, target_host, e
                );
                eprintln!("{}", err_msg);
                Err(err_msg)
            }
        }
    }

    pub async fn forward_spawned_task(
        &self,
        spawned_task: epic_control::SpawnedTask,
        target_host: &HostId,
    ) -> Result<activation_intent::NandoActivationResolution, String> {
        let request = Request::new((&spawned_task).into());
        let mut target_client = self
            .get_client(target_host)
            .await
            .expect(&format!("failed to get rpc client for {}", target_host));

        match target_client.schedule_spawned_task(request).await {
            Ok(resolution_response) => {
                let resolution = resolution_response.get_ref();
                Ok(resolution.into())
            }
            Err(e) => {
                let err_msg = format!(
                    "could not forward spawned task for {} to {}: {}",
                    spawned_task.intent.name, target_host, e
                );
                eprintln!("{}", err_msg);
                Err(err_msg)
            }
        }
    }

    pub async fn schedule_task_graph(
        &self,
        spawned_tasks: &Vec<epic_control::SpawnedTask>,
        spawned_task_locations: &HashMap<HostId, Vec<EcbId>>,
        target_host: &HostId,
    ) -> Result<Vec<activation_intent::NandoActivationResolution>, String> {
        let mut sent_locations = false;
        let mut resolutions: Vec<activation_intent::NandoActivationResolution> = Vec::default();

        // FIXME is this enough to ensure no chunking?
        let chunk_size = 1024 * 1024 * 1024;
        let mut target_client = self
            .get_client(target_host)
            .await
            .expect(&format!("failed to get rpc client for {}", target_host));

        if spawned_tasks.len() > chunk_size {
            sent_locations = true;
            let request = Request::new(SpawnedTaskLocations {
                location_map: spawned_task_locations
                    .iter()
                    .map(|(l, st_ids)| {
                        (
                            l.clone(),
                            SpawnedTaskIdList {
                                ids: st_ids.iter().map(|st_id| st_id.to_string()).collect(),
                            },
                        )
                    })
                    .collect(),
            });

            match target_client.update_task_ownership(request).await {
                Ok(_) => {}
                Err(e) => {
                    let err_msg =
                        format!("could not forward task locations to {}: {}", target_host, e);
                    eprintln!("{}", err_msg);
                    return Err(err_msg);
                }
            }
        }

        for chunk in spawned_tasks.chunks(chunk_size) {
            let request = Request::new(TaskGraph {
                graph_tasks: chunk.iter().map(|st| st.into()).collect(),
                spawned_task_locations: match sent_locations {
                    true => None,
                    false => {
                        sent_locations = true;
                        Some(SpawnedTaskLocations {
                            location_map: spawned_task_locations
                                .iter()
                                .map(|(l, st_ids)| {
                                    (
                                        l.clone(),
                                        SpawnedTaskIdList {
                                            ids: st_ids
                                                .iter()
                                                .map(|st_id| st_id.to_string())
                                                .collect(),
                                        },
                                    )
                                })
                                .collect(),
                        })
                    }
                },
            });

            match target_client.schedule_task_graph(request).await {
                Ok(resolution_response) => {
                    let resolution = resolution_response.get_ref();
                    resolutions.extend(resolution.activation_resolutions.iter().map(|r| r.into()))
                }
                Err(e) => {
                    let err_msg = format!("could not forward task graph to {}: {}", target_host, e);
                    eprintln!("{}", err_msg);
                    return Err(err_msg);
                }
            }
        }

        Ok(resolutions)
    }

    pub async fn assume_ownership(
        &self,
        assume_ownership_request: ownership::AssumeOwnershipRequest,
        target_host: &HostId,
    ) -> Result<Vec<u8>, String> {
        let request = Request::new((&assume_ownership_request).into());
        let mut target_client = self
            .get_client(target_host)
            .await
            .expect(&format!("failed to get rpc client for {}", target_host));

        match target_client.assume_ownership(request).await {
            Ok(assume_ownership_response) => {
                let response = assume_ownership_response.get_ref();
                Ok(response.signature.clone())
            }
            Err(e) => {
                let err_msg = format!(
                    "could not forward request to assume ownership of {} to {}: {}",
                    assume_ownership_request.object_id, target_host, e
                );
                eprintln!("{}", err_msg);
                Err(err_msg)
            }
        }
    }

    /*
    pub async fn push_copy(
        &self,
        assume_ownership_request: ownership::AssumeOwnershipRequest,
        target_host: &HostId,
    ) -> Result<Vec<u8>, String> {
        let request = Request::new((&assume_ownership_request).into());
        println!("about to push copy to target host {}", target_host);
        let mut target_client = self
            .get_client(target_host)
            .await
            .expect(&format!("failed to get rpc client for {}", target_host));

        match target_client.push_copy(request).await {
            Ok(assume_ownership_response) => {
                let response = assume_ownership_response.get_ref();
                Ok(response.signature.clone())
            }
            Err(e) => {
                let err_msg = format!(
                    "could not forward request to assume ownership of {} to {}: {}",
                    assume_ownership_request.object_id, target_host, e
                );
                eprintln!("{}", err_msg);
                Err(err_msg)
            }
        }
    }
    */

    pub async fn move_ownership(
        &self,
        move_ownership_request: ownership::MoveOwnershipRequest,
        target_host: &HostId,
    ) -> Result<ownership_support::MoveOwnershipResponse, String> {
        let request = Request::new((&move_ownership_request).into());
        let mut target_client = self
            .get_client(target_host)
            .await
            .expect(&format!("failed to get rpc client for {}", target_host));

        match target_client.move_ownership(request).await {
            Ok(move_ownership_response) => {
                let response = move_ownership_response.get_ref();
                Ok(ownership_support::MoveOwnershipResponse {
                    whomstone_versions: response
                        .whomstone_versions
                        .iter()
                        .map(|pair| {
                            (
                                pair.object_id.parse().expect(&format!(
                                    "failed to parse object id {}",
                                    pair.object_id
                                )),
                                pair.version,
                            )
                        })
                        .collect(),
                })
            }
            Err(e) => {
                let err_msg = format!(
                    "could not forward request to move ownership of {:?} to {}: {}",
                    move_ownership_request.object_refs, target_host, e
                );
                eprintln!("{}", err_msg);
                Err(err_msg)
            }
        }
    }

    pub async fn fault_shared_cache(
        &self,
        host_idx: ownership::HostIdx,
        original_object_id: ObjectId,
        cached_object_id: ObjectId,
        cache_version: ObjectVersion,
        target_host: &HostId,
    ) -> Result<(), String> {
        let request = Request::new(FaultCacheRequest {
            host_idx,
            original_object_id: original_object_id.to_string(),
            cached_object_id: cached_object_id.to_string(),
            version: cache_version,
        });
        let mut target_client = self
            .get_client(target_host)
            .await
            .expect(&format!("failed to get rpc client for {}", target_host));

        match target_client.fault_shared_cache(request).await {
            Ok(_) => Ok(()),
            Err(e) => {
                let err_msg = format!("failed to insert shared cache entry remotely: {}", e);
                eprintln!("{}", err_msg);
                Err(err_msg)
            }
        }
    }

    pub async fn add_cache_mapping(
        &self,
        original_object: &IPtr,
        cache_object: &IPtr,
        version: ObjectVersion,
        target_host: &HostId,
        own_idx: ownership::HostIdx,
    ) -> Result<(), String> {
        let request = Request::new(CacheMapping {
            original_object: Some(original_object.into()),
            cache_object: Some(cache_object.into()),
            version,
            original_owner_idx: own_idx,
        });
        let mut target_client = self
            .get_client(target_host)
            .await
            .expect(&format!("failed to get rpc client for {}", target_host));

        match target_client.add_cache_mapping(request).await {
            Ok(_) => Ok(()),
            Err(e) => {
                let err_msg = format!("failed to add cache mapping remotely: {}", e);
                eprintln!("{}", err_msg);
                Err(err_msg)
            }
        }
    }
}
