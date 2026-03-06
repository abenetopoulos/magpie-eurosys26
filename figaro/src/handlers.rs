use actix_web::{get, post, put, web, HttpResponse, Responder};
use nando_support::activation_intent::{NandoActivationIntentSerializable, SchedulerIntent};
use nando_support::ObjectId;
use ownership_support::{
    ConsolidationIntent, MultiPublishRequest, OwnershipSnapshotResponse, PublishRequest,
    RegisterWorkerRequest, RegisterWorkerResponse, ScheduleResponse, WorkerMapping,
};
use rand::{rngs::SmallRng, Rng, SeedableRng};

use crate::orchestration::{self, registry};

macro_rules! resolve_host_idx {
    ($idx:literal) => {
        orchestration::get_worker_hostname_by_idx($idx)
            .expect(&format!("idx {} does not correspond to host", $idx))
    };
    ($idx:ident) => {
        orchestration::get_worker_hostname_by_idx($idx)
            .expect(&format!("idx {} does not correspond to host", $idx))
    };
}

#[get("/healthcheck")]
pub async fn healthcheck_handler() -> impl Responder {
    println!("accepting healthcheck");
    HttpResponse::Ok().body("Global Scheduler Instance is up\n")
}

// TODO replace json with bincode
#[post("/schedule")]
pub async fn schedule_handler(
    // intent: web::Json<NandoActivationIntentSerializable>,
    scheduler_intent: web::Json<SchedulerIntent>,
) -> impl Responder {
    println!("Got request to schedule {:#?}", scheduler_intent);
    #[cfg(feature = "timing")]
    let start = tokio::time::Instant::now();
    let object_dependencies = match scheduler_intent.mutable_argument_indices.is_empty() {
        true => scheduler_intent.get_object_references(),
        false => scheduler_intent.get_mut_object_references(),
    };
    let requesting_host_idx = scheduler_intent.intent.host_idx;
    let response = 'response: {
        if object_dependencies.len() == 0 {
            // randomly pick a host
            // FIXME this should pick the least-busy host
            let host_idx = {
                let mut host_idx_rng = SmallRng::from_entropy();
                host_idx_rng.gen::<u64>() % orchestration::get_num_workers() as u64
            };

            break 'response ScheduleResponse {
                target_host: resolve_host_idx!(host_idx),
                had_to_consolidate: false,
            };
        }

        let ownership_registry = registry::OwnershipRegistry::get_ownership_registry().clone();
        #[cfg(not(feature = "always_move"))]
        if let Some(h) = ownership_registry.objects_are_colocated(&object_dependencies) {
            break 'response ScheduleResponse {
                target_host: resolve_host_idx!(h),
                had_to_consolidate: false,
            };
        };

        let activation_site =
            match orchestration::consolidate(requesting_host_idx, &object_dependencies).await {
                None => {
                    return HttpResponse::Conflict().body(format!(
                        "failed to consolidate objects {:?} to {:?}",
                        object_dependencies, requesting_host_idx,
                    ));
                }
                Some((activation_site, _)) => activation_site,
            };
        ScheduleResponse {
            target_host: resolve_host_idx!(activation_site),
            had_to_consolidate: true,
        }
    };

    #[cfg(feature = "timing")]
    {
        let duration = start.elapsed();
        println!(
            "Took {}ns to service ({}us)",
            duration.as_nanos(),
            duration.as_micros()
        );
    }

    HttpResponse::Ok().json(response)
}

#[post("/consolidate")]
pub async fn consolidate_handler(intent: web::Json<ConsolidationIntent>) -> impl Responder {
    println!("[DEBUG] accepting consolidate");
    let object_dependencies: Vec<ObjectId> = intent.args.clone();
    let activation_site =
        match orchestration::force_consolidate(intent.to_host, &object_dependencies).await {
            Some((activation_site, _)) => activation_site,
            None => {
                return HttpResponse::Conflict().body(format!(
                    "failed to consolidate objects {:?} to {}",
                    object_dependencies, intent.to_host
                ));
            }
        };

    HttpResponse::Ok().body(format!(
        "consolidated redir {}",
        resolve_host_idx!(activation_site)
    ))
}

#[post("/register_worker")]
pub async fn register_worker_handler(
    register_worker_request: web::Json<RegisterWorkerRequest>,
) -> impl Responder {
    // FIXME the response here should also contain a snapshot of the worker and ownership caches,
    // given that the worker that submitted this request did so during startup and hence only has a
    // view of its local ownership.

    #[cfg(debug_assertions)]
    println!(
        "accepting registration of worker '{}'",
        register_worker_request.hostname
    );

    match orchestration::register_worker(
        register_worker_request.host_idx,
        register_worker_request.hostname.clone(),
    ) {
        Ok(idx) => HttpResponse::Ok().json(&RegisterWorkerResponse { host_idx: idx }),
        // FIXME match on error type
        Err(e) => {
            eprintln!("Could not register worker: {}", e);
            HttpResponse::InternalServerError()
                .body("idx was taken or something (pls fix this error)")
        }
    }
}

#[post("/publish_object")]
pub async fn publish_handler(publish_request: web::Json<PublishRequest>) -> impl Responder {
    let ownership_registry = registry::OwnershipRegistry::get_ownership_registry().clone();
    let object_id = publish_request.object.get_object_id().clone();
    let host_idx = publish_request.host_idx;

    #[cfg(debug_assertions)]
    println!("Accepting publish of {object_id} from host with idx {host_idx}");

    // FIXME error handling (once we change the return type)
    if !ownership_registry.handle_publish(object_id, host_idx) {
        // FIXME response type
        return HttpResponse::Conflict().body(format!("{} has already been published", object_id));
    }

    // FIXME response type
    HttpResponse::Ok().body(format!("publish of {} ok", object_id,))
}

