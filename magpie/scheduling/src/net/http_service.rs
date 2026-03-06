use std::sync::Arc;

use axum::{
    extract,
    routing::{get, post, put},
    Router,
};
#[cfg(feature = "observability")]
use axum::{
    extract::{self, MatchedPath, Request},
    middleware::{self, Next},
    response::IntoResponse,
};
#[cfg(feature = "observability")]
use lazy_static::lazy_static;
use nando_support::{activation_intent, epic_control, epic_definitions};
use ownership_support as ownership;
#[cfg(feature = "observability")]
use prometheus::{
    register_counter, register_counter_vec, register_histogram_vec, Counter, CounterVec, Encoder,
    HistogramVec,
};
use tokio;

use crate::activation_router::ActivationRouter;

#[cfg(feature = "observability")]
lazy_static! {
    static ref HTTP_AROUTER_REQ_COUNTER: CounterVec = register_counter_vec!(
        "activation_router_req_total",
        "Number of requests made to the activation router",
        &["path", "intent_name"],
    )
    .unwrap();
    static ref HTTP_AROUTER_INVALIDATION_COUNTER: Counter = register_counter!(
        "activation_router_invalidation_total",
        "Number of invalidation requests made to the activation router",
    )
    .unwrap();
    static ref HTTP_AROUTER_REQ_HISTOGRAM: HistogramVec = register_histogram_vec!(
        "activation_router_req_latency_microseconds",
        "Activation router request latencies in microseconds",
        &["path", "intent_name"],
        vec![10.0, 100.0, 1000.0, 10000.0],
    )
    .unwrap();
}

pub trait NetServiceInterface {
    fn populate_routes(path_prefix: &str, router: Router) -> Router;
}

impl NetServiceInterface for ActivationRouter {
    fn populate_routes(path_prefix: &str, router: Router) -> Router {
        let router = router
            .route(
                &format!("{}/healthcheck", path_prefix),
                get(healthcheck_wrapper),
            )
            .route(&format!("{}/schedule", path_prefix), post(schedule_wrapper))
            .route(
                &format!("{}/schedule_spawned_task", path_prefix),
                post(schedule_spawned_task_wrapper),
            )
            .route(
                &format!("{}/task_completion", path_prefix),
                put(task_completion_wrapper),
            )
            .route(
                &format!("{}/move_ownership", path_prefix),
                post(move_ownership_wrapper),
            )
            .route(
                &format!("{}/assume_ownership", path_prefix),
                post(assume_ownership_wrapper),
            )
            .route(
                &format!("{}/epic/status", path_prefix),
                get(epic_status_wrapper),
            )
            .route(
                &format!("{}/epic/await_result", path_prefix),
                get(epic_result_wrapper),
            )
            .route(
                &format!("{}/fetch_host_mapping", path_prefix),
                get(fetch_host_mapping_wrapper),
            )
            .route(
                &format!("{}/add_cache_mapping", path_prefix),
                post(add_cache_mapping_wrapper),
            );

        #[cfg(feature = "observability")]
        let router = {
            router
                .route("/metrics", get(fetch_metrics))
                .route_layer(middleware::from_fn(track_metrics))
        };

        router
    }
}

async fn healthcheck_wrapper() -> &'static str {
    let activation_router = ActivationRouter::get_activation_router(None, None, None, None).clone();
    activation_router.healthcheck().await
}

async fn schedule_wrapper(
    extract::Json(payload): extract::Json<activation_intent::NandoActivationIntentSerializable>,
) -> extract::Json<activation_intent::NandoActivationResolution> {
    let activation_router = ActivationRouter::get_activation_router(None, None, None, None).clone();
    let is_external = payload.host_idx.is_none();
    let with_plan = payload.with_plan.clone();
    let activation_intent = activation_intent::NandoActivationIntent::from(payload);
    let execution_status = activation_router
        .try_execute_nando(activation_intent, true, with_plan)
        .await;

    // FIXME? this is very graphql of us (http status codes will always be 200, other side needs to
    // inspect the contents of our response to decide what to do)
    extract::Json(match execution_status {
        Ok((output, cacheable_objects)) => {
            let serializable_output = output.iter().map(|r| r.into()).collect();
            activation_intent::NandoActivationResolution {
                status: activation_intent::NandoActivationStatus::Executed(
                    activation_intent::NandoActivationExecutionStatus::Success,
                ),
                output: serializable_output,
                cacheable_objects: match is_external {
                    true => None,
                    false => Some(cacheable_objects),
                },
            }
        }
        Err(e) => {
            let output = if let Some(ref r) = e.1 {
                vec![r.into()]
            } else {
                vec![]
            };
            activation_intent::NandoActivationResolution {
                status: activation_intent::NandoActivationStatus::Executed(
                    activation_intent::NandoActivationExecutionStatus::Error(e.0),
                ),
                output,
                cacheable_objects: None,
            }
        }
    })
}

