use std::collections::HashMap;
use std::mem::MaybeUninit;
use std::sync::Once;
use std::sync::{Arc, Mutex};
#[cfg(feature = "timing-ownership-transfer")]
use std::time::{SystemTime, UNIX_EPOCH};

use async_lib::AsyncRuntimeManager;
use async_std::sync::Arc as AsyncArc;
use execution_definitions::nando_handle::ActivationOutput;
#[cfg(any(feature = "observability", feature = "object-caching"))]
use lazy_static::lazy_static;
use location_manager::{HostId, LocationManager};
use nando_lib::{
    nando_scheduler::{NandoScheduler, TaskCompletionNotification},
    transaction_manager::TransactionManager,
};
use nando_support::{
    activation_intent, ecb_id::EcbId, epic_control, epic_definitions, iptr::IPtr, HostIdx,
};
use object_lib::{ObjectId, ObjectVersion};
use ownership_support as ownership;
use ownership_tracker::OwnershipTracker;
#[cfg(feature = "observability")]
use prometheus::{
    register_counter, register_counter_vec, register_histogram_vec, Counter, CounterVec, Encoder,
    HistogramVec,
};
use reqwest;
#[cfg(feature = "telemetry")]
use telemetry;
use tokio;

use crate::config;
use crate::net::rpc::worker_rpc_client;

#[cfg(feature = "observability")]
lazy_static! {
    static ref HTTP_AROUTER_EREQ_HISTOGRAM: HistogramVec = register_histogram_vec!(
        "activation_router_ereq_latency_microseconds",
        "Activation router outgoing request latencies in microseconds",
        &["path", "intent_name"],
        vec![10.0, 100.0, 1000.0, 10000.0],
    )
    .unwrap();
    static ref HTTP_AROUTER_CACHING_HISTOGRAM: HistogramVec = register_histogram_vec!(
        "activation_router_cache_ownership_latency_microseconds",
        "Activation router cache ownership change latencies in microseconds",
        &["status"],
        vec![10.0, 100.0, 1000.0, 10000.0],
    )
    .unwrap();
    static ref OWNERSHIP_SIGNATURE_CALCULATION_HISTOGRAM: HistogramVec = register_histogram_vec!(
        "activation_router_ownership_signature_calculation_milliseconds",
        "Activation router signature calculation latencies in milliseconds",
        &[],
        vec![0.1, 1.0, 10.0, 100.0],
    )
    .unwrap();
}

#[cfg(feature = "object-caching")]
lazy_static! {
    static ref IGNORE_CACHEABLE: bool = match std::env::var("MAGPIE_IGNORE_CACHEABLE") {
        Err(_) => false,
        Ok(_) => true,
    };
}

#[cfg(feature = "object-caching")]
enum CachePullResult {
    Done,
    NeedsRetry,
}

pub struct ActivationRouter {
    config: config::Config,
    host_id: HostId,
    // TODO consider making the `locationManager` the only component that can directly interface
    // with the ownership subsystem, for simplicity
    location_manager: AsyncArc<LocationManager>,

    scheduler_client: Arc<reqwest::Client>,
    worker_client: Arc<worker_rpc_client::WorkerRpcClient>,

    host_idx: Arc<Mutex<Option<u64>>>,
    async_rt_manager: Arc<AsyncRuntimeManager>,

    #[cfg(feature = "telemetry")]
    telemetry_handle: telemetry::TelemetryEventSender,
}

impl ActivationRouter {
    pub fn new(
        config: config::Config,
        host_id: HostId,
        location_manager: AsyncArc<LocationManager>,
        async_rt_manager: Arc<AsyncRuntimeManager>,
    ) -> Self {
        let rpc_server_port = config.worker_rpc_server_port.clone();

        #[cfg(feature = "telemetry")]
        let telemetry_handle = telemetry::get_telemetry_handle();

        Self {
            config,
            host_id,
            location_manager,
            scheduler_client: Arc::new(
                reqwest::Client::builder()
                    .pool_idle_timeout(None)
                    .build()
                    .unwrap(),
            ),
            worker_client: Arc::new(worker_rpc_client::WorkerRpcClient::new(rpc_server_port)),
            host_idx: Arc::new(Mutex::new(None)),
            async_rt_manager,

            #[cfg(feature = "telemetry")]
            telemetry_handle,
        }
    }

    #[cfg(feature = "telemetry")]
    #[inline(always)]
    fn submit_telemetry_event(&self, event: telemetry::TelemetryEvent) {
        telemetry::submit_telemetry_event(&self.telemetry_handle, event);
    }

    #[cfg(feature = "offline")]
    pub fn healthcheck_all_threads(&self) {}

    #[cfg(not(feature = "offline"))]
    pub fn healthcheck_all_threads(&self) {
        let rt_handle = Arc::clone(&self.async_rt_manager);
        for rt in rt_handle.runtime_iter() {
            let figaro_client = self.scheduler_client.clone();
            let activation_router = Self::get_activation_router(None, None, None, None);
            rt.block_on(async move {
                figaro_client
                    .get(activation_router.get_scheduler_url("healthcheck"))
                    .send()
                    .await
                    .expect("failed to send healthcheck");
            });
        }
    }

    pub fn get_activation_router(
        config: Option<config::Config>,
        host_id: Option<HostId>,
        location_manager: Option<AsyncArc<LocationManager>>,
        async_rt_manager: Option<Arc<AsyncRuntimeManager>>,
    ) -> &'static Arc<ActivationRouter> {
        static mut INSTANCE: MaybeUninit<Arc<ActivationRouter>> = MaybeUninit::uninit();
        static mut ONCE: Once = Once::new();

        unsafe {
            ONCE.call_once(|| {
                let config = config.expect(
                    "cannot instantiate global coordinator without a valid configuration"
                );
                let host_id = host_id.expect(
                    "cannot instantiate global coordinator without a valid host id"
                );
                let location_manager = location_manager.expect(
                    "cannot instantiate global coordinator without an active LocationManager instance"
                );
                let async_rt_manager = async_rt_manager.expect(
                    "cannot instantiate global coordinator without an async runtime manager instance"
                );
                INSTANCE
                    .as_mut_ptr()
                    .write(Arc::new(ActivationRouter::new(config, host_id, location_manager, async_rt_manager)));
            });
        }

