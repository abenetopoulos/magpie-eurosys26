use actix_web::{App, HttpServer};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;

use clap::Parser;
use cli::Args;
use config::Config;
use nando_support::ObjectId;
use orchestration::HostIdx;

mod cli;
mod config;
mod handlers;
mod orchestration;

fn parse_args() -> Args {
    let mut args = Args::parse();

    if args.config_file_path.is_none() {
        args.config_file_path = Some("config.toml".into());
    }

    return args;
}

fn init_host_mapping(worker_hosts: &Vec<String>) {
    let host_manager = orchestration::host_manager::HostManager::get_host_manager().clone();

    for (idx, hostname) in worker_hosts.iter().enumerate() {
        host_manager
            .insert_host_with_idx(idx.try_into().unwrap(), hostname.to_string())
            .expect(&format!("failed to register host '{}'", hostname));
    }
}

async fn seed_local_objects(seed_data: Option<PathBuf>) {
    if seed_data.is_none() {}

    let reader = match seed_data {
        Some(pb) => {
            let seed_data_file = File::open(pb).unwrap();
            BufReader::new(seed_data_file)
        }
        None => {
            println!("No seed file provided, skipping");
            return;
        }
    };
    let ownership_registry =
        orchestration::registry::OwnershipRegistry::get_ownership_registry().clone();
    for line_iter in reader.lines() {
        let line = line_iter.unwrap();
        let line_elems = line.split(";").collect::<Vec<&str>>();
        let host_idx: HostIdx = line_elems[0].trim().parse::<HostIdx>().unwrap();
        let object_id: ObjectId = line_elems[1].trim().parse::<ObjectId>().unwrap();

        let hostname = format!("host-{}", host_idx);
        orchestration::register_worker(Some(host_idx), hostname)
            .expect("failed to register host, should be idempotent");
        if !ownership_registry.handle_publish(object_id, host_idx) {
            eprintln!("seeding of {} (by host {}) failed", object_id, host_idx);
            panic!("aborting");
        }
    }

    println!("Done seeding");
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    let args = parse_args();
    let config = Config::init_from_file(args.config_file_path.unwrap());
    init_host_mapping(&config.worker_hosts);
    seed_local_objects(args.seed_data).await;

    #[cfg(feature = "always_move")]
    println!("[DEBUG] Running in 'always_move' mode");
    #[cfg(not(feature = "always_move"))]
    println!("[DEBUG] Running in 'move_least' mode");

    HttpServer::new(|| {
        App::new()
            .service(handlers::healthcheck_handler)
            .service(handlers::schedule_handler)
            .service(handlers::consolidate_handler)
            .service(handlers::publish_handler)
            .service(handlers::multi_publish_handler)
            .service(handlers::register_worker_handler)
            .service(handlers::get_worker_mapping_handler)
            .service(handlers::reset_state_handler)
            .service(handlers::cache_object)
            .service(handlers::get_location_handler)
            .service(handlers::peer_location_change_handler)
            .service(handlers::get_ownership_snapshot_handler)
    })
    .bind(("0.0.0.0", config.server_port))?
    .workers(config.worker_threads as usize)
    .keep_alive(std::time::Duration::from_secs(90))
    // TODO check if this leads clients to hang
    .client_request_timeout(std::time::Duration::from_secs(0))
    .client_disconnect_timeout(std::time::Duration::from_secs(0))
    .run()
    .await
}
