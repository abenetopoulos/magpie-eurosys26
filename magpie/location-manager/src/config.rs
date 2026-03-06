use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone)]
pub struct RSyncConfig {
    pub block_size: u32,
    pub hash_size: u32,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Config {
    /// The numerical ID of the current host (index into the [`Config::data_server_hosts`] field).
    pub client_id: u16,
    pub data_server_hosts: Vec<String>,
    pub data_server_port: u16,

    pub rsync_config: RSyncConfig,
}