        unsafe { &*INSTANCE.as_ptr() }
    }

    pub fn register_callback_fns(&self) {
        self.register_task_fwd_fn();
        self.register_tasks_fwd_to_fn();
        self.register_task_completion_fwd_fn();
    }

    pub fn register_task_fwd_fn(&self) {
        let transaction_manager = TransactionManager::get_transaction_manager_mut(None, None);
        transaction_manager.set_task_fwd_fn(Box::new(
            |spawned_task: epic_control::SpawnedTask, maybe_location: Option<HostId>| {
                let activation_router =
                    ActivationRouter::get_activation_router(None, None, None, None).clone();
                activation_router.async_rt_manager.spawn(async move {
                    let activation_router =
                        ActivationRouter::get_activation_router(None, None, None, None).clone();
                    if let Some(location) = maybe_location {
                        activation_router
                            .forward_spawned_task_to(spawned_task, location)
                            .await;
                    } else {
                        activation_router.forward_spawned_task(spawned_task).await;
                    }
                });
            },
        ));
    }

    pub fn register_tasks_fwd_to_fn(&self) {
        let transaction_manager = TransactionManager::get_transaction_manager_mut(None, None);
        transaction_manager.set_tasks_fwd_to_fn(Box::new(
            |spawned_tasks: Vec<epic_control::SpawnedTask>,
             spawned_task_locations: HashMap<HostId, Vec<EcbId>>,
             location: Option<HostId>| {
                let activation_router =
                    ActivationRouter::get_activation_router(None, None, None, None).clone();
                activation_router.async_rt_manager.spawn(async move {
                    let activation_router =
                        ActivationRouter::get_activation_router(None, None, None, None).clone();
                    activation_router
                        .forward_spawned_tasks_to(spawned_tasks, spawned_task_locations, location)
                        .await;
                });
            },
        ));
    }

    pub fn register_task_completion_fwd_fn(&self) {
        let transaction_manager = TransactionManager::get_transaction_manager_mut(None, None);
        transaction_manager.set_task_completion_fwd_fn(Box::new(
            |task_completion_notification: TaskCompletionNotification| {
                let activation_router =
                    ActivationRouter::get_activation_router(None, None, None, None).clone();
                activation_router.async_rt_manager.spawn(async move {
                    let activation_router =
                        ActivationRouter::get_activation_router(None, None, None, None).clone();
                    activation_router
                        .forward_task_completion(task_completion_notification)
                        .await
                });
            },
        ));
    }

    pub async fn healthcheck(&self) -> &'static str {
        "Activation Router instance is running"
    }

    // FIXME this is too similar to `OwnershipTracker::args_are_resolvable_locally()`, merge them?
    async fn can_execute_locally(
        &self,
        activation_intent: &mut activation_intent::NandoActivationIntent,
        rewrite_cached_args: bool,
    ) -> bool {
        let ownership_tracker = OwnershipTracker::get_ownership_tracker(None);
        let intent_args = &mut activation_intent.args;

        let intent_metadata =
            match NandoScheduler::get_nando_metadata_external(&activation_intent.name) {
                Err(e) => {
                    eprintln!(
                        "Could not decide if the request for '{}' can be satisfied locally: {}",
                        activation_intent.name, e
                    );
                    return false;
                }
                Ok(meta) => meta,
            };

        let intent_is_readonly = intent_metadata.kind.is_read_only();
        let mutable_argument_indices = match intent_metadata.mutable_argument_indices {
            None => &[],
            Some(m) => m,
        };

        let mut cache_rewrites = Vec::with_capacity(intent_args.len());
        for (idx, arg) in intent_args.iter().enumerate() {
            // let activation_intent::NandoArgumentSerializable::Ref(dependency) = arg else {
            let activation_intent::NandoArgument::Ref(dependency) = arg else {
                continue;
            };

            let dependency = if !intent_is_readonly && mutable_argument_indices.contains(&idx) {
                dependency.object_id
            } else {
                match ownership_tracker.get_cache_mapping_if_valid(dependency.object_id) {
                    None => {
                        #[cfg(debug_assertions)]
                        println!("no valid cache mapping for {}", dependency.object_id);
                        dependency.object_id
                    }
                    Some(c) => {
                        #[cfg(debug_assertions)]
                        println!("valid cache mapping for {}: {}", dependency.object_id, c);
                        cache_rewrites.push((
                            idx,
                            activation_intent::NandoArgument::Ref(IPtr::new(c, 0, 0)),
                        ));
                        c
                    }
                }
            };

            if ownership_tracker.object_is_owned(dependency) {
                #[cfg(debug_assertions)]
                println!("[DEBUG] Object {} is owned", dependency);
                continue;
            }

            if ownership_tracker.object_is_incoming(dependency) {
                #[cfg(debug_assertions)]
                println!("[DEBUG] Object {} is incoming, will await", dependency);
                // TODO move the waiting, this function should only be a quick check
                self.location_manager.wait_for_object(dependency).await;
                continue;
            }

            return false;
        }

        // Update cached dependencies with correct object IDs so that the executors can proceed to
        // resolve without consulting the ownership tracker's cache mapping.
        // Not the best thing to do in a method whose name suggests a simple check, but...
        if rewrite_cached_args {
            for (rewrite_idx, iptr) in cache_rewrites {
                intent_args[rewrite_idx] = iptr;
            }
        }

        true
    }

    #[inline]
    fn get_scheduler_url(&self, endpoint: &str) -> String {
        format!(
            "http://{}:{}/{}",
            self.config.scheduler_config.host, self.config.scheduler_config.port, endpoint,
        )
    }

    #[inline]
    fn get_worker_url(&self, target_host: &HostId, endpoint: &str) -> String {
        format!(
            "http://{}:52017/activation_router/{}",
            target_host, endpoint,
        )
    }

    async fn ask_scheduler(
        &self,
        activation_intent_request: &activation_intent::NandoActivationIntent,
    ) -> HostId {
        let ownership_tracker = OwnershipTracker::get_ownership_tracker(None);
        let refs = activation_intent_request.get_object_references();
        if let Some(host_id) = ownership_tracker.compute_activation_site(&refs) {
            if host_id != self.host_id {
                return host_id;
            }
        }
        let client = self.scheduler_client.clone();
        let mut serializable_intent =
            activation_intent::NandoActivationIntentSerializable::from(activation_intent_request);
        serializable_intent.host_idx = OwnershipTracker::get_host_idx_static(None);
        let intent_metadata =
            match NandoScheduler::get_nando_metadata_external(&activation_intent_request.name) {
                Err(e) => {
                    panic!(
                        "failed to get meta for {}: {e}",
                        activation_intent_request.name
                    );
                }
                Ok(meta) => meta,
            };

        let scheduler_intent = activation_intent::SchedulerIntent {
            intent: serializable_intent,
            mutable_argument_indices: match intent_metadata.mutable_argument_indices {
                None => vec![],
                Some(i) => i.iter().map(|i| *i).collect(),
            },
        };

        let response = client
            .post(self.get_scheduler_url("schedule"))
            .json(&scheduler_intent)
            .send()
            .await
            .expect("failed to forward activation intent to scheduler");
        assert!(response.status().is_success());

        let parsed_response = response
            .json::<ownership_support::ScheduleResponse>()
            .await
            .expect("failed to parse scheduler response");

        if parsed_response.target_host != self.host_id {
            for object_id in activation_intent_request.get_object_references() {
                #[cfg(debug_assertions)]
                println!(
                    "caching location of {object_id}: {}",
                    parsed_response.target_host.clone()
                );
                ownership_tracker.insert_mapping_in_ownership_map(
                    object_id,
                    parsed_response.target_host.clone(),
                );
            }
        }
        parsed_response.target_host
    }

    async fn ask_scheduler_for_location(
        &self,
        activation_intent_request: &activation_intent::NandoActivationIntent,
    ) -> HostId {
        let ownership_tracker = OwnershipTracker::get_ownership_tracker(None);
        let refs = activation_intent_request.get_object_references();
        if let Some(host_id) = ownership_tracker.compute_activation_site(&refs) {
            if host_id != self.host_id {
                return host_id;
            }
        }
        let client = self.scheduler_client.clone();
        let mut scheduler_intent =
            activation_intent::NandoActivationIntentSerializable::from(activation_intent_request);
        scheduler_intent.host_idx = OwnershipTracker::get_host_idx_static(None);
        let response = client
            .get(self.get_scheduler_url("location"))
            .json(&scheduler_intent)
            .send()
            .await
            .expect("failed to forward activation intent to scheduler");
        assert!(response.status().is_success());

        let parsed_response = response
            .json::<ownership_support::ScheduleResponse>()
            .await
            .expect("failed to parse scheduler response");

        if parsed_response.target_host != self.host_id {
            for object_id in activation_intent_request.get_object_references() {
                #[cfg(debug_assertions)]
                println!(
                    "caching location of {object_id}: {}",
                    parsed_response.target_host.clone()
                );
                ownership_tracker.insert_mapping_in_ownership_map(
                    object_id,
                    parsed_response.target_host.clone(),
                );
            }
        }
        parsed_response.target_host
    }

    async fn forward_schedule_request(
        &self,
        activation_intent_request: &activation_intent::NandoActivationIntent,
        target_host: HostId,
        // ) -> activation_intent::NandoActivationResolution {
    ) -> (Vec<ActivationOutput>, Vec<(ObjectId, ObjectVersion)>) {
        let client = self.worker_client.clone();
        let mut activation_intent_request = activation_intent_request.clone();
        activation_intent_request.host_idx = OwnershipTracker::get_host_idx_static(None);

        match client
            .schedule_nando(activation_intent_request, &target_host)
            .await
        {
            Err(e) => {
                eprintln!("failed to fwd nando: {}", e);
                todo!("failed to fwd nando: {}", e);
            }
            Ok(res) => res,
        }
    }

    pub async fn try_execute_nando(
        &self,
        activation_intent_request: activation_intent::NandoActivationIntent,
        await_epic_result: bool,
        with_plan: Option<String>,
    ) -> Result<
        (Vec<ActivationOutput>, Vec<(ObjectId, ObjectVersion)>),
        (String, Option<ActivationOutput>),
    > {
        #[cfg(feature = "timing-svc-latency")]
        let svc_start = tokio::time::Instant::now();

        #[cfg(not(feature = "offline"))]
        let mut activation_intent_request = activation_intent_request.clone();
        #[cfg(feature = "offline")]
        let activation_intent_request = activation_intent_request.clone();

        #[cfg(not(feature = "offline"))]
        let activation_intent_name = activation_intent_request.name.clone();

        #[cfg(not(feature = "offline"))]
        {
            if activation_intent_name == "invalidate" {
                match self.invalidate_intent_dependencies(&mut activation_intent_request) {
                    Err(e) => return Err((e, None)),
                    Ok(r) => return Ok(r),
                }
            }

            if !self
                .can_execute_locally(&mut activation_intent_request, true)
                .await
            {
                // TODO try to compute activation location from local cache before sending a scheduler
                // request.
                let dependency_object_ids = activation_intent_request.get_object_references();
                #[cfg(debug_assertions)]
                println!("[DEBUG] Cannot execute nando locally, will contact scheduler");
                let target_host = self.ask_scheduler(&activation_intent_request).await;
                if target_host != self.host_id {
                    // TODO @physical add optional plan parameter
                    let response = self
                        .forward_schedule_request(&activation_intent_request, target_host.clone())
                        .await;

                    self.spawn_caching_tasks(target_host, &response.1).await;

                    return Ok((response.0, vec![]));
                }

                for object_id in dependency_object_ids {
                    #[cfg(debug_assertions)]
                    println!("[DEBUG] Will wait for {}", object_id);
                    self.location_manager.wait_for_object(object_id).await;
                }
            }
        }

        let activation_intent = activation_intent_request.clone();

        // if ok, pass activation (intent) to TM
        let transaction_manager = TransactionManager::get_transaction_manager(None, None);
        match transaction_manager
            .execute_nando(activation_intent, await_epic_result, with_plan)
            .await
        {
            Ok((r, c)) => {
                #[cfg(feature = "object-caching")]
                if activation_intent_name == "spawn_cache" && !r.is_empty() {
                    let original_object_id = activation_intent_request
                        .args
                        .get(0)
                        .unwrap()
                        .get_object_id()
                        .unwrap();
                    let ownership_tracker = OwnershipTracker::get_ownership_tracker(None);
                    let activation_intent_host_idx = activation_intent_request.host_idx.clone();
                    let target_host_idx = activation_intent_host_idx
                        .expect("caching request did not include a host idx");
                    let cache_id = match r.first() {
                        None => panic!("cannot happen"),
                        Some(ActivationOutput::Result(activation_intent::NandoResult::Ref(
                            cache_iptr,
                        ))) => cache_iptr.object_id,
                        _ => panic!("unrecognized argument in spawn_cache response"),
                    };

                    let cache_version = match r.last() {
                        None => panic!("cannot happen"),
                        Some(ActivationOutput::Result(activation_intent::NandoResult::Value(
                            v,
                        ))) => v.try_into().unwrap(),
                        _ => panic!("unrecognized argument in spawn_cache response"),
                    };

                    match ownership_tracker.add_shared_cache_entry(
                        original_object_id,
                        cache_id,
                        cache_version,
                        target_host_idx,
                    ) {
                        Ok(()) => {}
                        Err(()) => return Ok((vec![], vec![])),
                    }
                }

                #[cfg(feature = "timing-svc-latency")]
                {
                    let svc_duration = svc_start.elapsed();
                    println!(
                        "Execution of '{}' took {}us ({}ns)",
                        activation_intent_name,
                        svc_duration.as_micros(),
                        svc_duration.as_nanos()
                    );
                }

                // Ok((r.iter().map(|r| r.into()).collect(), c))
                Ok((r, c))
            }
            Err(e) => Err(e),
        }
    }

    pub fn add_cache_entry(
        &self,
        original_object_id: ObjectId,
        cache_id: ObjectId,
        cache_version: ObjectVersion,
        target_host_idx: HostIdx,
    ) -> Result<(), ()> {
        let ownership_tracker = OwnershipTracker::get_ownership_tracker(None);
        ownership_tracker.add_shared_cache_entry(
            original_object_id,
            cache_id,
            cache_version,
            target_host_idx,
        )
    }

    fn invalidate_intent_dependencies(
        &self,
        invalidation_intent: &mut activation_intent::NandoActivationIntent,
    ) -> Result<(Vec<ActivationOutput>, Vec<(ObjectId, ObjectVersion)>), String> {
        #[cfg(debug_assertions)]
        println!(
            "[DEBUG] got request to invalidate: {:#?}",
            invalidation_intent
        );
        let invalidation_args = &mut invalidation_intent.args;
        let cached_object_version: u64 = match invalidation_args.pop() {
            None => return Err("could not get cached object version".to_string()),
            Some(v) => match v {
                activation_intent::NandoResult::Value(ref v) => {
                    v.try_into().expect("failed to parse version")
                }
                _ => return Err("unexpected argument in place of object version".to_string()),
            },
        };
        let cached_object_id = match invalidation_args.pop() {
            None => return Err("could not get cached object id".to_string()),
            Some(oid) => match oid {
                activation_intent::NandoResult::Ref(iptr) => iptr.object_id,
                _ => return Err("unexpected argument in place of object id".to_string()),
            },
        };
        let original_object_id = match invalidation_args.pop() {
            None => return Err("could not get original object id of cached object".to_string()),
            Some(oid) => match oid {
                activation_intent::NandoResult::Ref(iptr) => iptr.object_id,
                _ => return Err("unexpected argument in place of object id".to_string()),
            },
        };

        let ownership_tracker = OwnershipTracker::get_ownership_tracker(None);
        match ownership_tracker.get_cache_mapping_if_valid(original_object_id) {
            None => {}
            Some(stored_cached_object_id) => {
                if stored_cached_object_id != cached_object_id {
                    #[cfg(debug_assertions)]
                    println!(
                        "Got request to invalidate {} but local mapping of {} is {}",
                        cached_object_id, original_object_id, stored_cached_object_id
                    );
                }

                // NOTE whomstone versions should not matter for cached objects, but we might need
                // to mark as migrated instead of whomstoned.
                ownership_tracker.mark_whomstoned(stored_cached_object_id, cached_object_version);
            }
        }
        ownership_tracker.mark_invalidated(
            original_object_id,
            cached_object_id,
            cached_object_version,
        );

        Ok((vec![], vec![]))
    }

    #[cfg(feature = "object-caching")]
    async fn forward_invalidations(
        &self,
        invalidation_task: epic_control::SpawnedTask,
    ) -> Result<(), String> {
        let ownership_tracker = OwnershipTracker::get_ownership_tracker(None);
        let tasks = match invalidation_task.intent.args.len() == 1 {
            true => {
                let original_object_id = invalidation_task
                    .intent
                    .args
                    .get(0)
                    .expect("invalid num args")
                    .get_object_id()
                    .expect("invalidate arg not an object ref");

                let shared_caches =
                    ownership_tracker.get_shared_caches_for_object(original_object_id);

                shared_caches
                    .iter()
                    .map(|(cache_id, version)| {
                        epic_control::create_invalidation_task(
                            &invalidation_task,
                            original_object_id,
                            *cache_id,
                            *version,
                        )
                    })
                    .collect()
            }
            false => vec![invalidation_task.clone()],
        };

        let host_idx = OwnershipTracker::get_host_idx_static(None);
        let client = self.worker_client.clone();

        let mut can_complete_immediately = false;
        for mut spawned_task in tasks.into_iter() {
            #[cfg(debug_assertions)]
            println!(
                "Will attempt to forward invalidation task {:#?}",
                spawned_task
            );
            let original_object_id = spawned_task
                .intent
                .args
                .get(0)
                .unwrap()
                .get_object_id()
                .unwrap();

            let target_object_iptr = match spawned_task
                .intent
                .args
                .get(1)
                .expect("invalid number of arguments to 'invalidate' spawned task")
            {
                activation_intent::NandoArgument::Ref(target_object_iptr) => {
                    // NOTE the below ownership change is necessary to be able to schedule a cache
                    // re-spawn task from the same host in the future. We can avoid this by making
                    // object ids explicitly passable as a separate argument/result type, in which
                    // case they will not be considered by the dependency ownership check, but that
                    // seems like a recipe for disaster.
                    let _ = self
                        .location_manager
                        .get_object_tracker()
                        .open(target_object_iptr.object_id)
                        .expect("failed to open recycled cache object");
                    ownership_tracker.mark_owned(target_object_iptr.object_id);

                    target_object_iptr
                }
                _ => panic!("'invalidate' spawned task missing cached object argument"),
            };

            let Some((_object_id, host_id)) = ownership_tracker
                .invalidate_or_remove_shared_cache_entry(
                    original_object_id,
                    target_object_iptr.object_id,
                )
            else {
                #[cfg(debug_assertions)]
                println!("invalidated entry, will skip");

                if invalidation_task.intent.args.len() == 1 {
                    can_complete_immediately = true;
                }

                continue;
            };

            spawned_task.intent.host_idx = host_idx;

            #[cfg(feature = "observability")]
            let req_start = tokio::time::Instant::now();
            let _response = client
                .forward_spawned_task(spawned_task, &host_id)
                .await
                .expect("failed to fwd invalidation task");
            #[cfg(feature = "observability")]
            {
                let req_duration = req_start.elapsed();
                HTTP_AROUTER_EREQ_HISTOGRAM
                    .with_label_values(&[
                        "/activation_router/schedule_spawned_task",
                        &spawned_task.intent.name,
                    ])
                    .observe(req_duration.as_micros() as f64);
            }
        }

        #[cfg(debug_assertions)]
        println!("Done forwarding invalidation tasks");

        if can_complete_immediately {
            let transaction_manager = TransactionManager::get_transaction_manager(None, None);
            let tasks_to_notify = match invalidation_task.should_notify_parent() {
                true => {
                    let parent_dep = invalidation_task.get_parent_as_downstream_dependency();
                    if parent_dep.is_none() {
                        Vec::default()
                    } else {
                        vec![(parent_dep.clone(), epic_control::TaskStatus::Success, None)]
                    }
                }
                false => invalidation_task
                    .downstream_dependents
                    .iter()
                    .map(|t| (t.clone(), epic_control::TaskStatus::Success, None))
                    .collect(),
            };

            transaction_manager
                .handle_task_completion(invalidation_task.id, tasks_to_notify)
                .await;
        }

        Ok(())
    }

    pub async fn try_execute_spawned_task(
        &self,
        mut spawned_task: epic_control::SpawnedTask,
    ) -> Result<(Vec<ActivationOutput>, Vec<(ObjectId, ObjectVersion)>), String> {
        #[cfg(debug_assertions)]
        println!(
            "Received spawned task {}: {:#?}",
            spawned_task.id, spawned_task
        );
        let transaction_manager = TransactionManager::get_transaction_manager(None, None);

        let ownership_tracker = OwnershipTracker::get_ownership_tracker(None);
        let source_host_idx = spawned_task
            .intent
            .host_idx
            .expect("no host idx found for remote spawned task");

        if let Some(parent_ecb_id) = spawned_task.parent_task.get_inner_ecb_id() {
            ownership_tracker.insert_control_block_entry(parent_ecb_id, source_host_idx);
        }

        for downstream_dependent in &spawned_task.downstream_dependents {
            match downstream_dependent {
                epic_control::DownstreamTaskDependency::None => (),
                epic_control::DownstreamTaskDependency::DataDependency(ref dep_ref, _)
                | epic_control::DownstreamTaskDependency::ControlDependency(ref dep_ref)
                | epic_control::DownstreamTaskDependency::ParentDependency(ref dep_ref) => {
                    ownership_tracker.insert_control_block_entry(
                        dep_ref.get_inner_ecb_id().unwrap(),
                        source_host_idx,
                    );
                }
            }
        }

        if spawned_task.intent.is_whomstone_intent()
            || spawned_task.intent.is_whomstone_and_move_intent()
        {
            let figaro_client = self.scheduler_client.clone();
            let target_host_idx = source_host_idx;
            let target_host = ownership_tracker
                .get_host_id_for_idx(target_host_idx)
                .expect("no host mapping for idx");

            let dependencies = match spawned_task.should_notify_parent() {
                true => vec![spawned_task.get_parent_as_downstream_dependency()],
                false => spawned_task.downstream_dependents.clone(),
            };

            ownership_tracker.insert_control_block_entry(spawned_task.id, target_host_idx);

            let target_object = spawned_task.intent.args[0]
                .get_object_id()
                .expect("missing object id in ownership change intent");

            let whomstone_version = match spawned_task.intent.is_whomstone_and_move_intent() {
                true => self
                    .whomstone_and_move_object(target_object, target_host.clone())
                    .await
                    .expect("failed to change ownership and move object"),
                false => {
                    self.whomstone_object(target_object, target_host.clone(), false, None)
                        .await
                        .expect("failed to change ownership")
                        .0
                }
            };

            // NOTE if whomstone_and_move, then we need to notify the ownership orchestrator that
            // the transfer might be ongoing
            let mut notified = false;
            for _ in 0..5 {
                match figaro_client
                    .put(self.get_scheduler_url("peer_location_change"))
                    .json(&ownership::ConsolidationIntent {
                        to_host: target_host_idx as u64,
                        args: vec![target_object],
                        versions: vec![whomstone_version],
                    })
                    .send()
                    .await
                {
                    Ok(response) => {
                        if response.status().is_success() {
                            notified = true;
                            break;
                        }

                        eprintln!(
                            "Failed to notify of peer location change, will try again: {:?}",
                            response
                        );
                        tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;
                    }
                    Err(e) => {
                        eprintln!("Failed to notify of peer location change, will try again: {e}");
                        tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;
                    }
                }
            }

            if !notified {
                // FIXME
                panic!("Failed to notify of peer location change");
            }

            let dependencies = dependencies
                .into_iter()
                .map(|d| {
                    (
                        d,
                        // FIXME can't assume whomstone task will always succeed
                        epic_control::TaskStatus::Success,
                        Some(activation_intent::NandoResult::from_object_id(
                            target_object,
                        )),
                    )
                })
                .collect();
            self.forward_task_completion(TaskCompletionNotification::new(
                spawned_task.id,
                dependencies,
            ))
            .await;

            return Ok((vec![], vec![]));
        }

        let rewrite_cached_args = spawned_task.intent.name != "invalidate";
        if !self
            .can_execute_locally(&mut spawned_task.intent, rewrite_cached_args)
            .await
        {
            return Err(format!("cannot execute {:#?} locally", spawned_task));
        }

        ownership_tracker.insert_control_block_entry(spawned_task.id, source_host_idx);

        if spawned_task.intent.name == "invalidate" {
            #[cfg(feature = "telemetry")]
            {
                let mut invalidation_task = spawned_task.clone();
                invalidation_task.downgrade_parent_dependency();
                let telemetry_ts = telemetry::zoned_timestamp_now();
                self.submit_telemetry_event(telemetry::TelemetryEvent::new_schedule(
                    invalidation_task.id,
                    0,
                    telemetry_ts,
                ));
            }

            let mut activation_intent =
                activation_intent::NandoActivationIntent::from(spawned_task.intent.clone());
            let invalidation_response = self.invalidate_intent_dependencies(&mut activation_intent);
            self.trigger_invalidation_task_completion(spawned_task)
                .await;

            return invalidation_response;
        }

        match transaction_manager.execute_spawned_task(spawned_task).await {
            Ok((r, c)) => Ok((r, c)),
            Err(e) => return Err(e),
        }
    }

    pub async fn update_ownership_cache(&self, ownership_tracker: &OwnershipTracker) {
        let client = self.scheduler_client.clone();
        let response = client
            .get(self.get_scheduler_url("ownership_snapshot"))
            .send()
            .await
            .expect("failed to get ownership snapshot from scheduler");
        assert!(response.status().is_success(), "{:#?}", response);
        let parsed_response = response
            .json::<ownership_support::OwnershipSnapshotResponse>()
            .await
            .expect("failed to parse scheduler response");

        ownership_tracker.update_remote_ownership(&parsed_response.snapshot);
    }

    pub fn update_task_ownership(&self, spawned_task_locations: HashMap<String, Vec<EcbId>>) {
        let ownership_tracker = OwnershipTracker::get_ownership_tracker(None);
        let own_host_id = ownership_tracker.get_own_host_id().unwrap();
        for (location, owned_task_ids) in spawned_task_locations.into_iter() {
            if location == own_host_id {
                continue;
            }

            ownership_tracker.insert_control_block_entries_with_id(&owned_task_ids, location);
        }
    }

    pub async fn try_schedule_task_graph(
        &self,
        spawned_tasks: Vec<epic_control::SpawnedTask>,
        spawned_task_locations: HashMap<String, Vec<EcbId>>,
    ) -> Result<Vec<(Vec<ActivationOutput>, Vec<(ObjectId, ObjectVersion)>)>, String> {
        assert!(!spawned_tasks.is_empty());
        #[cfg(debug_assertions)]
        println!(
            "Received a subgraph with {} spawned tasks: {:#?}",
            spawned_tasks.len(),
            spawned_tasks
        );
        let transaction_manager = TransactionManager::get_transaction_manager(None, None);

        let ownership_tracker = OwnershipTracker::get_ownership_tracker(None);

        if !spawned_task_locations.is_empty() {
            self.update_task_ownership(spawned_task_locations);
        }

        let (local_whomstone_tasks, mut spawned_tasks): (Vec<epic_control::SpawnedTask>, _) =
            spawned_tasks.into_iter().partition(|st| {
                if !(st.intent.is_whomstone_intent() || st.intent.is_whomstone_and_move_intent()) {
                    return false;
                }

                if st.intent.has_unresolved_args() {
                    return false;
                }

                // assume that we can execute all tasks for now.
                true
            });

        let unschedulable_tasks_end_idx = spawned_tasks.iter_mut().partition_in_place(|t| {
            // FIXME here we assume that the present host will eventually be able to execute all tasks in the
            // subgraph locally, this might not be the case (e.g. in case we were sent a whomstone
            // task to fwd to the actual owner of the object).
            !t.is_schedulable()
        });

        let mut to_await = Vec::with_capacity(local_whomstone_tasks.len());

        for local_whomstone_task in local_whomstone_tasks.into_iter() {
            let (target_host_idx, target_host) =
                match local_whomstone_task.downstream_dependents.get(0) {
                    None => {
                        let target_host_idx = local_whomstone_task
                            .intent
                            .host_idx
                            .expect("whomstone without source host idx");
                        let target_host = ownership_tracker
                            .get_host_id_for_idx(target_host_idx)
                            .expect("no host mapping for idx");
                        ownership_tracker
                            .insert_control_block_entry(local_whomstone_task.id, target_host_idx);

                        (target_host_idx, target_host)
                    }
                    Some(d) => {
                        let owner = ownership_tracker
                            .get_control_block_entry(d.get_inner_ecb_id().unwrap())
                            .unwrap();
                        let owner_idx = ownership_tracker.get_host_idx_for_id(&owner).unwrap();

                        (owner_idx, owner)
                    }
                };
            #[cfg(feature = "telemetry")]
            let local_whomstone_task_id = local_whomstone_task.id;
            #[cfg(feature = "telemetry")]
            {
                let telemetry_ts = telemetry::zoned_timestamp_now();
                self.submit_telemetry_event(telemetry::TelemetryEvent::new_schedule(
                    local_whomstone_task_id,
                    0,
                    telemetry_ts.clone(),
                ));
            }

            let figaro_client = self.scheduler_client.clone();
            let target_host = target_host.clone();
            let activation_router =
                ActivationRouter::get_activation_router(None, None, None, None).clone();

            let handle = self.async_rt_manager.spawn(async move {
                let dependencies = match local_whomstone_task.should_notify_parent() {
                    true => vec![local_whomstone_task.get_parent_as_downstream_dependency()],
                    false => local_whomstone_task.downstream_dependents.clone(),
                };

                let target_object = local_whomstone_task.intent.args[0]
                    .get_object_id()
                    .expect("missing object id in ownership change intent");

                let whomstone_version =
                    match local_whomstone_task.intent.is_whomstone_and_move_intent() {
                        true => activation_router
                            .whomstone_and_move_object(target_object, target_host.clone())
                            .await
                            .expect("failed to change ownership and move object"),
                        false => {
                            activation_router
                                .whomstone_object(target_object, target_host.clone(), false, None)
                                .await
                                .expect("failed to change ownership")
                                .0
                        }
                    };

                // NOTE if whomstone_and_move, then we need to notify the ownership orchestrator that
                // the transfer might be ongoing
                let mut notified = false;
                for _ in 0..5 {
                    match figaro_client
                        .put(activation_router.get_scheduler_url("peer_location_change"))
                        .json(&ownership::ConsolidationIntent {
                            to_host: target_host_idx as u64,
                            args: vec![target_object],
                            versions: vec![whomstone_version],
                        })
                        .send()
                        .await
                    {
                        Ok(response) => {
                            if response.status().is_success() {
                                notified = true;
                                break;
                            }

                            eprintln!(
                                "Failed to notify of peer location change, will try again: {:?}",
                                response
                            );
                            tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;
                        }
                        Err(e) => {
                            eprintln!(
                                "Failed to notify of peer location change, will try again: {e}"
                            );
                            tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;
                        }
                    }
                }

                if !notified {
                    // FIXME
                    panic!("Failed to notify of peer location change");
                }

                #[cfg(feature = "telemetry")]
                {
                    let telemetry_ts = telemetry::zoned_timestamp_now();
                    activation_router.submit_telemetry_event(
                        telemetry::TelemetryEvent::new_commit(
                            local_whomstone_task_id,
                            telemetry_ts,
                        ),
                    );
                }

                let dependencies = dependencies
                    .into_iter()
                    .map(|d| {
                        (
                            d,
                            // FIXME can't assume whomstone task will always succeed
                            epic_control::TaskStatus::Success,
                            Some(activation_intent::NandoResult::from_object_id(
                                target_object,
                            )),
                        )
                    })
                    .collect();
                activation_router
                    .forward_task_completion(TaskCompletionNotification::new(
                        local_whomstone_task.id,
                        dependencies,
                    ))
                    .await;
            });
            to_await.push(handle);
        }

        futures::future::join_all(to_await).await;

        let mut schedulable_task_results =
            Vec::with_capacity(spawned_tasks.len() - unschedulable_tasks_end_idx);
        for (task_idx, mut spawned_task) in spawned_tasks.drain(..).enumerate() {
            let source_host_idx = spawned_task
                .intent
                .host_idx
                .expect("no host idx found for remote spawned task");

            if let Some(parent_ecb_id) = spawned_task.parent_task.get_inner_ecb_id() {
                ownership_tracker.insert_control_block_entry(parent_ecb_id, source_host_idx);
            }

            // FIXME remove -- why do we need this?
            // ownership_tracker.insert_control_block_entry(spawned_task.id, source_host_idx);
            let rewrite_cached_args = spawned_task.intent.name != "invalidate";
            let can_execute_locally = self
                .can_execute_locally(&mut spawned_task.intent, rewrite_cached_args)
                .await;

            if task_idx < unschedulable_tasks_end_idx {
                transaction_manager.store_spawned_task(spawned_task);
            } else {
                if spawned_task.intent.is_invalidation_intent() {
                    #[cfg(feature = "telemetry")]
                    let task_id = spawned_task.id;

                    #[cfg(feature = "telemetry")]
                    {
                        let telemetry_ts = telemetry::zoned_timestamp_now();
                        self.submit_telemetry_event(telemetry::TelemetryEvent::new_schedule(
                            task_id,
                            0,
                            telemetry_ts,
                        ));
                    }

                    let mut activation_intent =
                        activation_intent::NandoActivationIntent::from(spawned_task.intent.clone());
                    let invalidation_response =
                        self.invalidate_intent_dependencies(&mut activation_intent);

                    match invalidation_response {
                        Ok(ir) => schedulable_task_results.push(ir),
                        Err(e) => return Err(e),
                    }

                    self.trigger_invalidation_task_completion(spawned_task)
                        .await;

                    #[cfg(feature = "telemetry")]
                    {
                        let telemetry_ts = telemetry::zoned_timestamp_now();
                        self.submit_telemetry_event(telemetry::TelemetryEvent::new_commit(
                            task_id,
                            telemetry_ts,
                        ));
                    }

                    continue;
                }

                if !can_execute_locally {
                    return Err(format!("cannot execute {:#?} locally", spawned_task));
                }

                match transaction_manager.execute_spawned_task(spawned_task).await {
                    Ok((r, c)) => schedulable_task_results.push((r, c)),
                    Err(e) => return Err(e),
                }
            }
        }

        Ok(schedulable_task_results)
    }

    async fn trigger_invalidation_task_completion(
        &self,
        invalidation_task: epic_control::SpawnedTask,
    ) {
        let rt_handle = tokio::runtime::Handle::current();
        rt_handle.spawn(async move {
            let completed_task = invalidation_task.id;
            let mut tasks_to_notify = Vec::with_capacity(2);
            match invalidation_task.parent_task {
                epic_control::DownstreamTaskDependency::ParentDependency(dep_ref) => {
                    tasks_to_notify.push(
                        (epic_control::DownstreamTaskDependency::ParentDependency(dep_ref.clone()), epic_control::TaskStatus::Success, None)
                    );
                },
                _ => (),
            }

            for downstream_dependent in &invalidation_task.downstream_dependents {
                match downstream_dependent {
                    epic_control::DownstreamTaskDependency::None => (),
                    epic_control::DownstreamTaskDependency::ParentDependency(_) => {
                        panic!("encountered parent dependency in downstream dependency -- this is not allowed");
                    },
                    d @ epic_control::DownstreamTaskDependency::DataDependency(_, _)
                    | d @ epic_control::DownstreamTaskDependency::ControlDependency(_) => {
                            tasks_to_notify.push((d.clone(), epic_control::TaskStatus::Success, None));
                    },
                }
            }

            let activation_router = Self::get_activation_router(None, None, None, None);
            activation_router.forward_task_completion(
                TaskCompletionNotification::new(completed_task, tasks_to_notify)
            ).await;
        });
    }

    pub async fn whomstone_and_move_object(
        &self,
        object_id: ObjectId,
        new_host: HostId,
    ) -> Result<ObjectVersion, String> {
        match self
            .whomstone_object(object_id, new_host.clone(), true, None)
            .await
        {
            Ok((object_version, signature)) => {
                let signature = signature
                    .expect("missing signature of whomstoned object despite requesting it");
                self.trigger_whomstoned_object_move(object_id, &signature, new_host)
                    .await
                    .expect("failed to trigger move");

                Ok(object_version)
            }
            Err(e) => Err(e),
        }
    }

    pub async fn whomstone_and_move_cache_object(
        &self,
        cache_object_id: ObjectId,
        src_object_id: ObjectId,
        new_host: HostId,
        target_host_idx: HostIdx,
        whomstone_version: Option<ObjectVersion>,
    ) -> Result<ObjectVersion, String> {
        let (object_version, signature) = match self
            .whomstone_object(cache_object_id, new_host.clone(), true, whomstone_version)
            .await
        {
            Ok((object_version, signature)) => match signature {
                None => {
                    return Err(
                        "missing signature of whomstoned object despite requesting it".to_string(),
                    )
                }
                Some(s) => (object_version, s),
            },
            Err(e) => return Err(e),
        };

        let client = self.worker_client.clone();
        let src_object = IPtr::new(src_object_id, 0, 0);
        let cache_object = IPtr::new(cache_object_id, 0, 0);
        let own_host_idx = OwnershipTracker::get_host_idx_static(None).unwrap();
        match client
            .add_cache_mapping(
                &src_object,
                &cache_object,
                object_version,
                &new_host,
                own_host_idx,
            )
            .await
        {
            Ok(()) => {
                let ownership_tracker = OwnershipTracker::get_ownership_tracker(None);
                match ownership_tracker.add_shared_cache_entry(
                    src_object_id,
                    cache_object_id,
                    object_version,
                    target_host_idx,
                ) {
                    Ok(()) => {}
                    Err(_) => return Err("could not add shared cache entry".to_string()),
                }
            }
            Err(e) => return Err(e),
        }

        self.trigger_whomstoned_object_move(cache_object_id, &signature, new_host)
            .await
            .expect("failed to trigger move");

        Ok(object_version)
    }

    pub async fn whomstone_object(
        &self,
        object_id: ObjectId,
        new_host: HostId,
        get_signature: bool,
        whomstone_version: Option<ObjectVersion>,
    ) -> Result<(ObjectVersion, Option<Vec<u8>>), String> {
        #[cfg(feature = "timing-ownership-transfer")]
        {
            let start = SystemTime::now();
            println!(
                "move_ownership request for {object_id} received at {}",
                start
                    .duration_since(UNIX_EPOCH)
                    .expect("time moved backwards")
                    .as_millis()
            );
        }
        {
            let ownership_tracker = OwnershipTracker::get_ownership_tracker(None);
            ownership_tracker.mark_under_migration(object_id);
            ownership_tracker.insert_mapping_in_ownership_map(object_id, new_host.clone());
        }
        let whomstone_version = match whomstone_version {
            None => {
                let transaction_manager = TransactionManager::get_transaction_manager(None, None);
                let ownership_change_result = transaction_manager.whomstone_object(object_id).await;

                let Ok(whomstone_version) = ownership_change_result else {
                    panic!(
                        "failed to whomstone object {}: {:?}",
                        object_id, ownership_change_result
                    );
                };

                whomstone_version
            }
            Some(wv) => wv,
        };

        let client = self.worker_client.clone();
        let assume_ownership_request = ownership::AssumeOwnershipRequest {
            object_id,
            first_version: whomstone_version + 1,
            get_signature,
        };

        let maybe_signature = match client
            .assume_ownership(assume_ownership_request, &new_host)
            .await
        {
            Ok(ref serialized_foreign_signature) => match get_signature {
                false => None,
                true => Some(serialized_foreign_signature.clone()),
            },
            Err(e) => return Err(e),
        };

        #[cfg(feature = "timing-ownership-transfer")]
        {
            let done = SystemTime::now();
            println!(
                "move_ownership handoff for {object_id} completed at {}",
                done.duration_since(UNIX_EPOCH)
                    .expect("time moved backwards")
                    .as_millis()
            );
        }

        Ok((whomstone_version, maybe_signature))
    }

    pub async fn trigger_whomstoned_object_move(
        &self,
        object_id: ObjectId,
        serialized_foreign_signature: &Vec<u8>,
        new_host: HostId,
    ) -> Result<(), String> {
        // FIXME error handling in trigger_move
        self.location_manager
            .trigger_move(object_id, serialized_foreign_signature, new_host)
            .await;

        #[cfg(debug_assertions)]
        println!("Triggered move for {object_id}");

        Ok(())
    }

    pub async fn assume_ownership(
        &self,
        object_id: ObjectId,
        first_version: ObjectVersion,
    ) -> Result<Vec<u8>, String> {
        // println!("About to assume ownership of {object_id}");
        let ownership_tracker = OwnershipTracker::get_ownership_tracker(None);
        ownership_tracker.mark_incoming(object_id, first_version);

        #[cfg(any(feature = "observability", feature = "timing-ownership-transfer"))]
        let signature_calculation_start = tokio::time::Instant::now();
        let signature = self.location_manager.calculate_signature(object_id).await;
        #[cfg(feature = "observability")]
        {
            let signature_calculation_duration = signature_calculation_start.elapsed();
            OWNERSHIP_SIGNATURE_CALCULATION_HISTOGRAM
                .with_label_values(&[])
                .observe(signature_calculation_duration.as_micros() as f64 / 1000.0);
        }
        #[cfg(feature = "timing-ownership-transfer")]
        {
            let signature_calculation_duration = signature_calculation_start.elapsed();
            println!(
                "Took {}ms ({}s) to assume ownership/calculate signature of {object_id}",
                signature_calculation_duration.as_millis(),
                signature_calculation_duration.as_secs()
            );
        }
        self.location_manager.insert_move_handle(object_id).await;

        #[cfg(debug_assertions)]
        println!("Assumed ownership of {object_id} at {first_version}");
        Ok(signature)
    }

    pub async fn get_epic_status(
        &self,
        get_status_request: epic_definitions::GetEpicStatusRequest,
    ) -> Result<epic_definitions::EpicStatus, String> {
        #[cfg(not(feature = "offline"))]
        {
            let ownership_tracker = OwnershipTracker::get_ownership_tracker(None);
            let ecb_id = get_status_request.ecb_id;
            if !ownership_tracker.control_block_is_local(&ecb_id) {
                todo!("forward epic status request to ecb owner");
            }
        }

        let transaction_manager = TransactionManager::get_transaction_manager(None, None);
        match transaction_manager
            .get_epic_result_and_status(get_status_request.ecb_id)
            .await
        {
            Ok(res_status) => Ok(epic_definitions::EpicStatus {
                status: res_status.1,
                result: match res_status.0 {
                    Some(ref r) => Some(r.into()),
                    None => None,
                },
            }),
            Err(e) => Err(e),
        }
    }

    pub async fn get_epic_result(
        &self,
        get_status_request: epic_definitions::AwaitEpicResultRequest,
    ) -> Result<epic_definitions::EpicStatus, String> {
        #[cfg(not(feature = "offline"))]
        {
            let ownership_tracker = OwnershipTracker::get_ownership_tracker(None);
            let ecb_id = get_status_request.ecb_id;
            if !ownership_tracker.control_block_is_local(&ecb_id) {
                todo!("forward epic status request to ecb owner");
            }
        }

        let transaction_manager = TransactionManager::get_transaction_manager(None, None);
        match transaction_manager
            .get_epic_result_and_status(get_status_request.ecb_id)
            .await
        {
            Ok(res_status) => Ok(epic_definitions::EpicStatus {
                status: res_status.1,
                result: match res_status.0 {
                    Some(ref r) => Some(r.into()),
                    None => None,
                },
            }),
            Err(e) => Err(e),
        }
    }

    pub async fn spawn_caching_tasks(
        &self,
        target_host: HostId,
        cacheable_objects: &Vec<(ObjectId, ObjectVersion)>,
    ) {
        #[cfg(feature = "object-caching")]
        if *IGNORE_CACHEABLE {
            return;
        }

        if cacheable_objects.is_empty() {
            return;
        }

        let ownership_tracker = OwnershipTracker::get_ownership_tracker(None);
        let host_idx = OwnershipTracker::get_host_idx_static(None);
        // FIXME maybe this should submit directly through the mgr
        for (cacheable_object_id, cacheable_object_version) in cacheable_objects {
            let previous_cache_id = match ownership_tracker
                .mark_incoming_if_valid(*cacheable_object_id, *cacheable_object_version)
            {
                (true, true, _) => continue,
                (_, _, oid) => oid,
            };

            let cacheable_object_id = *cacheable_object_id;
            let cacheable_object_version = *cacheable_object_version;
            let mut caching_intent = activation_intent::NandoActivationIntent {
                host_idx: host_idx.clone(),
                name: "spawn_cache".to_string(),
                args: vec![
                    activation_intent::NandoArgument::Ref(IPtr::new(cacheable_object_id, 0, 0)),
                    (<u64 as Into<activation_intent::NandoArgument>>::into(
                        cacheable_object_version,
                    )),
                ],
            };

            if let Some(oid) = previous_cache_id {
                caching_intent.args.push(oid.into());
            }

            let target_host = target_host.clone();
            self.async_rt_manager.spawn(async move {
                #[cfg(feature = "observability")]
                let cache_start = tokio::time::Instant::now();
                let activation_router =
                    ActivationRouter::get_activation_router(None, None, None, None).clone();
                let (caching_response_output, _) = activation_router
                    .forward_schedule_request(&caching_intent, target_host.clone())
                    .await;

                let cache_iptr = match caching_response_output.first() {
                    Some(activation_output) => match activation_output.into() {
                        activation_intent::NandoResultSerializable::Ref(cache_iptr) => cache_iptr,
                        _ => panic!(),
                    },
                    _ => {
                        #[cfg(debug_assertions)]
                        eprintln!(
                            "response to caching request did not include a cache object, aborting"
                        );

                        return;
                    }
                };

                let ownership_tracker = OwnershipTracker::get_ownership_tracker(None);
                if !ownership_tracker.add_incoming_cache_mapping(
                    cacheable_object_id,
                    cache_iptr.object_id,
                    cacheable_object_version,
                ) {
                    return;
                }

                let ownership_transfer_request = ownership_support::MoveOwnershipRequest {
                    object_refs: vec![cache_iptr.object_id],
                    new_host: activation_router.host_id.clone(),
                };

                let client = activation_router.worker_client.clone();
                match client
                    .move_ownership(ownership_transfer_request, &target_host)
                    .await
                {
                    Err(e) => {
                        eprintln!(
                            "failed while requesting ownership move of object cache {} for {}: {}",
                            cache_iptr.object_id, cacheable_object_id, e
                        );
                        return;
                    }
                    Ok(_rb) => {
                        // TODO check that whomstone_version matches the cacheable object's version
                    }
                }

                if !ownership_tracker.object_is_incoming(cache_iptr.object_id) {
                    #[cfg(debug_assertions)]
                    eprintln!(
                        "object {} supposed to be under migration but not incoming",
                        cache_iptr.object_id
                    );
                    #[cfg(feature = "observability")]
                    {
                        let cache_duration = cache_start.elapsed();
                        HTTP_AROUTER_CACHING_HISTOGRAM
                            .with_label_values(&["abort"])
                            .observe(cache_duration.as_micros() as f64);
                    }
                    return;
                }

                activation_router
                    .location_manager
                    .wait_for_object_or_update(cache_iptr.object_id)
                    .await;
                /*
                activation_router
                    .location_manager
                    .get_object_tracker()
                    .create_materialized_ro_version(cache_iptr.object_id);
                    */

                // make object available for local reading.
                ownership_tracker.add_owned_cache_mapping(
                    cacheable_object_id,
                    cache_iptr.object_id,
                    cacheable_object_version,
                );
                #[cfg(feature = "observability")]
                {
                    let cache_duration = cache_start.elapsed();
                    HTTP_AROUTER_CACHING_HISTOGRAM
                        .with_label_values(&["success"])
                        .observe(cache_duration.as_micros() as f64);
                }
            });
        }
    }

    async fn wait_for_object_or_cache(&self, object_id: ObjectId) {
        let ownership_tracker = OwnershipTracker::get_ownership_tracker(None);
        if ownership_tracker.object_is_owned(object_id) {
            return;
        }

        if ownership_tracker.object_is_incoming(object_id) {
            self.location_manager
                .wait_for_object_or_update(object_id)
                .await;
            return;
        }

        #[cfg(feature = "object-caching")]
        {
            if ownership_tracker
                .get_cache_mapping_if_valid(object_id)
                .is_some()
            {
                return;
            }

            if let Some(incoming_cache_mapping) =
                ownership_tracker.get_cache_mapping_if_incoming(object_id)
            {
                loop {
                    if self
                        .location_manager
                        .wait_for_object_or_update(incoming_cache_mapping)
                        .await
                        .is_some()
                    {
                        break;
                    }

                    tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;
                }
                return;
            };
        }
    }

    pub async fn forward_spawned_task(&self, spawned_task: epic_control::SpawnedTask) {
        let mut serializable_task: activation_intent::SpawnedTaskSerializable =
            (&spawned_task).into();
        serializable_task.intent.host_idx = OwnershipTracker::get_host_idx_static(None);

        let target_host = {
            let ownership_tracker = OwnershipTracker::get_ownership_tracker(None);
            let intent_metadata =
                match NandoScheduler::get_nando_metadata_external(&spawned_task.intent.name) {
                    Err(e) => {
                        eprintln!(
                            "Could not decide if the request for '{}' can be satisfied locally: {}",
                            spawned_task.intent.name, e
                        );
                        return;
                    }
                    Ok(meta) => meta,
                };
            let mutable_object_references = match intent_metadata.mutable_argument_indices {
                None => spawned_task.intent.get_object_references(),
                Some(i) => spawned_task
                    .intent
                    .args
                    .iter()
                    .enumerate()
                    .filter(|(idx, _)| i.contains(idx))
                    .map(|(_, r)| r.get_object_id().unwrap())
                    .collect(),
            };

            #[cfg(debug_assertions)]
            println!(
                "will attempt to compute activation site based on {:?}",
                mutable_object_references
            );
            match ownership_tracker.compute_activation_site(&mutable_object_references) {
                None => match ownership_tracker.objects_are_owned(&mutable_object_references) {
                    false => {
                        println!(
                            "could not compute activation site based on {:?}, will ask scheduler",
                            mutable_object_references
                        );
                        self.ask_scheduler(&spawned_task.intent).await
                    }
                    true => ownership_tracker.get_own_host_id().unwrap(),
                },
                Some(host) => host,
            }
        };

        self.forward_spawned_task_to(spawned_task, target_host)
            .await;
    }

    async fn forward_spawned_task_to(
        &self,
        spawned_task: epic_control::SpawnedTask,
        target_host: HostId,
    ) {
        #[cfg(debug_assertions)]
        println!(
            "attempting to forward spawned task {} to {}: {:#?}",
            spawned_task.id, target_host, spawned_task
        );
        let mut spawned_task = spawned_task;

        if target_host == self.host_id {
            println!(
                "Will wait for dependencies {:?} of spawned task {:#?}",
                spawned_task.intent.get_object_references(),
                spawned_task,
            );
            for object in spawned_task.intent.get_object_references() {
                self.wait_for_object_or_cache(object).await;
            }
            // If we got ownership of the dependencies for the spawned task, we can proceed to execute
            // locally by reintroducing it into the local execution subsystem.
            let transaction_manager = TransactionManager::get_transaction_manager(None, None);
            let intent_name = spawned_task.intent.name.clone();
            let task_id = spawned_task.id;
            match transaction_manager.execute_spawned_task(spawned_task).await {
                Ok(_) => {}
                Err(e) => eprintln!(
                    "could not execute spawned task '{}' ({}): {}",
                    intent_name, task_id, e
                ),
            }

            return;
        }

        // FIXME isn't this wrong?
        spawned_task.intent.host_idx = OwnershipTracker::get_host_idx_static(None);

        let client = self.worker_client.clone();
        #[cfg(feature = "observability")]
        let req_start = tokio::time::Instant::now();

        let response = client
            .forward_spawned_task(spawned_task.clone(), &target_host)
            .await
            .expect("failed");

        #[cfg(feature = "observability")]
        {
            let req_duration = req_start.elapsed();
            HTTP_AROUTER_EREQ_HISTOGRAM
                .with_label_values(&[
                    "/activation_router/schedule_spawned_task",
                    &spawned_task.intent.name,
                ])
                .observe(req_duration.as_micros() as f64);
        }

        match response.cacheable_objects {
            None => {}
            Some(ref c) => self.spawn_caching_tasks(target_host, c).await,
        }
    }

    async fn forward_spawned_tasks_to(
        &self,
        spawned_tasks: Vec<epic_control::SpawnedTask>,
        spawned_task_locations: HashMap<HostId, Vec<EcbId>>,
        target_host: Option<HostId>,
    ) {
        let ownership_tracker = OwnershipTracker::get_ownership_tracker(None);
        let host_idx = match ownership_tracker.get_host_idx() {
            None => 0,
            Some(hi) => hi,
        };

        let (local_whomstone_tasks, mut remote_tasks): (Vec<epic_control::SpawnedTask>, _) =
            spawned_tasks.into_iter().partition(|st| {
                if !(st.intent.is_whomstone_intent() || st.intent.is_whomstone_and_move_intent()) {
                    return false;
                }

                if st.intent.has_unresolved_args() {
                    return false;
                }

                let target_object = st.intent.args[0]
                    .get_object_id()
                    .expect("no target object in whomstone task");
                ownership_tracker.object_is_owned(target_object)
            });

        if !remote_tasks.is_empty() {
            let mut task_grouping: HashMap<HostId, Vec<epic_control::SpawnedTask>> =
                HashMap::default();
            match target_host {
                None => {
                    let mut ownership_cache_updated = false;
                    // NOTE currently if we find ourselves here it's most likely because we need to
                    // schedule a local whomstone intent with a remote downstream dependency, and we
                    // want to find the target host by inspecting the host that owns the remote task
                    for task_to_schedule in remote_tasks.drain(..) {
                        if task_to_schedule.intent.is_whomstone_intent()
                            || task_to_schedule.intent.is_whomstone_and_move_intent()
                        {
                            let cbe = loop {
                                let mut cbe = None;
                                for downstream_dep in
                                    task_to_schedule.downstream_dependents.iter().rev()
                                {
                                    let dependency_ecb_id = downstream_dep
                                        .get_inner_ecb_id()
                                        .expect("could not get whomstone dep ecb id");
                                    match ownership_tracker
                                        .get_control_block_entry(dependency_ecb_id)
                                    {
                                        None => continue,
                                        Some(e) => {
                                            cbe = Some(e);
                                            break;
                                        }
                                    }
                                }

                                break cbe;
                            };

                            match cbe {
                                Some(cbe) => match task_grouping.get_mut(&cbe) {
                                    Some(e) => e.push(task_to_schedule),
                                    None => {
                                        task_grouping.insert(cbe, vec![task_to_schedule]);
                                    }
                                },
                                None => {
                                    self.update_ownership_cache(&ownership_tracker).await;
                                    let cached_object_id = {
                                        let target_arg = match task_to_schedule
                                            .intent
                                            .is_invalidation_intent()
                                        {
                                            true => task_to_schedule.intent.args.get(1),
                                            false => task_to_schedule.intent.args.get(0),
                                        };

                                        target_arg.unwrap().get_object_id().unwrap()
                                    };

                                    match ownership_tracker
                                        .get_owner_of_shared_cache(cached_object_id)
                                    {
                                        Some(o) => match task_grouping.get_mut(&o) {
                                            Some(e) => e.push(task_to_schedule),
                                            None => {
                                                task_grouping.insert(o, vec![task_to_schedule]);
                                            }
                                        },
                                        None => {
                                            if !ownership_cache_updated {
                                                self.update_ownership_cache(&ownership_tracker)
                                                    .await;
                                                ownership_cache_updated = true;
                                            }

                                            let object_dependencies =
                                                task_to_schedule.intent.get_object_references();
                                            match ownership_tracker
                                                .compute_activation_site(&object_dependencies)
                                            {
                                                Some(h) => match task_grouping.get_mut(&h) {
                                                    Some(e) => e.push(task_to_schedule),
                                                    None => {
                                                        task_grouping
                                                            .insert(h, vec![task_to_schedule]);
                                                    }
                                                },
                                                None => unreachable!(),
                                            }
                                        }
                                    }
                                }
                            }
                        } else {
                            if !ownership_cache_updated {
                                self.update_ownership_cache(&ownership_tracker).await;
                                ownership_cache_updated = true;
                            }

                            let object_dependencies =
                                task_to_schedule.intent.get_object_references();
                            match ownership_tracker.compute_activation_site(&object_dependencies) {
                                Some(h) => match task_grouping.get_mut(&h) {
                                    Some(e) => e.push(task_to_schedule),
                                    None => {
                                        task_grouping.insert(h, vec![task_to_schedule]);
                                    }
                                },
                                None => unreachable!(),
                            }
                        }
                    }
                }
                Some(ref h) => {
                    task_grouping.insert(h.to_string(), remote_tasks);
                }
            };

            for (target_host, host_remote_tasks) in &mut task_grouping {
                let target_host_idx = ownership_tracker.get_host_idx_for_id(target_host).unwrap();
                for remote_task in host_remote_tasks.iter_mut() {
                    remote_task.set_intent_host_idx(host_idx);
                    ownership_tracker.insert_control_block_entry(remote_task.id, target_host_idx);
                }

                #[cfg(debug_assertions)]
                println!(
                    "Will try to forward {} tasks to {}: {:#?}",
                    host_remote_tasks.len(),
                    target_host,
                    host_remote_tasks
                );

                let client = self.worker_client.clone();
                client
                    .schedule_task_graph(&host_remote_tasks, &spawned_task_locations, &target_host)
                    .await
                    .expect("call to schedule tg failed");
            }
        }

        let mut to_await = Vec::with_capacity(local_whomstone_tasks.len());

        for local_whomstone_task in local_whomstone_tasks.into_iter() {
            #[cfg(feature = "telemetry")]
            let local_whomstone_task_id = local_whomstone_task.id;

            #[cfg(feature = "telemetry")]
            {
                let telemetry_ts = telemetry::zoned_timestamp_now();
                self.submit_telemetry_event(telemetry::TelemetryEvent::new_schedule(
                    local_whomstone_task_id,
                    0,
                    telemetry_ts,
                ));
            }

            let figaro_client = self.scheduler_client.clone();
            let target_host = match target_host {
                Some(ref target_host) => target_host.clone(),
                None => {
                    let cbe = loop {
                        let mut cbe = None;
                        for downstream_dep in
                            local_whomstone_task.downstream_dependents.iter().rev()
                        {
                            let dependency_ecb_id = downstream_dep
                                .get_inner_ecb_id()
                                .expect("could not get whomstone dep ecb id");
                            match ownership_tracker.get_control_block_entry(dependency_ecb_id) {
                                None => continue,
                                Some(e) => {
                                    cbe = Some(e);
                                    break;
                                }
                            }
                        }

                        break cbe;
                    };

                    match cbe {
                        Some(cbe) => cbe,
                        None => {
                            let cached_object_id = {
                                let target_arg = local_whomstone_task.intent.args.get(0);
                                target_arg.unwrap().get_object_id().unwrap()
                            };

                            match ownership_tracker.get_owner_of_shared_cache(cached_object_id) {
                                Some(o) => o,
                                None => local_whomstone_task
                                    .intent
                                    .args
                                    .last()
                                    .unwrap()
                                    .get_string()
                                    .expect("no string arg at end of whomstone"),
                            }
                        }
                    }
                }
            };
            let target_host_idx = ownership_tracker.get_host_idx_for_id(&target_host).unwrap();

            #[cfg(debug_assertions)]
            println!(
                "About to execute a whomstone tasks with {} as target: {:#?}",
                target_host, local_whomstone_task
            );

            let activation_router =
                ActivationRouter::get_activation_router(None, None, None, None).clone();

            let handle = self.async_rt_manager.spawn(async move {
                let dependencies = match local_whomstone_task.should_notify_parent() {
                    true => vec![local_whomstone_task.get_parent_as_downstream_dependency()],
                    false => {
                        ownership_tracker
                            .insert_control_block_entry(local_whomstone_task.id, target_host_idx);
                        local_whomstone_task.downstream_dependents.clone()
                    }
                };

                let target_object = local_whomstone_task.intent.args[0]
                    .get_object_id()
                    .expect("missing object id in ownership change intent");

                let whomstone_version =
                    match local_whomstone_task.intent.is_whomstone_and_move_intent() {
                        true => match local_whomstone_task.intent.is_cache_related_intent() {
                            false => activation_router
                                .whomstone_and_move_object(target_object, target_host.clone())
                                .await
                                .expect("failed to change ownership and move object"),
                            true => {
                                let src_object = local_whomstone_task.intent.args[1]
                                    .get_object_id()
                                    .expect("missing original object in cache whomstone intent");
                                let whomstone_version =
                                    local_whomstone_task.intent.args.get(2).unwrap();
                                activation_router
                                    .whomstone_and_move_cache_object(
                                        target_object,
                                        src_object,
                                        target_host.clone(),
                                        target_host_idx,
                                        whomstone_version.get_u64_value(),
                                    )
                                    .await
                                    .expect("failed to change ownership and move object")
                            }
                        },
                        false => {
                            activation_router
                                .whomstone_object(target_object, target_host.clone(), false, None)
                                .await
                                .expect("failed to change ownership")
                                .0
                        }
                    };

                // NOTE if whomstone_and_move, then we need to notify the ownership orchestrator that
                // the transfer might be ongoing

                let mut notified = false;
                for _ in 0..5 {
                    match figaro_client
                        .put(activation_router.get_scheduler_url("peer_location_change"))
                        .json(&ownership::ConsolidationIntent {
                            to_host: target_host_idx as u64,
                            args: vec![target_object],
                            versions: vec![whomstone_version],
                        })
                        .send()
                        .await
                    {
                        Ok(response) => {
                            if response.status().is_success() {
                                notified = true;
                                break;
                            }

                            eprintln!(
                                "Failed to notify of peer location change, will try again: {:?}",
                                response
                            );
                            tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;
                        }
                        Err(e) => {
                            eprintln!(
                                "Failed to notify of peer location change, will try again: {e}"
                            );
                            tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;
                        }
                    }
                }

                if !notified {
                    // FIXME
                    panic!("Failed to notify of peer location change");
                }

                let dependencies = dependencies
                    .into_iter()
                    .map(|d| {
                        (
                            d,
                            epic_control::TaskStatus::Success,
                            Some(activation_intent::NandoResult::from_object_id(
                                target_object,
                            )),
                        )
                    })
                    .collect();
                if local_whomstone_task.should_notify_parent() {
                    // NOTE since this task was generated locally (because of the code path we took
                    // to get here), the parent of this task is local to this node, so we can just
                    // call the local completion notification handler.
                    activation_router
                        .handle_task_completion(local_whomstone_task.id, dependencies, None)
                        .await;
                } else {
                    activation_router
                        .forward_task_completion(TaskCompletionNotification::new(
                            local_whomstone_task.id,
                            dependencies,
                        ))
                        .await;
                }

                #[cfg(feature = "telemetry")]
                {
                    let telemetry_ts = telemetry::zoned_timestamp_now();
                    activation_router.submit_telemetry_event(
                        telemetry::TelemetryEvent::new_commit(
                            local_whomstone_task_id,
                            telemetry_ts,
                        ),
                    );
                }
            });
            to_await.push(handle);
        }

        futures::future::join_all(to_await).await;
    }

    pub async fn forward_task_completion(
        &self,
        task_completion_notification: TaskCompletionNotification,
    ) {
        let completed_task = task_completion_notification.completed_task_id;
        let ownership_tracker = OwnershipTracker::get_ownership_tracker(None);
        let own_host_idx = ownership_tracker.get_host_idx().expect("missing own idx");
        match ownership_tracker.get_control_block_entry(completed_task) {
            None => {
                let mut tasks_by_owner: HashMap<
                    Option<HostId>,
                    Vec<(
                        epic_control::DownstreamTaskDependency,
                        epic_control::TaskStatus,
                        Option<activation_intent::NandoResult>,
                    )>,
                > = HashMap::new();
                for task in task_completion_notification.tasks_to_notify.into_iter() {
                    let dependency_id = task.0.get_inner_ecb_id().unwrap();
                    let target = ownership_tracker.get_control_block_entry(dependency_id);

                    match tasks_by_owner.get_mut(&target) {
                        None => {
                            tasks_by_owner.insert(target, vec![task]);
                        }
                        Some(ts) => ts.push(task),
                    }
                }

                let client = self.worker_client.clone();
                for (task_owner, tasks_to_notify) in tasks_by_owner.into_iter() {
                    let Some(host_id) = task_owner else {
                        println!(
                            "No task owner for tasks under {}, will handle locally: {:#?}",
                            completed_task, tasks_to_notify,
                        );
                        self.handle_task_completion(
                            task_completion_notification.completed_task_id,
                            tasks_to_notify,
                            None,
                        )
                        .await;
                        continue;
                    };

                    #[cfg(debug_assertions)]
                    println!(
                        "will forward task completion of {} to dependency host {host_id}: {:#?}",
                        completed_task, tasks_to_notify,
                    );
                    let mut notification = TaskCompletionNotification::new(
                        task_completion_notification.completed_task_id,
                        tasks_to_notify,
                    );
                    notification.subgraph_allocations =
                        task_completion_notification.subgraph_allocations.clone();

                    client
                        .forward_task_completion(notification, own_host_idx, &host_id)
                        .await
                        .expect("taks completion fwd failed");
                }
            }
            Some(host_id) => {
                #[cfg(debug_assertions)]
                println!(
                    "will forward task completion of {} to known host {host_id}: {:#?}",
                    completed_task, task_completion_notification.tasks_to_notify,
                );

                #[cfg(feature = "observability")]
                let req_start = tokio::time::Instant::now();
                let client = self.worker_client.clone();

                client
                    .forward_task_completion(task_completion_notification, own_host_idx, &host_id)
                    .await
                    .expect("taks completion fwd failed");
                #[cfg(feature = "observability")]
                {
                    let req_duration = req_start.elapsed();
                    HTTP_AROUTER_EREQ_HISTOGRAM
                        .with_label_values(&["/activation_router/task_completion", "N/A"])
                        .observe(req_duration.as_micros() as f64);
                }
            }
        }
    }

    pub fn store_remote_allocations(&self, allocations: Vec<(HostIdx, ObjectId)>) {
        let ownership_tracker = OwnershipTracker::get_ownership_tracker(None);
        for (owning_host_idx, object_id) in &allocations {
            let owning_host = ownership_tracker
                .get_host_id_for_idx(*owning_host_idx)
                .expect("missing host mapping for idx");
            ownership_tracker.insert_mapping_in_ownership_map(*object_id, owning_host);
        }
    }

    pub async fn handle_task_completion(
        &self,
        completed_task: EcbId,
        tasks_to_notify: Vec<(
            epic_control::DownstreamTaskDependency,
            epic_control::TaskStatus,
            Option<activation_intent::NandoResult>,
        )>,
        notifying_host_idx: Option<HostIdx>,
    ) {
        #[cfg(debug_assertions)]
        println!(
            "will handle task completion of {}: {:?}",
            completed_task, tasks_to_notify
        );

        let ownership_tracker = OwnershipTracker::get_ownership_tracker(None);

        if let Some(notifying_host_idx) = notifying_host_idx {
            let notifying_host = ownership_tracker
                .get_host_id_for_idx(notifying_host_idx)
                .unwrap();
            for task_notification in &tasks_to_notify {
                if !task_notification.0.is_data_dependency() {
                    continue;
                }

                let Some(ref data_ref) = task_notification.2 else {
                    continue;
                };

                if let Some(object_id) = data_ref.get_object_id() {
                    ownership_tracker.insert_mapping_in_ownership_map_if_not_present(
                        object_id,
                        notifying_host.clone(),
                    );
                }
            }
        }

        // let _ = ownership_tracker.remove_control_block_entry(completed_task);

        let transaction_manager = TransactionManager::get_transaction_manager(None, None);
        let mut schedulable_after_transit = transaction_manager
            .handle_task_completion(completed_task, tasks_to_notify)
            .await;

        // NOTE ideally the below logic would be entirely inside TransactionManager, but
        // unfortunately the call to `LocationManager::wait_for_object()` breaks axum's derive
        // logic for the task completion endpoint, so we have to do it out here where it's somehow
        // fine.
        for task_with_deps_in_flight in schedulable_after_transit.drain(..) {
            let in_flight_args = {
                let control_info = task_with_deps_in_flight.get_control_info_read();
                ownership_tracker.get_in_flight_args(&control_info.intent.args)
            };

            for in_flight_arg in in_flight_args {
                // FIXME I think here we should actually be using `wait_for_object()` but since we
                // don't evict objects from ObjectTracker after successfully moving an object
                // somewhere else, using that method would immediately return and so we would
                // potentially end up reading a stale version of an object dependency if we were
                // the owner of a previous version of the object.
                // We used to drop objects from the tracker but we changed that in #74 in an
                // attempt to reuse stale caches. We will eventually need to refactor data movement
                // anyway so it might be worth reconsidering this decision then.
                self.location_manager
                    .wait_for_object_or_update(in_flight_arg)
                    .await;
            }

            transaction_manager.schedule_parkable_entry(task_with_deps_in_flight);
        }
    }

    pub async fn fetch_host_mapping(&self) {
        let ownership_tracker = OwnershipTracker::get_ownership_tracker(None);
        ownership_tracker.fetch_host_mapping().await;
    }

    pub async fn add_valid_cache_mapping(
        &self,
        original_object_id: ObjectId,
        cached_object_id: ObjectId,
        cache_version: ObjectVersion,
        original_object_owner: HostIdx,
    ) {
        let ownership_tracker = OwnershipTracker::get_ownership_tracker(None);
        let original_owning_host = ownership_tracker
            .get_host_id_for_idx(original_object_owner)
            .expect("failed to get host with idx {original_object_owner}");
        ownership_tracker.insert_mapping_in_ownership_map(original_object_id, original_owning_host);
        // First, add a placeholder mapping.
        let _ = ownership_tracker.mark_incoming_if_valid(original_object_id, cache_version);
        // Then, update mapping to owned.
        ownership_tracker.add_owned_cache_mapping(
            original_object_id,
            cached_object_id,
            cache_version,
        );
    }
}