#[post("/publish_objects")]
pub async fn multi_publish_handler(
    publish_request: web::Json<MultiPublishRequest>,
) -> impl Responder {
    let ownership_registry = registry::OwnershipRegistry::get_ownership_registry().clone();
    let object_ids: Vec<ObjectId> = publish_request
        .objects
        .iter()
        .map(|o| o.get_object_id())
        .collect();
    let host_idx = publish_request.host_idx;

    #[cfg(debug_assertions)]
    println!(
        "Accepting multi-publish of {:?} from host with idx {host_idx}",
        object_ids
    );

    for object_id in object_ids.into_iter() {
        if !ownership_registry.handle_publish(object_id, host_idx) {
            // FIXME
            continue;
        }
    }

    // FIXME response type
    HttpResponse::Ok().body("Multi-publish ok")
}

#[get("/worker_mapping")]
pub async fn get_worker_mapping_handler() -> impl Responder {
    println!("getting worker mapping");
    let worker_mapping = WorkerMapping {
        mapping: orchestration::get_hostname_projection(),
    };

    HttpResponse::Ok().json(&worker_mapping)
}

// Only used for benchmarking runs
#[put("/reset_state")]
pub async fn reset_state_handler() -> impl Responder {
    orchestration::reset_host_mgr_state();

    let ownership_registry = registry::OwnershipRegistry::get_ownership_registry().clone();
    ownership_registry.reset_state();

    HttpResponse::Ok()
}

#[put("/cache_single_object")]
pub async fn cache_object(intent: web::Json<ConsolidationIntent>) -> impl Responder {
    let object_to_cache: ObjectId = intent.args.get(0).unwrap().clone();
    let ownership_registry = registry::OwnershipRegistry::get_ownership_registry().clone();

    let owning_host_idx = ownership_registry
        .get_owning_host(&object_to_cache)
        .expect("attempt to cache non-published object");

    let host_projection = orchestration::get_hostname_projection();
    for (non_owner_host_idx, _non_owner_host) in &host_projection {
        if owning_host_idx == *non_owner_host_idx {
            continue;
        }

        let cached_object_id = vec![
            orchestration::request_cache_spawn(
                object_to_cache,
                owning_host_idx,
                *non_owner_host_idx,
            )
            .await,
        ];
        println!("Done spawning cache, will request move");
        let ownership_change_result = orchestration::request_ownership_change(
            &cached_object_id,
            owning_host_idx,
            *non_owner_host_idx,
            false,
        )
        .await;
        let whomstone_version = ownership_change_result.get(0).unwrap();

        orchestration::notify_add_cache_mapping(
            *non_owner_host_idx,
            object_to_cache,
            cached_object_id[0],
            whomstone_version.1,
            owning_host_idx,
        )
        .await;

        println!("Moved object {object_to_cache}");
    }
    println!("Done spreading caches of {object_to_cache} around");

    HttpResponse::Ok()
}

#[get("/location")]
pub async fn get_location_handler(
    intent: web::Json<NandoActivationIntentSerializable>,
) -> impl Responder {
    #[cfg(feature = "timing")]
    let start = tokio::time::Instant::now();
    let object_dependencies = intent.get_object_references();
    let response = 'response: {
        if object_dependencies.len() == 0 {
            // randomly pick a host
            // FIXME this should pick the least-busy host
            let host_idx = {
                let mut host_idx_rng = SmallRng::from_entropy();
                host_idx_rng.gen::<u64>() % orchestration::get_num_workers() as u64
            };

            break 'response ScheduleResponse {
                target_host: resolve_host_idx!(host_idx),
                had_to_consolidate: false,
            };
        }

        let ownership_registry = registry::OwnershipRegistry::get_ownership_registry().clone();
        #[cfg(not(feature = "always_move"))]
        if let Some(h) = ownership_registry.objects_are_colocated(&object_dependencies) {
            break 'response ScheduleResponse {
                target_host: resolve_host_idx!(h),
                had_to_consolidate: false,
            };
        } else {
            return HttpResponse::Conflict().body(format!(
                "objects {:?} are not colocated",
                object_dependencies
            ));
        }
    };

    #[cfg(feature = "timing")]
    {
        let duration = start.elapsed();
        println!(
            "Took {}ns to service ({}us)",
            duration.as_nanos(),
            duration.as_micros()
        );
    }

    HttpResponse::Ok().json(response)
}

#[get("/ownership_snapshot")]
pub async fn get_ownership_snapshot_handler() -> impl Responder {
    #[cfg(feature = "timing")]
    let start = tokio::time::Instant::now();

    let ownership_registry = registry::OwnershipRegistry::get_ownership_registry().clone();
    let state_snapshot = ownership_registry.get_state_snapshot();
    #[cfg(feature = "timing")]
    {
        let duration = start.elapsed();
        println!(
            "Took {}ns to service ({}us)",
            duration.as_nanos(),
            duration.as_micros()
        );
    }
    let response = OwnershipSnapshotResponse {
        snapshot: state_snapshot,
    };

    HttpResponse::Ok().json(response)
}

#[put("/peer_location_change")]
pub async fn peer_location_change_handler(
    intent: web::Json<ConsolidationIntent>,
) -> impl Responder {
    let object_to_move = intent.args.get(0).unwrap();
    let whomstone_version = intent.versions.get(0).unwrap();
    let target_host = intent.to_host;

    #[cfg(debug_assertions)]
    println!("Accepting peer location change of {object_to_move} to {target_host}");

    orchestration::peer_location_change(target_host, *object_to_move, *whomstone_version);

    HttpResponse::Ok()
}
