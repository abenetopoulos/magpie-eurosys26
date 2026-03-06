use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum RuntimeType {
    TokioMultiThread,
    RuntimePerCore,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Config {
    pub num_threads: u16,
    pub event_interval: Option<u32>,
    pub global_queue_interval: Option<u32>,
    pub kind: Option<RuntimeType>,
}
