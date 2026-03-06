use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use async_lib::AsyncRuntimeManager;
use async_std::sync::Arc as AsyncArc;
use clap::Parser;
use cli::Args;
use config::Config;
use ctrlc;
use location_manager::LocationManager;
use logging;
use nando_lib::nando_executor::NandoExecutor;
use nando_lib::nando_scheduler::NandoScheduler;
use nando_lib::transaction_manager::TransactionManager;
use net::{spawn_axum_servers, spawn_servers};
use object_tracker::ObjectTracker;
use ownership_tracker::OwnershipTracker;
use scheduling::{activation_router::ActivationRouter, net::http_service::NetServiceInterface};
#[cfg(feature = "telemetry")]
use telemetry;
#[cfg(target_os = "linux")]
use tikv_jemallocator::Jemalloc;

#[cfg(target_os = "linux")]
#[global_allocator]
static GLOBAL: Jemalloc = Jemalloc;

mod cli;
mod config;
mod net;

#[cfg(test)]
mod tests;

fn parse_args() -> Args {
    let mut args = Args::parse();

    if args.config_file_path.is_none() {
        args.config_file_path = Some("config.toml".into());
    }

    return args;
}

fn print_startup_message() {
    let mut enabled_flags = Vec::new();

    #[cfg(feature = "epic_opts")]
    enabled_flags.push("- epic_opts");
    #[cfg(not(feature = "epic_opts"))]
    enabled_flags.push("- no epic_opts");

    #[cfg(feature = "object-caching")]
    enabled_flags.push("- object-caching");
    #[cfg(not(feature = "object-caching"))]
    enabled_flags.push("- no object-caching");

    #[cfg(feature = "observability")]
    enabled_flags.push("- observability");
    #[cfg(not(feature = "observability"))]
    enabled_flags.push("- no observability");

    #[cfg(feature = "offline")]
    enabled_flags.push("- offline");
    #[cfg(not(feature = "offline"))]
    enabled_flags.push("- no offline");

    #[cfg(feature = "simd")]
    enabled_flags.push("- simd");
    #[cfg(not(feature = "simd"))]
    enabled_flags.push("- no simd");

    #[cfg(feature = "timing-svc-latency")]
    enabled_flags.push("- timing svc latency");
    #[cfg(not(feature = "timing-svc-latency"))]
    enabled_flags.push("- no timing svc latency");

    #[cfg(feature = "timing-exec")]
    enabled_flags.push("- timing exec");
    #[cfg(not(feature = "timing-exec"))]
    enabled_flags.push("- no timing exec");

    #[cfg(feature = "timing-sched-submission")]
    enabled_flags.push("- timing sched-submission");
    #[cfg(not(feature = "timing-sched-submission"))]
    enabled_flags.push("- no timing sched-submission");

    #[cfg(feature = "timing-sched-completion")]
    enabled_flags.push("- timing sched-completion");
    #[cfg(not(feature = "timing-sched-completion"))]
    enabled_flags.push("- no timing sched-completion");

    #[cfg(feature = "timing-log")]
    enabled_flags.push("- timing log");
    #[cfg(not(feature = "timing-log"))]
    enabled_flags.push("- no timing log");

    #[cfg(feature = "timing-tm")]
    enabled_flags.push("- timing tm");
    #[cfg(not(feature = "timing-tm"))]
    enabled_flags.push("- no timing tm");

    #[cfg(feature = "timing-tm-wait")]
    enabled_flags.push("- timing tm-wait");
    #[cfg(not(feature = "timing-tm-wait"))]
    enabled_flags.push("- no timing tm-wait");

    #[cfg(feature = "no-persist")]
    enabled_flags.push("- no-persist");
    #[cfg(not(feature = "no-persist"))]
    enabled_flags.push("- no no-persist (persistence enabled)");

    let enabled_flags = enabled_flags.join("\n");
    // TODO add config summary
    println!("[DEBUG] Compile-time flags:\n{enabled_flags}\nInitting subcomponents");
}

