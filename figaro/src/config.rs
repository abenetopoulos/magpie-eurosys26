use std::fs::read_to_string;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use toml;

#[derive(Serialize, Deserialize, Clone)]
pub struct Config {
    // NOTE Set to 4 or 8.
    pub worker_threads: u16,
    pub worker_hosts: Vec<String>,

    pub server_port: u16,
}

impl Config {
    pub fn init_from_file(config_file: PathBuf) -> Self {
        let config_str = read_to_string(&config_file)
            .expect(&format!("failed to open config file {:#?}", config_file));
        let mut config: Self = toml::from_str(&config_str).unwrap();

        // Sanity checks
        if config.worker_threads == 0 {
            config.worker_threads = 4;
        }

        if config.server_port == 0 {
            config.server_port = 8080;
        }

        config
    }
}
