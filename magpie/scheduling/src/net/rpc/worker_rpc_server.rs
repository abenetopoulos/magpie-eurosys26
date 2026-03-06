use std::collections::HashMap;
use std::sync::Arc;

#[cfg(feature = "observability")]
use lazy_static::lazy_static;
use nando_support::{activation_intent, epic_control, iptr::IPtr};
#[cfg(feature = "observability")]
use prometheus::{
    register_counter, register_counter_vec, register_histogram_vec, Counter, CounterVec, Encoder,
    HistogramVec,
};
use tonic::{Code, Request, Response, Status};
use worker_rpc::worker_api_server::WorkerApi;
use worker_rpc::{
    ActivationResolution, AssumeOwnershipRequest, AssumeOwnershipResponse, CacheMapping, Empty,
    FaultCacheRequest, MoveOwnershipRequest, MoveOwnershipResponse, NandoStatus, NandoStatusKind,
    ObjectVersionPair, SpawnedTask, SpawnedTaskLocations, TaskCompletion, TaskGraph,
    TaskGraphResolution,
};

use crate::activation_router::ActivationRouter;

pub mod worker_rpc {
    tonic::include_proto!("ww");
}

#[cfg(feature = "observability")]
lazy_static! {
    static ref RPC_AROUTER_REQ_COUNTER: CounterVec = register_counter_vec!(
        "activation_router_rpc_req_total",
        "Number of requests made to the activation router",
        &["path"],
    )
    .unwrap();
    static ref RPC_AROUTER_REQ_HISTOGRAM: HistogramVec = register_histogram_vec!(
        "activation_router_rpc_req_latency_microseconds",
        "Activation router request latencies in microseconds",
        &["path"],
        vec![10.0, 100.0, 1000.0, 10000.0],
    )
    .unwrap();
}

#[derive(Debug, Default)]
pub struct WorkerRpcServer {}

#[tonic::async_trait]
impl WorkerApi for WorkerRpcServer {
    async fn schedule_nando(
        &self,
        request: Request<worker_rpc::ActivationIntent>,
    ) -> Result<Response<ActivationResolution>, Status> {
        let activation_router =
            ActivationRouter::get_activation_router(None, None, None, None).clone();
        let intent = activation_intent::NandoActivationIntent::from(request.get_ref());

        // FIXME @physical plan info in request
        let execution_status = activation_router
            .try_execute_nando(intent, true, None)
            .await;

        match execution_status {
            Err(e) => {
                let result = if let Some(ref r) = e.1 {
                    vec![r.into()]
                } else {
                    vec![]
                };
                Ok(Response::new(ActivationResolution {
                    status: Some(NandoStatus {
                        kind: NandoStatusKind::Error.into(),
                        error_string: Some(e.0),
                        new_site: None,
                    }),
                    result,
                    cacheable_objects: vec![],
                }))
            }
            Ok((output, cacheable_objects)) => {
                let cacheable_objects_proto = cacheable_objects
                    .iter()
                    .map(|(oid, ov)| ObjectVersionPair {
                        object_id: oid.to_string(),
                        version: *ov,
                    })
                    .collect();

                let resolution: ActivationResolution = ActivationResolution {
                    status: Some(NandoStatus {
                        kind: NandoStatusKind::Success.into(),
                        error_string: None,
                        new_site: None,
                    }),
                    result: output.iter().map(|e| e.into()).collect(),
                    cacheable_objects: cacheable_objects_proto,
                };

                Ok(Response::new(resolution))
            }
        }
    }

    async fn schedule_spawned_task(
        &self,
        request: Request<SpawnedTask>,
    ) -> Result<Response<ActivationResolution>, Status> {
        let activation_router =
            ActivationRouter::get_activation_router(None, None, None, None).clone();
        let spawned_task = epic_control::SpawnedTask::from(request.get_ref());

        let execution_status = activation_router
            .try_execute_spawned_task(spawned_task)
            .await;

        match execution_status {
            Err(e) => Ok(Response::new(ActivationResolution {
                status: Some(NandoStatus {
                    kind: NandoStatusKind::Error.into(),
                    error_string: Some(e),
                    new_site: None,
                }),
                result: vec![],
                cacheable_objects: vec![],
            })),
            Ok((output, cacheable_objects)) => {
                let cacheable_objects_proto = cacheable_objects
                    .iter()
                    .map(|(oid, ov)| ObjectVersionPair {
                        object_id: oid.to_string(),
                        version: *ov,
                    })
                    .collect();

                let resolution: ActivationResolution = ActivationResolution {
                    status: Some(NandoStatus {
                        kind: NandoStatusKind::Success.into(),
                        error_string: None,
                        new_site: None,
                    }),
                    result: output.iter().map(|e| e.into()).collect(),
                    cacheable_objects: cacheable_objects_proto,
                };
                Ok(Response::new(resolution))
            }
        }
    }