async fn schedule_spawned_task_wrapper(
    extract::Json(payload): extract::Json<activation_intent::SpawnedTaskSerializable>,
) -> extract::Json<activation_intent::NandoActivationResolution> {
    let activation_router = ActivationRouter::get_activation_router(None, None, None, None).clone();
    let spawned_task: epic_control::SpawnedTask = (&payload).into();
    let execution_status = activation_router
        .try_execute_spawned_task(spawned_task)
        .await;

    extract::Json(match execution_status {
        Ok((output, cacheable_objects)) => {
            let serializable_output = output.iter().map(|r| r.into()).collect();
            activation_intent::NandoActivationResolution {
                status: activation_intent::NandoActivationStatus::Executed(
                    activation_intent::NandoActivationExecutionStatus::Success,
                ),
                output: serializable_output,
                cacheable_objects: match cacheable_objects.is_empty() {
                    true => None,
                    false => Some(cacheable_objects),
                },
            }
        }
        Err(e) => activation_intent::NandoActivationResolution {
            status: activation_intent::NandoActivationStatus::Executed(
                activation_intent::NandoActivationExecutionStatus::Error(e),
            ),
            output: vec![],
            cacheable_objects: None,
        },
    })
}

async fn task_completion_wrapper(
    extract::Json(payload): extract::Json<activation_intent::TaskCompletionSerializable>,
) -> extract::Json<()> {
    let activation_router = ActivationRouter::get_activation_router(None, None, None, None).clone();
    let tasks_to_notify: Vec<(
        epic_control::DownstreamTaskDependency,
        epic_control::TaskStatus,
        Option<activation_intent::NandoResult>,
    )> = payload
        .tasks_to_notify
        .iter()
        .map(|(e, s, r)| {
            (
                <activation_intent::DownstreamTaskDependencyDef as Into<
                    epic_control::DownstreamTaskDependency,
                >>::into(*e),
                (*s).clone().into(),
                match r {
                    None => None,
                    Some(ref r) => Some(r.into()),
                },
            )
        })
        .collect();
    let notifying_host = payload.notifying_host;
    activation_router
        .handle_task_completion(payload.id, tasks_to_notify, Some(notifying_host))
        .await;

    extract::Json(())
}

