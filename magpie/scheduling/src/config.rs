use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone)]
pub struct SchedulerConfig {
    pub host: String,
    pub port: u16,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Config {
    pub scheduler_config: SchedulerConfig,
    pub worker_rpc_server_port: u16,
}