    async fn handle_task_completion(
        &self,
        request: Request<TaskCompletion>,
    ) -> Result<Response<Empty>, Status> {
        let task_completion = request.get_ref();
        let completed_task_id = task_completion
            .id
            .parse()
            .expect("failed to parse completed task id");
        let mut req_tasks_to_notify = task_completion.tasks_to_notify.iter().map(|t| t.into());
        let mut statuses = task_completion.statuses.iter().map(|s| {
            match worker_rpc::NandoStatusKind::try_from(*s) {
                Ok(ref s) => s.into(),
                _ => panic!(),
            }
        });
        let mut results = task_completion.results.iter().map(|r| {
            match worker_rpc::NandoArgumentKind::try_from(r.kind) {
                Ok(worker_rpc::NandoArgumentKind::Nil) => None,
                _ => Some(r.into()),
            }
        });

        let mut tasks_to_notify = Vec::with_capacity(req_tasks_to_notify.len());
        while let Some(task) = req_tasks_to_notify.next() {
            let status = statuses.next().unwrap();
            let result = results.next().unwrap();
            tasks_to_notify.push((task, status, result));
        }

        let allocations = task_completion
            .subgraph_allocations
            .iter()
            .map(|a| {
                (
                    a.allocation_host_idx,
                    a.allocated_object.parse().expect("failed to parse"),
                )
            })
            .collect();
        let notifying_host_idx = task_completion.notifying_host_idx;

        #[cfg(debug_assertions)]
        println!(
            "About to handle task completion of {completed_task_id} for {:?}",
            tasks_to_notify
        );

        let activation_router =
            ActivationRouter::get_activation_router(None, None, None, None).clone();
        activation_router.store_remote_allocations(allocations);
        activation_router
            .handle_task_completion(completed_task_id, tasks_to_notify, Some(notifying_host_idx))
            .await;

        Ok(Response::new(Empty {}))
    }

    async fn assume_ownership(
        &self,
        request: Request<AssumeOwnershipRequest>,
    ) -> Result<Response<AssumeOwnershipResponse>, Status> {
        let request = request.get_ref();
        let object_id = request
            .object_id
            .parse()
            .expect("failed to parse object id");
        let first_version = request.first_version;
        let activation_router =
            ActivationRouter::get_activation_router(None, None, None, None).clone();
        let signature = activation_router
            .assume_ownership(object_id, first_version)
            .await
            .unwrap();

        Ok(Response::new(AssumeOwnershipResponse { signature }))
    }

    async fn move_ownership(
        &self,
        request: Request<MoveOwnershipRequest>,
    ) -> Result<Response<MoveOwnershipResponse>, Status> {
        let request = request.get_ref();
        let new_host = request.new_host.clone();

        let activation_router =
            ActivationRouter::get_activation_router(None, None, None, None).clone();
        let mut join_handles = Vec::with_capacity(request.object_refs.len());
        for object_ref in &request.object_refs {
            let activation_router = Arc::clone(&activation_router);
            let new_host = new_host.clone();
            let object_id = object_ref.parse().expect("failed to parse object id");
            join_handles.push((
                object_ref,
                tokio::spawn(async move {
                    activation_router
                        .whomstone_and_move_object(object_id, new_host)
                        .await
                }),
            ));
        }

        let mut whomstone_versions = Vec::with_capacity(join_handles.len());
        for (object_id, join_handle) in join_handles {
            // FIXME error handling
            let whomstone_version = join_handle.await.unwrap().unwrap();
            whomstone_versions.push((object_id, whomstone_version));
        }

        Ok(Response::new(MoveOwnershipResponse {
            whomstone_versions: whomstone_versions
                .iter()
                .map(|(oid, ov)| ObjectVersionPair {
                    object_id: oid.to_string(),
                    version: *ov,
                })
                .collect(),
        }))
    }

