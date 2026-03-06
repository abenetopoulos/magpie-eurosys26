use std::fs::read_to_string;
use std::path::PathBuf;

use serde_derive::{Deserialize, Serialize};
use toml;

#[derive(Serialize, Deserialize, Clone)]
pub struct Config {
    pub load_gen_hosts: Vec<String>,
    pub load_gen_port: u16,

    pub max_clients_per_load_gen_host: u16,
    pub num_concurrent_clients: Vec<u16>,

    pub worker_hosts: Vec<String>,
    pub worker_key_file_path: String,

    pub output_path: String,

    pub ssh_user: String,

    pub figaro_host: String,
    pub figaro_host_port: u16,

    pub num_iterations: usize,
    pub num_worker_threads: Vec<usize>,

    pub num_input_objects: u64,
    // NOTE for multiple hosts, this will be the base seed
    pub seed: u64,

    pub magpie_root: String,
    pub magpie_config: String,

    pub bucket_objects: Vec<String>,

    pub graph_input: String,
    pub graph_root_object: String,

    pub sizes_kb: Vec<usize>,
    pub num_chunks: Vec<usize>,

    pub root_allocation_dir: String,

    pub sort_num_input_partitions: Vec<usize>,
    pub sort_num_output_partitions: Vec<usize>,
}

impl Config {
    pub fn init_from_file(config_file: PathBuf) -> Self {
        let config_str = read_to_string(&config_file)
            .expect(&format!("failed to open config file {:#?}", config_file));

        let config: Self = toml::from_str(&config_str).unwrap();
        config
    }
}