fn main() {
    let args = parse_args();
    let config = Config::init_from_file(args.config_file_path.unwrap());

    print_startup_message();

    let ownership_tracker = AsyncArc::new(OwnershipTracker::get_ownership_tracker(Some(
        &config.ownership_tracker_config,
    )));

    let async_rt_manager = Arc::new(AsyncRuntimeManager::new(
        &config.async_lib_config,
        config.nando_lib_config.executor_config.num_worker_threads as usize,
    ));

    let host_idx: u16 = {
        let hostname = config.hostname.clone();
        #[cfg(not(feature = "offline"))]
        let preset_host_idx = None;
        #[cfg(feature = "offline")]
        let preset_host_idx = Some(config.client_config.client_id.into());

        async_rt_manager
            .block_on(async {
                ownership_tracker
                    .register_worker(hostname, preset_host_idx)
                    .await
            })
            .expect("failed to register worker")
            .try_into()
            .expect("could not convert host idx to u16")
    };

    #[cfg(feature = "telemetry")]
    {
        let host_id = config.hostname.clone();
        telemetry::init_telemetry_manager(host_id, host_idx.clone() as u64);
        OwnershipTracker::initialize_telemetry_handle();
    }

    let object_tracker = match ObjectTracker::new(host_idx) {
        Some(v) => Arc::new(v),
        _ => panic!(),
    };

    object_lib::files::set_up_allocation_dir().expect("failed to set up allocation dir");
    match args.reset_object_dir {
        true => {
            println!("[DEBUG] Resetting object directory");
            object_tracker.reset_object_directory()
        }
        false => {
            println!("[DEBUG] Loading existing objects from object directory");
            object_tracker.read_object_directory()
        }
    }

    let location_manager = AsyncArc::new(LocationManager::new(
        config.location_manager_config,
        Arc::clone(&async_rt_manager),
        Arc::clone(&object_tracker),
    ));

    {
        let location_manager = AsyncArc::clone(&location_manager);
        async_rt_manager.spawn(async move {
            location_manager
                .init_server()
                .await
                .expect("failed to init location manager servers")
        });
    }

    {
        let async_rt_manager = Arc::clone(&async_rt_manager);
        let location_manager = AsyncArc::clone(&location_manager);
        TransactionManager::get_transaction_manager(Some(async_rt_manager), Some(location_manager));
    }

    let activation_router = {
        let host_id = config.hostname.clone();
        let location_manager = AsyncArc::clone(&location_manager);
        let async_rt_manager = Arc::clone(&async_rt_manager);
        let activation_router = ActivationRouter::get_activation_router(
            Some(config.scheduling_config),
            Some(host_id.to_string()),
            Some(location_manager),
            Some(async_rt_manager),
        );

        activation_router.healthcheck_all_threads();
        activation_router
    };
    activation_router.register_callback_fns();

    {
        // Logging subsystem initialization.
        logging::LogManager::get_txn_log_manager(Some(config.logging_config.clone()));
    }

    let _nando_scheduler = {
        // Execution subsystem initialization.
        let object_tracker = Arc::clone(&object_tracker);
        let nando_lib_config = config.nando_lib_config.clone();
        NandoExecutor::get_nando_executor(
            Some(nando_lib_config.executor_config.clone()),
            Some(Arc::clone(&object_tracker)),
        );
        NandoScheduler::get_nando_scheduler_mut(Some(object_tracker), Some(nando_lib_config))
    };

    let client_config = config.client_config.clone();
    {
        let client_config = config.client_config;
        let async_rt_manager = Arc::clone(&async_rt_manager);
        spawn_servers(&client_config, async_rt_manager);
    }

    let router = {
        let router = axum::Router::new();
        ActivationRouter::populate_routes("/activation_router", router)
    };
    spawn_axum_servers(&client_config, router, Arc::clone(&async_rt_manager));

    let should_break = AsyncArc::new(AtomicBool::new(false));
    let should_break_handler = AsyncArc::clone(&should_break);

    ctrlc::set_handler(move || {
        should_break_handler.store(true, Ordering::Relaxed);
    })
    .expect("Failed to set SIGINT handler");

    println!(
        "[DEBUG] Startup complete, host '{}' ready to accept requests",
        config.hostname
    );
    while !should_break.load(Ordering::Relaxed) {
        thread::sleep(Duration::from_millis(500));
    }

    // Cleanup
    logging::LogManager::get_txn_log_manager(None).shutdown();
    object_tracker.shutdown();

    #[cfg(feature = "telemetry")]
    {
        let nando_scheduler = _nando_scheduler;
        nando_scheduler.log_stats();
        // since the above is async and we don't really want to await the completion of the event,
        // we can just sleep for a bit
        thread::sleep(Duration::from_millis(500));
        // activation_router.log_stats();
    }
}
