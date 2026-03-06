use std::fmt;

use clap::ValueEnum;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Copy, Clone, ValueEnum, PartialEq, Eq)]
pub enum Function {
    Healthcheck,
    Noop,
    MixedKvs,
    MixedKvsNando,
    MixedKvsMultihost,
    MixedKvsSkewed,
    MixedKvsSkewedBatchResetting,
    MixedKvsSkewedDistributedResetting,
    ReadModifyWrite,
    ReadModifyWriteNando,
    ReadModifyWriteMultihost,
    PageRank,
    TriangleCount,
    GraphTraversal,
    SmithWaterman,
    Sorting,
}

impl fmt::Display for Function {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::Healthcheck => f.write_str("healthcheck"),
            Self::Noop => f.write_str("noop"),
            Self::MixedKvs => f.write_str("mixed_kvs"),
            Self::MixedKvsNando => f.write_str("mixed_kvs_nando"),
            Self::MixedKvsMultihost => f.write_str("mixed_kvs_multi"),
            Self::MixedKvsSkewed => f.write_str("mixed_kvs_skewed"),
            Self::MixedKvsSkewedBatchResetting => f.write_str("mixed_kvs_skewed_batch_resetting"),
            Self::MixedKvsSkewedDistributedResetting => {
                f.write_str("mixed_kvs_skewed_distributed_resetting")
            }
            Self::ReadModifyWrite => f.write_str("rmw"),
            Self::ReadModifyWriteNando => f.write_str("rmw_nando"),
            Self::ReadModifyWriteMultihost => f.write_str("rmw_multi"),
            Self::PageRank => f.write_str("pagerank"),
            Self::TriangleCount => f.write_str("tc"),
            Self::GraphTraversal => f.write_str("traversal"),
            Self::SmithWaterman => f.write_str("smith_waterman"),
            Self::Sorting => f.write_str("sorting"),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
pub enum Sut {
    Magpie,
    Redis,
    RedisTransactions,
    Memcached,
}

impl fmt::Display for Sut {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::Magpie => f.write_str("magpie"),
            Self::Redis => f.write_str("redis"),
            Self::RedisTransactions => f.write_str("redis_txns"),
            Self::Memcached => f.write_str("memcached"),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ExperimentSetup {
    pub sut: Sut,
    pub concurrency: u16,
    pub seed: u64,
    pub request_duration_sec: u64,

    pub function: Function,
    pub num_input_objects: u64,
    // Only applies to kvs workloads
    pub read_write_ratio: Option<f32>,
    // only applies to skewed kvs workloads
    pub exponent: Option<f32>,

    pub output_file: String,

    pub target_hosts: Vec<String>,

    pub root_object: Option<u128>,
    pub bucket_objects: Vec<u128>,
    pub num_buckets_per_host: Option<u16>,

    pub num_multi_get_keys: Option<u32>,
    pub interval_secs: Option<u64>,

    pub graph_file: Option<String>,
    pub plan: Option<String>,

    pub string_size_kb: Option<usize>,
    pub num_chunks: Option<usize>,

    pub sort_num_output_partitions: Option<usize>,

    pub use_fold: bool,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ExperimentDeadline {
    // unix timestamp
    pub deadline: chrono::DateTime<chrono::Utc>,
}

// NOTE this should contain the status of the current experiment, but whatever.
#[derive(Serialize, Deserialize, Debug)]
pub struct ExperimentFinished {
    pub finished: bool,
}
