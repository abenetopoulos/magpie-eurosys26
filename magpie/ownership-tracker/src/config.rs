use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone)]
pub struct SchedulerConfig {
    pub host: String,
    pub port: u16,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Config {
    pub ownership_lease_duration_secs: u32,
    pub scheduler_config: SchedulerConfig,
}