async fn move_ownership_wrapper(
    extract::Json(payload): extract::Json<ownership::MoveOwnershipRequest>,
) -> extract::Json<ownership::MoveOwnershipResponse> {
    // FIXME request batching
    let activation_router = ActivationRouter::get_activation_router(None, None, None, None).clone();
    let new_host = payload.new_host.clone();

    let mut join_handles = Vec::with_capacity(payload.object_refs.len());
    for object_ref in payload.object_refs {
        let activation_router = Arc::clone(&activation_router);
        let new_host = new_host.clone();
        join_handles.push((
            object_ref,
            tokio::spawn(async move {
                activation_router
                    .whomstone_and_move_object(object_ref, new_host)
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

    extract::Json(ownership::MoveOwnershipResponse { whomstone_versions })
}

async fn assume_ownership_wrapper(
    extract::Json(payload): extract::Json<ownership::AssumeOwnershipRequest>,
) -> extract::Json<ownership::AssumeOwnershipResponse> {
    let activation_router = ActivationRouter::get_activation_router(None, None, None, None).clone();
    let signature = activation_router
        .assume_ownership(payload.object_id, payload.first_version)
        .await
        .unwrap();

    extract::Json(ownership::AssumeOwnershipResponse { signature })
}

async fn add_cache_mapping_wrapper(
    extract::Json(payload): extract::Json<ownership::AddCacheMappingRequest>,
) -> extract::Json<ownership::AddCacheMappingResponse> {
    let activation_router = ActivationRouter::get_activation_router(None, None, None, None).clone();

    activation_router
        .add_valid_cache_mapping(
            payload.original_object_id,
            payload.cached_object_id,
            payload.first_version,
            payload.original_owner_idx,
        )
        .await;

    extract::Json(ownership::AddCacheMappingResponse {})
}

async fn epic_status_wrapper(
    extract::Json(payload): extract::Json<epic_definitions::GetEpicStatusRequest>,
) -> extract::Json<epic_definitions::EpicStatusResponse> {
    let activation_router = ActivationRouter::get_activation_router(None, None, None, None).clone();
    let epic_execution_status = activation_router.get_epic_status(payload).await;

    extract::Json(match epic_execution_status {
        Ok(s) => epic_definitions::EpicStatusResponse::Status(s),
        Err(e) => epic_definitions::EpicStatusResponse::NotFound(e),
    })
}

async fn epic_result_wrapper(
    extract::Json(payload): extract::Json<epic_definitions::AwaitEpicResultRequest>,
) -> extract::Json<epic_definitions::EpicStatusResponse> {
    let activation_router = ActivationRouter::get_activation_router(None, None, None, None).clone();
    let epic_execution_status = activation_router.get_epic_result(payload).await;

    extract::Json(match epic_execution_status {
        Ok(s) => epic_definitions::EpicStatusResponse::Status(s),
        Err(e) => epic_definitions::EpicStatusResponse::NotFound(e),
    })
}

async fn fetch_host_mapping_wrapper() -> extract::Json<()> {
    let activation_router = ActivationRouter::get_activation_router(None, None, None, None).clone();
    activation_router.fetch_host_mapping().await;

    extract::Json(())
}

#[cfg(feature = "observability")]
async fn track_metrics(req: Request, next: Next) -> impl IntoResponse {
    let start = tokio::time::Instant::now();

    let path = if let Some(matched_path) = req.extensions().get::<MatchedPath>() {
        matched_path.as_str().to_owned()
    } else {
        req.uri().path().to_owned()
    };

    let (parts, body) = req.into_parts();
    let body_bytes = axum::body::to_bytes(body, usize::MAX)
        .await
        .expect("failed to get body bytes in metrics tracker");

    let intent = match path.contains("schedule") {
        false => "N/A".to_string(),
        true => match path.contains("spawned_task") {
            false => {
                let body: extract::Json<activation_intent::NandoActivationIntentSerializable> =
                    axum::Json::from_bytes(&body_bytes)
                        .expect("Failed to parse in metrics tracker");
                body.name.clone()
            }
            true => {
                let body: extract::Json<activation_intent::SpawnedTaskSerializable> =
                    axum::Json::from_bytes(&body_bytes)
                        .expect("Failed to parse in metrics tracker");
                body.intent.name.clone()
            }
        },
    };

    let req = Request::from_parts(parts, body_bytes.into());
    let response = next.run(req).await;

    let duration = start.elapsed().as_micros() as f64;
    HTTP_AROUTER_REQ_COUNTER
        .with_label_values(&[&path, &intent])
        .inc();
    HTTP_AROUTER_REQ_HISTOGRAM
        .with_label_values(&[&path, &intent])
        .observe(duration);

    if intent.contains("invalidate") {
        HTTP_AROUTER_INVALIDATION_COUNTER.inc();
    }

    response
}

#[cfg(feature = "observability")]
async fn fetch_metrics() -> Result<String, String> {
    let encoder = prometheus::TextEncoder::new();
    let metrics = prometheus::gather();

    let mut buffer = Vec::new();
    if let Err(e) = encoder.encode(&metrics, &mut buffer) {
        let err_msg = format!("could not encode custom metrics: {}", e);
        eprintln!("{}", err_msg);

        return Err(err_msg);
    };

    let metrics_response = match String::from_utf8(buffer.clone()) {
        Ok(v) => v,
        Err(e) => {
            let err_msg = format!("prometheus metrics could not be from_utf8'd: {}", e);
            eprintln!("{}", err_msg);

            return Err(err_msg);
        }
    };

    Ok(metrics_response)
}