    async fn fault_shared_cache(
        &self,
        request: Request<FaultCacheRequest>,
    ) -> Result<Response<Empty>, Status> {
        let request = request.get_ref();

        let activation_router =
            ActivationRouter::get_activation_router(None, None, None, None).clone();

        match activation_router.add_cache_entry(
            request
                .original_object_id
                .parse()
                .expect("failed to parse original object id"),
            request
                .cached_object_id
                .parse()
                .expect("failed to parse cache object id"),
            request.version,
            request.host_idx,
        ) {
            Ok(()) => Ok(Response::new(Empty {})),
            Err(()) => Err(Status::new(
                Code::Internal,
                "Internal error while inserting cache entry",
            )),
        }
    }

    async fn update_task_ownership(
        &self,
        request: Request<SpawnedTaskLocations>,
    ) -> Result<Response<Empty>, Status> {
        let activation_router =
            ActivationRouter::get_activation_router(None, None, None, None).clone();

        let spawned_task_locations: HashMap<String, Vec<nando_support::ecb_id::EcbId>> = request
            .get_ref()
            .location_map
            .iter()
            .map(|(l, id_list)| {
                (
                    l.clone(),
                    id_list.ids.iter().map(|id| id.parse().unwrap()).collect(),
                )
            })
            .collect();

        activation_router.update_task_ownership(spawned_task_locations);
        Ok(Response::new(Empty {}))
    }

    async fn schedule_task_graph(
        &self,
        request: Request<TaskGraph>,
    ) -> Result<Response<TaskGraphResolution>, Status> {
        let activation_router =
            ActivationRouter::get_activation_router(None, None, None, None).clone();
        let spawned_tasks: Vec<epic_control::SpawnedTask> = request
            .get_ref()
            .graph_tasks
            .iter()
            .map(|st| epic_control::SpawnedTask::from(st))
            .collect();

        if spawned_tasks.is_empty() {
            return Ok(Response::new(TaskGraphResolution {
                activation_resolutions: vec![],
            }));
        }

        let spawned_task_locations: HashMap<String, Vec<nando_support::ecb_id::EcbId>> =
            match request.get_ref().spawned_task_locations {
                None => HashMap::default(),
                Some(ref stl) => stl
                    .location_map
                    .iter()
                    .map(|(l, id_list)| {
                        (
                            l.clone(),
                            id_list.ids.iter().map(|id| id.parse().unwrap()).collect(),
                        )
                    })
                    .collect(),
            };

        let execution_status = activation_router
            .try_schedule_task_graph(spawned_tasks, spawned_task_locations)
            .await;

        match execution_status {
            Err(e) => {
                eprintln!("failed to schedule task graph: {e}");
                Err(Status::internal(e))
            }
            Ok(outputs) => {
                let cacheable_objects_proto = outputs.iter().map(|(_, co)| co).fold(
                    vec![],
                    |acc: Vec<ObjectVersionPair>, cacheable_objects| {
                        vec![
                            acc,
                            cacheable_objects
                                .iter()
                                .map(|(oid, ov)| ObjectVersionPair {
                                    object_id: oid.to_string(),
                                    version: *ov,
                                })
                                .collect(),
                        ]
                        .into_iter()
                        .flatten()
                        .collect()
                    },
                );

                let resolutions: Vec<ActivationResolution> = outputs
                    .iter()
                    .map(|(outputs, _)| outputs)
                    .map(|output| {
                        ActivationResolution {
                            // FIXME this isn't always a success
                            status: Some(NandoStatus {
                                kind: NandoStatusKind::Success.into(),
                                error_string: None,
                                new_site: None,
                            }),
                            result: output.iter().map(|e| e.into()).collect(),
                            // FIXME is this correct?
                            cacheable_objects: cacheable_objects_proto.clone(),
                        }
                    })
                    .collect();
                Ok(Response::new(TaskGraphResolution {
                    activation_resolutions: resolutions,
                }))
            }
        }
    }

    async fn add_cache_mapping(
        &self,
        request: Request<CacheMapping>,
    ) -> Result<Response<Empty>, Status> {
        let activation_router =
            ActivationRouter::get_activation_router(None, None, None, None).clone();
        let request = request.get_ref();
        let src_object: IPtr = request
            .original_object
            .as_ref()
            .expect("missing src object iptr")
            .into();
        let cache_object: IPtr = request
            .cache_object
            .as_ref()
            .expect("missing cache object iptr")
            .into();
        let version = request.version;
        let original_owner = request.original_owner_idx;

        activation_router
            .add_valid_cache_mapping(
                src_object.get_object_id(),
                cache_object.get_object_id(),
                version,
                original_owner,
            )
            .await;

        Ok(Response::new(Empty {}))
    }
}
