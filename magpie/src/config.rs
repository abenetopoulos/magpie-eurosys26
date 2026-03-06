//! Configuration for the runtime's various subcomponents.
use std::env;
use std::fs::read_to_string;
use std::path::PathBuf;

use async_lib::config::Config as AsyncLibConfig;
use location_manager::config::Config as LocationManagerConfig;
use logging::config::Config as LoggingConfig;
use nando_lib::config::Config as NandoLibConfig;
use ownership_tracker::config::Config as OwnershipTrackerConfig;
use scheduling::config::Config as SchedulingConfig;
use serde::{Deserialize, Serialize};
use toml;

#[derive(Serialize, Deserialize, Clone)]
pub struct ClientConfig {
    /// The numerical ID of the current host. Note that when running in online mode, the value
    /// specified in the configuration file is ignored and instead the worker gets the value from
    /// figaro after registering on startup.
    pub client_id: u16,
    pub worker_rpc_server_port: u16,
    pub execution_subsystem_port: u16,
}

/// Contains all subcomponent configuration objects.
#[derive(Serialize, Deserialize, Clone)]
pub struct Config {
    pub(crate) hostname: String,
    /// Configuration of a worker's async runtime.
    pub async_lib_config: AsyncLibConfig,
    /// Object location manager configuration.
    pub location_manager_config: LocationManagerConfig,
    /// Information about the global ownership orchestrator the worker should connect to.
    pub ownership_tracker_config: OwnershipTrackerConfig,
    /// Configuration of the execution subsystem frontend. No need to modify the default values.
    pub client_config: ClientConfig,
    /// Configuration of the execution subsystem.
    pub nando_lib_config: NandoLibConfig,
    /// Configuration of the logging subsystem. Note that the number of executor threads must match
    /// the number of worker threads specified in [`NandoLibConfig`].
    pub logging_config: LoggingConfig,
    /// Worker RPC server configuration for interworker communication.
    pub scheduling_config: SchedulingConfig,
}

impl Config {
    pub fn init_from_file(config_file: PathBuf) -> Self {
        let config_str = read_to_string(&config_file)
            .expect(&format!("failed to open config file {:#?}", config_file));

        let mut config: Self = toml::from_str(&config_str).unwrap();
        match env::var("CLIENT_ID") {
            // FIXME @hack
            Ok(v) => {
                let client_id: u16 = v
                    .parse()
                    .expect("failed to parse client id from command line");

                config.client_config.client_id = client_id;
                config.location_manager_config.client_id = client_id;
            }
            Err(_e) => (),
        };

        match env::var("MAGPIE_WORKER_THREADS") {
            Ok(v) => {
                let num_worker_threads: u16 =
                    v.parse().expect("failed to parse worker threads env var");

                config.nando_lib_config.executor_config.num_worker_threads = num_worker_threads;
                config.logging_config.num_executor_threads = num_worker_threads;
            }
            Err(_e) => (),
        };

        match config.hostname.is_empty() {
            false => {}
            true => config.hostname = env::var("HOSTNAME").unwrap(),
        }

        // Sanity checks
        let logging_config_threads = config.logging_config.num_executor_threads;
        let executor_config_threads = config.nando_lib_config.executor_config.num_worker_threads;
        if logging_config_threads != executor_config_threads {
            panic!(
                "Mismatch in executor thread configs: executor config specifies {} threads, but logging config specifies {}",
                executor_config_threads,
                logging_config_threads,
            );
        }

        config
    }
}
