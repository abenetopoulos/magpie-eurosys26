use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone)]
pub struct ExecutorConfig {
    /// Number of worker threads in the nando executor.
    pub num_worker_threads: u16,
    /// Size of worker thread input queue, in number of transaction activations.
    pub input_channel_capacity: u16,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct SchedulerConfig {
    /// Size of transaction manager <-> scheduler channel, in number of transaction activations.
    pub input_channel_capacity: u16,
    /// Size of log manager <-> scheduler channel, in number of transaction activations.
    pub log_channel_capacity: u16,
    /// Paths of nando libraries to be loaded.
    pub library_paths: HashMap<String, String>,

    /// Path of directory containing physical plans.
    pub physical_plans_path: Option<PathBuf>,

    /// Number of completion threads to spawn.
    pub num_completion_threads: u16,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Config {
    pub scheduler_config: SchedulerConfig,
    pub executor_config: ExecutorConfig,
}
