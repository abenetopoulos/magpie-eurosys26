use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone)]
pub struct Config {
    /// Number of worker threads in the nando executor.
    pub num_executor_threads: u16,
}
