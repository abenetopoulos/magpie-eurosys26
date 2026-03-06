use std::path::PathBuf;

use clap::{ArgGroup, Parser};
use load_gen_api::ExperimentSetup;

#[derive(Parser, Debug, Clone)]
pub struct Args {
    #[arg(long)]
    pub config_file_path: Option<PathBuf>,

    #[arg(short('l'), long, default_value("false"))]
    pub local_mode: bool,

    #[arg(long, default_value("false"))]
    pub dump_to_file: bool,

    // TODO consider using flatten and extracting the local_mode group into its own struct.
    #[arg(
        short('t'),
        long,
        value_enum,
        required_if_eq("local_mode", "true"),
        default_value = "magpie"
    )]
    pub sut: load_gen_api::Sut,

    #[arg(
        short('c'),
        long,
        default_value("1"),
        required_if_eq("local_mode", "true")
    )]
    pub concurrency: u16,

    #[arg(short('s'), long, default_value("51017"))]
    pub seed: u64,

    #[arg(
        short('f'),
        long,
        value_enum,
        required_if_eq("local_mode", "true"),
        default_value = "noop"
    )]
    pub function: load_gen_api::Function,

    // Number of keys for kvs workloads
    #[arg(
        short('i'),
        long,
        required_if_eq("local_mode", "true"),
        default_value = "1000"
    )]
    pub num_input_objects: u64,

    #[arg(long, required_if_eq("dump_to_file", "true"), default_value = "1000")]
    pub num_iterations: u64,

    #[arg(
        short('d'),
        long,
        required_if_eq("local_mode", "true"),
        default_value = "10"
    )]
    pub request_duration_sec: u64,

    #[arg(long, required_if_eq("local_mode", "true"))]
    pub output_file: Option<PathBuf>,

    #[arg(long, required_if_eq("local_mode", "true"))]
    pub read_write_ratio: Option<f32>,

    // The below only applies to skewed workloads
    #[arg(short('e'), long, default_value("1.0"))]
    pub exponent: Option<f32>,

    #[arg(short('r'), long)]
    pub root_object: Option<u128>,

    #[arg(long)]
    pub bucket_objects: Vec<u128>,

    #[arg(long)]
    pub worker_hosts: Option<Vec<String>>,

    #[arg(long)]
    pub num_buckets_per_host: Option<u16>,

    #[arg(long, default_value("4"))]
    pub num_multi_get_keys: u32,

    #[arg(long, default_value("1"))]
    pub interval_secs: u64,

    #[arg(long)]
    pub graph_file: Option<String>,

    #[arg(long, default_value("false"))]
    pub fetch_locally: bool,

    #[arg(long, default_value("false"))]
    pub only_load_graph: bool,

    #[arg(long)]
    pub plan: Option<String>,

    #[arg(long)]
    pub string_size_kb: Option<usize>,

    #[arg(long)]
    pub num_chunks: Option<usize>,

    #[arg(long)]
    pub sort_num_output_partitions: Option<usize>,

    #[arg(long, default_value = "false")]
    pub use_fold: bool,
}

impl From<&ExperimentSetup> for Args {
    fn from(value: &ExperimentSetup) -> Self {
        let use_redis = match value.sut {
            load_gen_api::Sut::Redis => true,
            _ => false,
        };

        let use_redis_transactions = match value.sut {
            load_gen_api::Sut::Redis => true,
            _ => false,
        };
        Self {
            sut: value.sut,
            local_mode: false,
            dump_to_file: false,
            config_file_path: None,
            concurrency: value.concurrency,
            seed: value.seed,
            function: value.function,
            num_input_objects: value.num_input_objects,
            num_iterations: 0,
            request_duration_sec: value.request_duration_sec,
            output_file: Some(PathBuf::from(value.output_file.clone())),
            read_write_ratio: value.read_write_ratio.or(Some(1.0)),
            exponent: value.exponent.or(Some(1.0)),
            root_object: value.root_object.clone(),
            bucket_objects: value.bucket_objects.clone(),
            worker_hosts: Some(value.target_hosts.clone()),
            num_buckets_per_host: value.num_buckets_per_host.clone(),
            num_multi_get_keys: match value.num_multi_get_keys {
                Some(m) => m,
                None => 4,
            },
            interval_secs: match value.interval_secs {
                None => 1,
                Some(i) => i,
            },

            graph_file: value.graph_file.clone(),
            fetch_locally: false,
            only_load_graph: true,

            string_size_kb: value.string_size_kb,
            num_chunks: value.num_chunks,
            plan: value.plan.clone(),

            sort_num_output_partitions: value.sort_num_output_partitions.clone(),

            use_fold: value.use_fold,
        }
    }
}
