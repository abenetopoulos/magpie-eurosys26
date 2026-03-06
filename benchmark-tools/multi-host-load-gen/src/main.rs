#![feature(slice_flatten)]
use std::collections::{hash_map::DefaultHasher, HashMap};
use std::fmt::Display;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::{atomic::AtomicBool, atomic::Ordering, Arc};
use std::thread::{self, sleep};
use std::time::{Duration, Instant};

use actix_web::{App, HttpServer};
use clap::{ArgGroup, Parser};
use config::Config;
use load_gen_api::{ExperimentSetup, Function as LoadGenFunction};
use memcache;
use nando_support::activation_intent::{
    NandoActivationExecutionStatus, NandoActivationIntentSerializable, NandoActivationResolution,
    NandoActivationStatus, NandoArgumentSerializable, NandoResultSerializable,
};
use nando_support::{ecb_id, epic_control, epic_definitions};
use object_lib::IPtr;
use rand::distributions::{Distribution, WeightedIndex};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use redis::{self, Commands};
use reqwest;
use serde::{Deserialize, Serialize};
use slog::{info, o, Drain};
use zipf::ZipfDistribution;

use crate::cli::Args;
use crate::experiment::ExperimentRunner;

mod cli;
mod config;
mod experiment;
mod handlers;

fn parse_args() -> Args {
    let mut args = Args::parse();

    if args.config_file_path.is_none() {
        args.config_file_path = Some("config.toml".into());
    }

    args
}

#[derive(Copy, Clone)]
enum KvsCommand {
    Get,
    Put,
    MGet,
}

impl Display for KvsCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self {
            Self::Get => f.write_str("get"),
            Self::Put => f.write_str("put"),
            Self::MGet => f.write_str("mget"),
        }
    }
}

enum Client {
    Magpie(reqwest::blocking::Client),
    Redis(redis::Client),
    Memcached(memcache::Client),
}

impl Client {
    fn get_magpie_client(self) -> reqwest::blocking::Client {
        match self {
            Self::Magpie(c) => c,
            _ => panic!("wrong client"),
        }
    }

    fn get_redis_client(self) -> redis::Client {
        match self {
            Self::Redis(c) => c,
            _ => panic!("wrong client"),
        }
    }

    fn get_memcached_client(self) -> memcache::Client {
        match self {
            Self::Memcached(c) => c,
            _ => panic!("wrong client"),
        }
    }
}

pub type VertexId = usize;
fn graph_traversal(
    thread_id: u16,
    hostname: String,
    args: Args,
    client: Client,
    control_tuple: (Arc<AtomicBool>, Arc<AtomicBool>),
) -> (u16, u64) {
    let drain = match args.output_file {
        Some(p) => {
            let pathbuf = p.clone();
            let path = pathbuf.as_path();
            let filename = path.file_name().unwrap();
            let path = path.with_file_name(format!("{}.{}", filename.to_str().unwrap(), thread_id));
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(true)
                .open(path)
                .unwrap();

            let decorator = slog_term::PlainDecorator::new(file);
            let drain = slog_term::FullFormat::new(decorator).build().fuse();
            slog_async::Async::new(drain)
                .chan_size(4 * 8192 as usize)
                .overflow_strategy(slog_async::OverflowStrategy::Block)
                .build()
                .fuse()
        }
        None => {
            let decorator = slog_term::PlainDecorator::new(std::io::stdout());
            let drain = slog_term::FullFormat::new(decorator).build().fuse();
            slog_async::Async::new(drain)
                .chan_size(4 * 8192 * args.concurrency as usize)
                .overflow_strategy(slog_async::OverflowStrategy::Block)
                .build()
                .fuse()
        }
    };
    let log = slog::Logger::root(drain, o!());

    let Some(graph_file) = args.graph_file else {
        panic!("missing graph file in load gen args");
    };

    let mut total_num_requests = 0;

    match args.sut {
        load_gen_api::Sut::Magpie => {
            let client = client.get_magpie_client();
            let url = format!("http://{}:52017/activation_router/schedule", hostname);

            // parse graph
            total_num_requests += 1;
            let parse_request = client.post(&url).json(&NandoActivationIntentSerializable {
                name: "nano4r::parse_graph".to_string(),
                host_idx: None,
                args: vec![
                    NandoArgumentSerializable::Value(graph_file.into()),
                    NandoArgumentSerializable::Value(1usize.into()),
                    NandoArgumentSerializable::Value(true.into()),
                ],
                with_plan: None,
            });

            let resp = parse_request.send().expect("failed to parse graph");
            let graph_object = match resp.json::<NandoActivationResolution>() {
                Ok(s) => s.output[0].clone(),
                Err(e) => panic!("could not get id of output object: {:?}", e),
            };

            total_num_requests += 1;
            let start = Instant::now();
            let parse_request = client.post(&url).json(&NandoActivationIntentSerializable {
                name: "nano4r::dfs".to_string(),
                host_idx: None,
                args: vec![graph_object, NandoArgumentSerializable::Value(true.into())],
                with_plan: None,
            });

            let traversal_duration = start.elapsed();
            info!(
                log,
                "Took {}s, {}ms, {}us to traverse graph",
                traversal_duration.as_secs(),
                traversal_duration.subsec_millis(),
                traversal_duration.subsec_micros(),
            );
        }
        load_gen_api::Sut::Memcached | load_gen_api::Sut::Redis => {
            let path = Path::new(&graph_file);
            let mut file_options = fs::OpenOptions::new();
            file_options
                .read(true)
                .write(false)
                .create(false)
                .truncate(false);

            let mut graph_representation: HashMap<VertexId, Vec<VertexId>> = HashMap::new();
            let mut visited: HashMap<VertexId, bool> = HashMap::new();

            let mut min_idx = VertexId::MAX;
            let mut max_idx = VertexId::MIN;

            match file_options.open(path) {
                Ok(f) => {
                    let line_reader = BufReader::new(f);
                    for line in line_reader.lines() {
                        let Ok(line) = line else {
                            break;
                        };

                        if line.starts_with('#') {
                            continue;
                        }

                        let vertices = line.split_once(|c| c == ' ' || c == '\t').unwrap();
                        let source: VertexId = match vertices.0.parse() {
                            Err(_) => continue,
                            Ok(s) => s,
                        };
                        visited.insert(source, false);
                        let dest: VertexId = vertices.1.parse().unwrap();
                        visited.insert(dest, false);

                        min_idx = std::cmp::min(std::cmp::min(min_idx, source), dest);
                        max_idx = std::cmp::max(std::cmp::max(max_idx, source), dest);

                        graph_representation
                            .entry(source)
                            .and_modify(|al| al.push(dest))
                            .or_insert(vec![dest]);
                        graph_representation
                            .entry(dest)
                            .and_modify(|al| al.push(source))
                            .or_insert(vec![source]);
                    }
                }
                Err(e) => {
                    eprintln!(
                        "could not open graph file {:?} from wd {:?}: {}",
                        path,
                        std::env::current_dir(),
                        e
                    );
                    panic!();
                }
            }

            let mut network_duration = Duration::default();
            match args.sut {
                load_gen_api::Sut::Redis => {
                    let client = client.get_redis_client();
                    let mut con = client.get_connection().unwrap();

                    for (src, dests) in graph_representation.into_iter() {
                        con.lpush::<VertexId, Vec<VertexId>, ()>(src, dests)
                            .expect("failed to lpush");
                    }

                    if args.only_load_graph {
                        if args.sut == load_gen_api::Sut::Redis {
                            let script_as_str = {
                                let path = Path::new(&"../../utils/redis-dfs.lua");
                                fs::read_to_string(path).expect("failed to read redis dfs script")
                            };
                            let script = redis::Script::new(&script_as_str);
                            let start = Instant::now();
                            let result: () = script
                                .arg(min_idx)
                                .arg(max_idx)
                                .invoke(&mut con)
                                .expect("failed to invoke script");
                            let traversal_duration = start.elapsed();
                            info!(
                                log,
                                "Took {}s, {}ms, {}us to traverse graph in redis udf",
                                traversal_duration.as_secs(),
                                traversal_duration.subsec_millis(),
                                traversal_duration.subsec_micros(),
                            );
                        }
                        return (thread_id, 0);
                    }

                    use std::collections::VecDeque;

                    let mut dfs_queue: VecDeque<VertexId> = VecDeque::new();
                    let start = Instant::now();
                    if !args.fetch_locally {
                        loop {
                            let unvisited = match dfs_queue.pop_front() {
                                None => {
                                    let unvisited = match visited.iter().find(|(k, v)| !*v) {
                                        None => break,
                                        Some((u, _)) => *u,
                                    };
                                    visited.insert(unvisited, true);
                                    unvisited
                                }
                                Some(u) => u,
                            };

                            let request_start = Instant::now();
                            let num_adjacent_verts = con.llen(unvisited).unwrap();
                            let adjacent_verts = con
                                .lrange::<VertexId, Vec<VertexId>>(unvisited, 0, num_adjacent_verts)
                                .unwrap();
                            network_duration += request_start.elapsed();
                            total_num_requests += 2;
                            for vert in adjacent_verts.into_iter().rev() {
                                match visited.get(&vert) {
                                    Some(true) => continue,
                                    Some(false) => {
                                        visited.insert(vert, true);
                                        dfs_queue.push_front(vert);
                                    }
                                    None => unreachable!(),
                                }
                            }
                        }
                    } else {
                        let mut graph_representation = HashMap::with_capacity(max_idx);
                        for vertex_id in min_idx..=max_idx {
                            let request_start = Instant::now();
                            let num_adjacent_verts = con.llen(vertex_id).unwrap();
                            if num_adjacent_verts == 0 {
                                graph_representation.insert(vertex_id, vec![]);
                                network_duration += request_start.elapsed();
                                total_num_requests += 1;
                                continue;
                            }

                            let adjacent_verts = con
                                .lrange::<VertexId, Vec<VertexId>>(vertex_id, 0, num_adjacent_verts)
                                .unwrap();
                            total_num_requests += 2;
                            graph_representation.insert(vertex_id, adjacent_verts);
                            network_duration += request_start.elapsed();
                        }

                        loop {
                            let unvisited = match dfs_queue.pop_front() {
                                None => {
                                    let unvisited = match visited.iter().find(|(k, v)| !*v) {
                                        None => break,
                                        Some((u, _)) => *u,
                                    };
                                    visited.insert(unvisited, true);
                                    unvisited
                                }
                                Some(u) => u,
                            };

                            for vert in graph_representation.get(&unvisited).unwrap().into_iter() {
                                match visited.get(&vert) {
                                    Some(true) => continue,
                                    Some(false) => {
                                        visited.insert(*vert, true);
                                        dfs_queue.push_front(*vert);
                                    }
                                    None => unreachable!(),
                                }
                            }
                        }
                    }
                    let traversal_duration = start.elapsed();
                    info!(
                        log,
                        "Took {}s, {}ms, {}us to traverse graph in redis, {total_num_requests} requests",
                        traversal_duration.as_secs(),
                        traversal_duration.subsec_millis(),
                        traversal_duration.subsec_micros(),
                    );
                    info!(
                        log,
                        "Spent {}s, {}ms, {}us waiting on the network",
                        network_duration.as_secs(),
                        network_duration.subsec_millis(),
                        network_duration.subsec_micros(),
                    );
                }
                load_gen_api::Sut::Memcached => {
                    let client = client.get_memcached_client();
                }
                _ => unreachable!(),
            }
        }
        _ => unreachable!(),
    }

    (thread_id, total_num_requests)
}

fn smith_waterman(
    thread_id: u16,
    hostname: String,
    args: Args,
    client: Client,
    control_tuple: (Arc<AtomicBool>, Arc<AtomicBool>),
) -> (u16, u64) {
    let drain = match args.output_file {
        Some(p) => {
            let pathbuf = p.clone();
            let path = pathbuf.as_path();
            let filename = path.file_name().unwrap();
            let path = path.with_file_name(format!("{}.{}", filename.to_str().unwrap(), thread_id));
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(true)
                .open(path)
                .unwrap();

            let decorator = slog_term::PlainDecorator::new(file);
            let drain = slog_term::FullFormat::new(decorator).build().fuse();
            slog_async::Async::new(drain)
                .chan_size(4 * 8192 as usize)
                .overflow_strategy(slog_async::OverflowStrategy::Block)
                .build()
                .fuse()
        }
        None => {
            let decorator = slog_term::PlainDecorator::new(std::io::stdout());
            let drain = slog_term::FullFormat::new(decorator).build().fuse();
            slog_async::Async::new(drain)
                .chan_size(4 * 8192 * args.concurrency as usize)
                .overflow_strategy(slog_async::OverflowStrategy::Block)
                .build()
                .fuse()
        }
    };
    let log = slog::Logger::root(drain, o!());
    let mut total_num_requests = 0;

    loop {
        // start signal
        if control_tuple.0.load(Ordering::Relaxed) == true {
            break;
        }
    }

    let num_chunks = match args.num_chunks {
        None => 1,
        Some(nc) => nc,
    };

    let string_size_kb = match args.string_size_kb {
        None => 1,
        Some(s) => s,
    };
    let plan = args.plan;

    match args.sut {
        load_gen_api::Sut::Magpie => {
            let client = client.get_magpie_client();
            let url = format!("http://{}:52017/activation_router/schedule", hostname);

            let sw_request = client.post(&url).json(&NandoActivationIntentSerializable {
                name: "smith_waterman::init_smith_waterman".to_string(),
                host_idx: None,
                args: vec![
                    NandoArgumentSerializable::get_nil(),
                    NandoArgumentSerializable::Value(string_size_kb.into()),
                    NandoArgumentSerializable::get_nil(),
                    NandoArgumentSerializable::Value(string_size_kb.into()),
                    NandoArgumentSerializable::Value(num_chunks.into()),
                ],
                with_plan: plan,
            });

            let start = Instant::now();
            let resp = sw_request.send().expect("failed to submit sw request");
            total_num_requests += 1;
            let request_duration = start.elapsed();
            info!(
                log,
                "Took {}s, {}ms, {}us to run sw",
                request_duration.as_secs(),
                request_duration.subsec_millis(),
                request_duration.subsec_micros(),
            );
        }
        _ => unreachable!(),
    }

    (thread_id, total_num_requests)
}

fn sorting(
    thread_id: u16,
    hostname: String,
    args: Args,
    client: Client,
    control_tuple: (Arc<AtomicBool>, Arc<AtomicBool>),
) -> (u16, u64) {
    let drain = match args.output_file {
        Some(p) => {
            let pathbuf = p.clone();
            let path = pathbuf.as_path();
            let filename = path.file_name().unwrap();
            let path = path.with_file_name(format!("{}.{}", filename.to_str().unwrap(), thread_id));
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(true)
                .open(path)
                .unwrap();

            let decorator = slog_term::PlainDecorator::new(file);
            let drain = slog_term::FullFormat::new(decorator).build().fuse();
            slog_async::Async::new(drain)
                .chan_size(4 * 8192 as usize)
                .overflow_strategy(slog_async::OverflowStrategy::Block)
                .build()
                .fuse()
        }
        None => {
            let decorator = slog_term::PlainDecorator::new(std::io::stdout());
            let drain = slog_term::FullFormat::new(decorator).build().fuse();
            slog_async::Async::new(drain)
                .chan_size(4 * 8192 * args.concurrency as usize)
                .overflow_strategy(slog_async::OverflowStrategy::Block)
                .build()
                .fuse()
        }
    };
    let log = slog::Logger::root(drain, o!());
    let mut total_num_requests = 0;

    loop {
        // start signal
        if control_tuple.0.load(Ordering::Relaxed) == true {
            break;
        }
    }

    let root_object_iptr = match args.root_object {
        None => unreachable!(),
        Some(oid) => IPtr::new(oid, 0, 0),
    };

    let num_output_partitions = args.sort_num_output_partitions.unwrap();

    let plan = args.plan;

    match args.sut {
        load_gen_api::Sut::Magpie => {
            let client = client.get_magpie_client();
            let url = format!("http://{}:52017/activation_router/schedule", hostname);

            let sw_request = client.post(&url).json(&NandoActivationIntentSerializable {
                name: "sorting::sort_collection_u64".to_string(),
                host_idx: None,
                args: vec![
                    NandoArgumentSerializable::Ref(root_object_iptr.into()),
                    NandoArgumentSerializable::Value(num_output_partitions.into()),
                    NandoArgumentSerializable::Value(false.into()),
                    NandoArgumentSerializable::Value(args.use_fold.into()),
                    NandoArgumentSerializable::Value(false.into()),
                    NandoArgumentSerializable::Value(false.into()),
                ],
                with_plan: plan,
            });

            let start = Instant::now();
            let resp = sw_request.send().expect("failed to submit sw request");
            total_num_requests += 1;
            let request_duration = start.elapsed();
            info!(
                log,
                "Took {}s, {}ms, {}us to run sort",
                request_duration.as_secs(),
                request_duration.subsec_millis(),
                request_duration.subsec_micros(),
            );
        }
        _ => unreachable!(),
    }

    (thread_id, total_num_requests)
}

fn magpie_workload_loop(
    thread_id: u16,
    hostname: String,
    args: Args,
    client: reqwest::blocking::Client,
    control_tuple: (Arc<AtomicBool>, Arc<AtomicBool>),
) -> (u16, u64) {
    let min_output_object_idx = args.num_input_objects + 1000;

    let drain = match args.output_file {
        Some(p) => {
            let pathbuf = p.clone();
            let path = pathbuf.as_path();
            let filename = path.file_name().unwrap();
            let path = path.with_file_name(format!("{}.{}", filename.to_str().unwrap(), thread_id));
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(true)
                .open(path)
                .unwrap();

            let decorator = slog_term::PlainDecorator::new(file);
            let drain = slog_term::FullFormat::new(decorator).build().fuse();
            slog_async::Async::new(drain)
                .chan_size(4 * 8192 as usize)
                .overflow_strategy(slog_async::OverflowStrategy::Block)
                .build()
                .fuse()
        }
        None => {
            let decorator = slog_term::PlainDecorator::new(std::io::stdout());
            let drain = slog_term::FullFormat::new(decorator).build().fuse();
            slog_async::Async::new(drain)
                .chan_size(4 * 8192 * args.concurrency as usize)
                .overflow_strategy(slog_async::OverflowStrategy::Block)
                .build()
                .fuse()
        }
    };
    let log = slog::Logger::root(drain, o!());

    let commands = if args.function == LoadGenFunction::MixedKvsSkewedDistributedResetting
        || args.function == LoadGenFunction::MixedKvsSkewedBatchResetting
    {
        [KvsCommand::MGet, KvsCommand::Put]
    } else {
        [KvsCommand::Get, KvsCommand::Put]
    };

    let read_write_ratio_weights: [f32; 2] = match args.read_write_ratio {
        None => [1.0, 1.0], // won't be used
        Some(w) => [w, 1.0 - w],
    };
    let weighted_distr =
        WeightedIndex::new(&read_write_ratio_weights).expect("failed to create weighted index");

    let root_object_iptr = match args.root_object {
        // NOTE we'll simply ignore this
        None => IPtr::new(0, 0, 0),
        Some(oid) => IPtr::new(oid, 0, 0),
    };

    let mut total_num_requests = 0;
    let url = match args.function {
        LoadGenFunction::Healthcheck => {
            format!("http://{}:52017/activation_router/healthcheck", hostname)
        }
        _ => format!("http://{}:52017/activation_router/schedule", hostname),
    };

    let output_object = if args.function == LoadGenFunction::MixedKvsSkewedDistributedResetting
        || args.function == LoadGenFunction::MixedKvsSkewedBatchResetting
    {
        let resp = client
            .post(&url)
            .json(&NandoActivationIntentSerializable {
                name: "kvs_consumer::init_multi_get_batch_i32".to_string(),
                host_idx: None,
                args: vec![NandoArgumentSerializable::Value(
                    args.num_multi_get_keys.into(),
                )],
                with_plan: None,
            })
            .send()
            .expect("failed to submit init batch");

        match resp.json::<NandoActivationResolution>() {
            Ok(s) => Some(s.output[0].clone()),
            Err(e) => panic!("could not get id of output object: {:?}", e),
        }
    } else {
        None
    };

    let input_zipf = {
        let exponent = match args.exponent {
            Some(e) => e as f64,
            // Doesn't matter what value we pick here, as we won't use the zipf generator.
            None => 1.0,
        };

        ZipfDistribution::new(args.num_input_objects.try_into().unwrap(), exponent).unwrap()
    };

    let seed_iter: [u8; 8] = (args.seed + thread_id as u64).to_ne_bytes();
    let nested_seed = [seed_iter; 4];
    let seed: [u8; 32] = nested_seed.flatten().try_into().expect("oops");
    let mut rng: StdRng = SeedableRng::from_seed(seed);

    loop {
        // start signal
        if control_tuple.0.load(Ordering::Relaxed) == true {
            break;
        }
    }

    let start = Instant::now();
    // let epic_results_url = format!("http://{}:52017/activation_router/epic/await_result", hostname);

    #[allow(unused_assignments)]
    let mut request_log_str = "";
    let mut start_instant = Instant::now();
    let mut num_resets = 0;
    loop {
        // stop signal
        if control_tuple.1.load(Ordering::Relaxed) == true {
            break;
        }

        rng = if args.function == LoadGenFunction::MixedKvsSkewedBatchResetting
            || args.function == LoadGenFunction::MixedKvsSkewedDistributedResetting
        {
            let elapsed = start_instant.elapsed().as_secs();

            if elapsed == args.interval_secs {
                info!(log, "Resetting rng after {}s interval", args.interval_secs);

                num_resets += 1;
                start_instant = Instant::now();
                let seed_iter: [u8; 8] =
                    (args.seed + (thread_id * (num_resets + 10)) as u64).to_ne_bytes();
                let nested_seed = [seed_iter; 4];
                let seed: [u8; 32] = nested_seed.flatten().try_into().expect("oops");
                SeedableRng::from_seed(seed)
            } else {
                rng
            }
        } else {
            rng
        };

        let request_builder = match args.function {
            LoadGenFunction::Healthcheck => {
                request_log_str = "healthcheck";
                client.get(&url)
            }
            LoadGenFunction::MixedKvs | LoadGenFunction::MixedKvsMultihost => {
                assert!(
                    root_object_iptr.get_object_id() != 0,
                    "need a non-zero object as kvs root object"
                );
                let key = format!("key-{}", rng.gen_range(0..args.num_input_objects));
                match commands[weighted_distr.sample(&mut rng)] {
                    KvsCommand::Put => {
                        request_log_str = "kvs_consumer::put_i32";

                        client.post(&url).json(&NandoActivationIntentSerializable {
                            name: request_log_str.to_string(),
                            host_idx: None,
                            args: vec![
                                NandoArgumentSerializable::Ref(root_object_iptr),
                                NandoArgumentSerializable::Value(key.into()),
                                NandoArgumentSerializable::Value(rng.gen_range(0..1000000).into()),
                            ],
                            with_plan: None,
                        })
                    }
                    KvsCommand::Get => {
                        request_log_str = "kvs_consumer::get_i32";

                        client.post(&url).json(&NandoActivationIntentSerializable {
                            name: request_log_str.to_string(),
                            host_idx: None,
                            args: vec![
                                NandoArgumentSerializable::Ref(root_object_iptr),
                                NandoArgumentSerializable::Value(key.into()),
                            ],
                            with_plan: None,
                        })
                    }
                    _ => unreachable!(),
                }
            }
            LoadGenFunction::MixedKvsNando => {
                let key = format!("key-{}", rng.gen_range(0..args.num_input_objects));
                let bucket_iptr = compute_bucket_object_id(&args.bucket_objects, &key);
                let bucket_object_id = bucket_iptr.get_object_id();
                match commands[weighted_distr.sample(&mut rng)] {
                    KvsCommand::Put => {
                        request_log_str = "kvs_consumer::put_i32_internal";

                        client.post(&url).json(&NandoActivationIntentSerializable {
                            name: "kvs_consumer::put_i32_internal".to_string(),
                            host_idx: None,
                            args: vec![
                                NandoArgumentSerializable::Ref(bucket_iptr),
                                NandoArgumentSerializable::Value(key.into()),
                                NandoArgumentSerializable::Value(rng.gen_range(0..1000000).into()),
                            ],
                            with_plan: None,
                        })
                    }
                    KvsCommand::Get => {
                        request_log_str = "kvs_consumer::get_i32_internal";

                        client.post(&url).json(&NandoActivationIntentSerializable {
                            name: "kvs_consumer::get_i32_internal".to_string(),
                            host_idx: None,
                            args: vec![
                                NandoArgumentSerializable::Ref(bucket_iptr),
                                NandoArgumentSerializable::Value(key.into()),
                            ],
                            with_plan: None,
                        })
                    }
                    _ => unreachable!(),
                }
            }
            LoadGenFunction::MixedKvsSkewed => {
                assert!(
                    root_object_iptr.get_object_id() != 0,
                    "need a non-zero object as kvs root object"
                );
                let idx = input_zipf.sample(&mut rng);
                let key = format!("key-{}", idx);

                match commands[weighted_distr.sample(&mut rng)] {
                    KvsCommand::Put => {
                        request_log_str = "kvs_consumer::put_i32-skewed";

                        client.post(&url).json(&NandoActivationIntentSerializable {
                            name: "kvs_consumer::put_i32".to_string(),
                            host_idx: None,
                            args: vec![
                                NandoArgumentSerializable::Ref(root_object_iptr),
                                NandoArgumentSerializable::Value(key.into()),
                                NandoArgumentSerializable::Value(rng.gen_range(0..1000000).into()),
                            ],
                            with_plan: None,
                        })
                    }
                    KvsCommand::Get => {
                        request_log_str = "kvs_consumer::get_i32-skewed";

                        client.post(&url).json(&NandoActivationIntentSerializable {
                            name: "kvs_consumer::get_i32".to_string(),
                            host_idx: None,
                            args: vec![
                                NandoArgumentSerializable::Ref(root_object_iptr),
                                NandoArgumentSerializable::Value(key.into()),
                            ],
                            with_plan: None,
                        })
                    }
                    _ => unreachable!(),
                }
            }
            LoadGenFunction::MixedKvsSkewedBatchResetting => {
                assert!(
                    root_object_iptr.get_object_id() != 0,
                    "need a non-zero object as kvs root object"
                );
                let idx = input_zipf.sample(&mut rng);
                let key = format!("key-{}", idx);

                match commands[weighted_distr.sample(&mut rng)] {
                    KvsCommand::Put => {
                        request_log_str = "kvs_consumer::put_i32-skewed";

                        client.post(&url).json(&NandoActivationIntentSerializable {
                            name: "kvs_consumer::put_i32".to_string(),
                            host_idx: None,
                            args: vec![
                                NandoArgumentSerializable::Ref(root_object_iptr),
                                NandoArgumentSerializable::Value(key.into()),
                                NandoArgumentSerializable::Value(rng.gen_range(0..1000000).into()),
                            ],
                            with_plan: None,
                        })
                    }
                    KvsCommand::MGet => {
                        request_log_str = "kvs_consumer::multi_get_batch_i32-skewed";
                        let mut keys = Vec::with_capacity(args.num_multi_get_keys as usize);
                        keys.push(key);
                        for _ in 1..args.num_multi_get_keys {
                            keys.push(format!("key-{}", input_zipf.sample(&mut rng)));
                        }
                        let mut intent_args = vec![
                            NandoArgumentSerializable::Ref(root_object_iptr),
                            output_object.as_ref().unwrap().clone(),
                            NandoArgumentSerializable::Value(keys.into()),
                        ];

                        client.post(&url).json(&NandoActivationIntentSerializable {
                            name: "kvs_consumer::multi_get_batch_i32".to_string(),
                            host_idx: None,
                            args: intent_args,
                            with_plan: Some("kvs-multi-get-batch-collect".to_string()),
                        })
                    }
                    _ => unreachable!(),
                }
            }
            LoadGenFunction::MixedKvsSkewedDistributedResetting => {
                assert!(
                    root_object_iptr.get_object_id() != 0,
                    "need a non-zero object as kvs root object"
                );
                let idx = input_zipf.sample(&mut rng);
                let key = format!("key-{}", idx);

                match commands[weighted_distr.sample(&mut rng)] {
                    KvsCommand::Put => {
                        request_log_str = "kvs_consumer::put_i32-skewed";

                        client.post(&url).json(&NandoActivationIntentSerializable {
                            name: "kvs_consumer::put_i32".to_string(),
                            host_idx: None,
                            args: vec![
                                NandoArgumentSerializable::Ref(root_object_iptr),
                                NandoArgumentSerializable::Value(key.into()),
                                NandoArgumentSerializable::Value(rng.gen_range(0..1000000).into()),
                            ],
                            with_plan: None,
                        })
                    }
                    KvsCommand::MGet => {
                        request_log_str = "kvs_consumer::multi_get_i32-skewed";
                        let mut keys = Vec::with_capacity(args.num_multi_get_keys as usize);
                        keys.push(key);
                        for _ in 1..args.num_multi_get_keys {
                            keys.push(format!("key-{}", input_zipf.sample(&mut rng)));
                        }
                        let mut intent_args = vec![
                            NandoArgumentSerializable::Ref(root_object_iptr),
                            NandoArgumentSerializable::Value(keys.into()),
                        ];

                        client.post(&url).json(&NandoActivationIntentSerializable {
                            name: "kvs_consumer::multi_get_i32".to_string(),
                            host_idx: None,
                            args: intent_args,
                            with_plan: Some("kvs-multi-get".to_string()),
                        })
                    }
                    _ => unreachable!(),
                }
            }
            LoadGenFunction::ReadModifyWriteNando => {
                let key = format!("key-{}", rng.gen_range(0..args.num_input_objects));
                let bucket_iptr = compute_bucket_object_id(&args.bucket_objects, &key);
                let bucket_object_id = bucket_iptr.get_object_id();

                match commands[weighted_distr.sample(&mut rng)] {
                    KvsCommand::Put => {
                        request_log_str = "kvs_consumer::set_or_increment";

                        client.post(&url).json(&NandoActivationIntentSerializable {
                            name: request_log_str.to_string(),
                            host_idx: None,
                            args: vec![
                                NandoArgumentSerializable::Ref(bucket_iptr),
                                NandoArgumentSerializable::Value(key.into()),
                            ],
                            with_plan: None,
                        })
                    }
                    KvsCommand::Get => {
                        request_log_str = "kvs_consumer::get_u64_internal";

                        client.post(&url).json(&NandoActivationIntentSerializable {
                            name: "kvs_consumer::get_u64_internal".to_string(),
                            host_idx: None,
                            args: vec![
                                NandoArgumentSerializable::Ref(bucket_iptr),
                                NandoArgumentSerializable::Value(key.into()),
                            ],
                            with_plan: None,
                        })
                    }
                    _ => unreachable!(),
                }
            }
            LoadGenFunction::ReadModifyWrite | LoadGenFunction::ReadModifyWriteMultihost => {
                let key = format!("key-{}", rng.gen_range(0..args.num_input_objects));
                match commands[weighted_distr.sample(&mut rng)] {
                    KvsCommand::Put => {
                        request_log_str = match args.function {
                            LoadGenFunction::ReadModifyWrite => "kvs_consumer::get_and_increment",
                            LoadGenFunction::ReadModifyWriteMultihost => {
                                "kvs_consumer::get_and_increment_i32"
                            }
                            _ => unreachable!(),
                        };

                        client.post(&url).json(&NandoActivationIntentSerializable {
                            name: request_log_str.to_string(),
                            host_idx: None,
                            args: vec![
                                NandoArgumentSerializable::Ref(root_object_iptr),
                                NandoArgumentSerializable::Value(key.into()),
                            ],
                            with_plan: None,
                        })
                    }
                    KvsCommand::Get => {
                        request_log_str = match args.function {
                            LoadGenFunction::ReadModifyWrite => "kvs_consumer::get_u64",
                            LoadGenFunction::ReadModifyWriteMultihost => "kvs_consumer::get_i32",
                            _ => unreachable!(),
                        };

                        client.post(&url).json(&NandoActivationIntentSerializable {
                            name: request_log_str.to_string(),
                            host_idx: None,
                            args: vec![
                                NandoArgumentSerializable::Ref(root_object_iptr),
                                NandoArgumentSerializable::Value(key.into()),
                            ],
                            with_plan: None,
                        })
                    }
                    _ => unreachable!(),
                }
            }
            LoadGenFunction::Noop => {
                request_log_str = "noop";

                client.post(&url).json(&NandoActivationIntentSerializable {
                    name: "noop".to_string(),
                    host_idx: None,
                    args: vec![],
                    with_plan: None,
                })
            }
            _ => panic!("unrecognized function {}", args.function),
        };

        // println!("Sending Request: {:?}", &request_body.args);

        #[cfg(feature = "timing")]
        let request_start = Instant::now();

        let resp = match request_builder.timeout(Duration::from_secs(1)).send() {
            Ok(r) => r,
            Err(_) => {
                eprintln!("Request to worker schedule endpoint timed out, skipping.");
                continue;
            }
        };
        assert!(resp.status().is_success());
        let _resp_json = resp
            .json::<NandoActivationResolution>()
            .expect("failed to parse response as json");

        total_num_requests += 1;
        #[cfg(feature = "timing")]
        {
            let request_duration = request_start.elapsed();
            info!(
                log,
                "{} request latency: {}us",
                request_log_str,
                request_duration.as_micros()
            );
        }
    }

    let total_time = start.elapsed();

    let avg_throughput = (total_num_requests as f64) / total_time.as_secs_f64();

    println!(
        "Thread {} Total Time: {}ms",
        thread_id,
        total_time.as_secs_f64()
    );
    println!(
        "Thread {} Average throughput: {} txns/sec",
        thread_id, avg_throughput
    );

    info!(
        log,
        "Thread {} Total Time: {}ms",
        thread_id,
        total_time.as_secs_f64()
    );
    info!(
        log,
        "Thread {} Average throughput: {} txns/sec", thread_id, avg_throughput
    );

    (thread_id, total_num_requests)
}

fn compute_bucket_object_id(bucket_object_ids: &Vec<u128>, key: &str) -> IPtr {
    let key_hash = {
        let mut hasher = DefaultHasher::default();
        key.hash(&mut hasher);
        hasher.finish() as u128
    };

    let bucket_object_id = {
        let mut max_weight = 0;
        let mut max_weight_bucket = 0;
        let mult_constant = 1103515245;

        for bucket_object_id in bucket_object_ids {
            let bucket_object_id_digest: u128 = bucket_object_id & ((1u128 << 64) - 1);
            let weight: u128 = (mult_constant
                * ((mult_constant * bucket_object_id_digest + 12345) ^ key_hash)
                + 12345)
                % (2u128.pow(31) - 1);

            #[cfg(debug_assertions)]
            println!("weight for {} and {} is {}", key, bucket_object_id, weight);

            if max_weight >= weight {
                continue;
            }

            max_weight = weight;
            max_weight_bucket = *bucket_object_id;
        }

        max_weight_bucket
    };

    IPtr::new(bucket_object_id, 0, 0)
}

fn magpie_multihost_workload_loop(
    thread_id: u16,
    hostname: String,
    args: Args,
    client: reqwest::blocking::Client,
    control_tuple: (Arc<AtomicBool>, Arc<AtomicBool>),
) -> (u16, u64) {
    let seed_iter: [u8; 8] = (args.seed + thread_id as u64).to_ne_bytes();
    let nested_seed = [seed_iter; 4];
    let seed: [u8; 32] = nested_seed.flatten().try_into().expect("oops");
    let mut rng: StdRng = SeedableRng::from_seed(seed);

    let drain = match args.output_file {
        Some(p) => {
            let pathbuf = p.clone();
            let path = pathbuf.as_path();
            let filename = path.file_name().unwrap();
            let path = path.with_file_name(format!("{}.{}", filename.to_str().unwrap(), thread_id));
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(true)
                .open(path)
                .unwrap();

            let decorator = slog_term::PlainDecorator::new(file);
            let drain = slog_term::FullFormat::new(decorator).build().fuse();
            slog_async::Async::new(drain)
                .chan_size(4 * 8192 as usize)
                .overflow_strategy(slog_async::OverflowStrategy::Block)
                .build()
                .fuse()
        }
        None => {
            let decorator = slog_term::PlainDecorator::new(std::io::stdout());
            let drain = slog_term::FullFormat::new(decorator).build().fuse();
            slog_async::Async::new(drain)
                .chan_size(4 * 8192 * args.concurrency as usize)
                .overflow_strategy(slog_async::OverflowStrategy::Block)
                .build()
                .fuse()
        }
    };
    let log = slog::Logger::root(drain, o!());

    let commands = [KvsCommand::Get, KvsCommand::Put];
    let read_write_ratio_weights: [f32; 2] = match args.read_write_ratio {
        None => [1.0, 1.0], // won't be used
        Some(w) => [w, 1.0 - w],
    };
    let weighted_distr =
        WeightedIndex::new(&read_write_ratio_weights).expect("failed to create weighted index");

    let assigned_worker_url = match args.function {
        LoadGenFunction::Healthcheck => {
            format!("http://{}:52017/activation_router/healthcheck", hostname)
        }
        _ => format!("http://{}:52017/activation_router/schedule", hostname),
    };
    let worker_hosts = args.worker_hosts.unwrap().clone();
    let urls: Vec<String> = match args.function {
        LoadGenFunction::Healthcheck => worker_hosts
            .iter()
            .map(|h| format!("http://{}:52017/activation_router/healthcheck", h))
            .collect(),
        _ => worker_hosts
            .iter()
            .map(|h| format!("http://{}:52017/activation_router/schedule", h))
            .collect(),
    };

    let root_object_iptr = match args.root_object {
        // NOTE we'll simply ignore this
        None => IPtr::new(0, 0, 0),
        Some(oid) => IPtr::new(oid, 0, 0),
    };

    let mut owner_cache = HashMap::with_capacity(args.bucket_objects.len());
    let num_buckets_per_host = args.num_buckets_per_host.unwrap();
    for (idx, _) in args.bucket_objects.iter().enumerate() {
        let owner_idx: u16 = idx as u16 / num_buckets_per_host;
        owner_cache.insert(idx, owner_idx);
    }

    loop {
        // start signal
        if control_tuple.0.load(Ordering::Relaxed) == true {
            break;
        }
    }

    let start = Instant::now();
    let mut total_num_requests = 0;

    let input_zipf = {
        let exponent = match args.exponent {
            Some(e) => e as f64,
            // Doesn't matter what value we pick here, as we won't use the zipf generator.
            None => 1.0,
        };

        ZipfDistribution::new(args.num_input_objects.try_into().unwrap(), exponent).unwrap()
    };

    #[allow(unused_assignments)]
    let mut request_log_str = "";
    loop {
        // stop signal
        if control_tuple.1.load(Ordering::Relaxed) == true {
            break;
        }

        let request_builder = match args.function {
            LoadGenFunction::Healthcheck => {
                request_log_str = "healthcheck";
                client.get(&assigned_worker_url)
            }
            LoadGenFunction::MixedKvs | LoadGenFunction::MixedKvsMultihost => {
                let key = format!("key-{}", rng.gen_range(0..args.num_input_objects));
                // let bucket_iptr = compute_bucket_object_id(&args.bucket_objects, &key);
                // let bucket_object_id = bucket_iptr.get_object_id();
                /*
                let host_base_url = &urls[*owner_cache.get(
                    &args.bucket_objects.iter().position(|b| *b == bucket_object_id).unwrap()
                ).unwrap() as usize];
                */
                match commands[weighted_distr.sample(&mut rng)] {
                    KvsCommand::Put => {
                        request_log_str = "kvs_consumer::put_i32";

                        client
                            .post(&assigned_worker_url)
                            .json(&NandoActivationIntentSerializable {
                                name: "kvs_consumer::put_i32".to_string(),
                                host_idx: None,
                                args: vec![
                                    NandoArgumentSerializable::Ref(root_object_iptr),
                                    NandoArgumentSerializable::Value(key.into()),
                                    NandoArgumentSerializable::Value(
                                        rng.gen_range(0..1000000).into(),
                                    ),
                                ],
                                with_plan: None,
                            })
                    }
                    KvsCommand::Get => {
                        request_log_str = "kvs_consumer::get_i32";

                        client
                            .post(&assigned_worker_url)
                            .json(&NandoActivationIntentSerializable {
                                name: "kvs_consumer::get_i32".to_string(),
                                host_idx: None,
                                args: vec![
                                    NandoArgumentSerializable::Ref(root_object_iptr),
                                    NandoArgumentSerializable::Value(key.into()),
                                ],
                                with_plan: None,
                            })
                    }
                    _ => unreachable!(),
                }
            }
            LoadGenFunction::MixedKvsSkewed => {
                todo!("skewed workload for multihost kvs")
            }
            LoadGenFunction::ReadModifyWrite | LoadGenFunction::ReadModifyWriteMultihost => {
                let key = format!("key-{}", rng.gen_range(0..args.num_input_objects));
                let bucket_iptr = compute_bucket_object_id(&args.bucket_objects, &key);
                let bucket_object_id = bucket_iptr.get_object_id();
                let host_base_url = &urls[*owner_cache
                    .get(
                        &args
                            .bucket_objects
                            .iter()
                            .position(|b| *b == bucket_object_id)
                            .unwrap(),
                    )
                    .unwrap() as usize];

                match commands[weighted_distr.sample(&mut rng)] {
                    KvsCommand::Put => {
                        request_log_str = "kvs_consumer::set_or_increment_i32";

                        client
                            .post(host_base_url)
                            .json(&NandoActivationIntentSerializable {
                                name: request_log_str.to_string(),
                                host_idx: None,
                                args: vec![
                                    NandoArgumentSerializable::Ref(bucket_iptr),
                                    NandoArgumentSerializable::Value(key.into()),
                                ],
                                with_plan: None,
                            })
                    }
                    KvsCommand::Get => {
                        request_log_str = "kvs_consumer::get_i32_internal";

                        client
                            .post(host_base_url)
                            .json(&NandoActivationIntentSerializable {
                                name: "kvs_consumer::get_i32_internal".to_string(),
                                host_idx: None,
                                args: vec![
                                    NandoArgumentSerializable::Ref(bucket_iptr),
                                    NandoArgumentSerializable::Value(key.into()),
                                ],
                                with_plan: None,
                            })
                    }
                    _ => unreachable!(),
                }
            }
            LoadGenFunction::Noop => {
                request_log_str = "noop";

                client
                    .post(&assigned_worker_url)
                    .json(&NandoActivationIntentSerializable {
                        name: "noop".to_string(),
                        host_idx: None,
                        args: vec![],
                        with_plan: None,
                    })
            }
            _ => panic!("unrecognized function {}", args.function),
        };

        // println!("Sending Request: {:?}", &request_body.args);

        #[cfg(feature = "timing")]
        let request_start = Instant::now();

        let resp = match request_builder.timeout(Duration::from_secs(5)).send() {
            Ok(r) => r,
            Err(_) => {
                eprintln!("Request to worker schedule endpoint timed out, skipping.");
                continue;
            }
        };
        assert!(resp.status().is_success());
        let _resp_json = resp
            .json::<NandoActivationResolution>()
            .expect("failed to parse response as json");

        total_num_requests += 1;
        #[cfg(feature = "timing")]
        {
            let request_duration = request_start.elapsed();
            info!(
                log,
                "{} request latency (target {}): {}us",
                request_log_str,
                hostname,
                request_duration.as_micros()
            );
        }
    }

    let total_time = start.elapsed();

    let avg_throughput = (total_num_requests as f64) / total_time.as_secs_f64();

    println!(
        "Thread {} Total Time: {}ms",
        thread_id,
        total_time.as_secs_f64()
    );
    println!(
        "Thread {} Average throughput: {} txns/sec",
        thread_id, avg_throughput
    );

    info!(
        log,
        "Thread {} Total Time: {}ms",
        thread_id,
        total_time.as_secs_f64()
    );
    info!(
        log,
        "Thread {} Average throughput: {} txns/sec", thread_id, avg_throughput
    );

    (thread_id, total_num_requests)
}

fn redis_workload_loop(
    thread_id: u16,
    hostname: String,
    args: Args,
    client: redis::Client,
    control_tuple: (Arc<AtomicBool>, Arc<AtomicBool>),
) -> (u16, u64) {
    let drain = match args.output_file {
        Some(p) => {
            let pathbuf = p.clone();
            let path = pathbuf.as_path();
            let filename = path.file_name().unwrap();
            let path = path.with_file_name(format!("{}.{}", filename.to_str().unwrap(), thread_id));
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(true)
                .open(path)
                .unwrap();

            let decorator = slog_term::PlainDecorator::new(file);
            let drain = slog_term::FullFormat::new(decorator).build().fuse();
            slog_async::Async::new(drain)
                .chan_size(4 * 8192 as usize)
                .overflow_strategy(slog_async::OverflowStrategy::Block)
                .build()
                .fuse()
        }
        None => {
            let decorator = slog_term::PlainDecorator::new(std::io::stdout());
            let drain = slog_term::FullFormat::new(decorator).build().fuse();
            slog_async::Async::new(drain)
                .chan_size(4 * 8192 * args.concurrency as usize)
                .overflow_strategy(slog_async::OverflowStrategy::Block)
                .build()
                .fuse()
        }
    };
    let log = slog::Logger::root(drain, o!());

    let mut con = client.get_connection().unwrap();

    let commands = if args.function == LoadGenFunction::MixedKvsSkewedDistributedResetting
        || args.function == LoadGenFunction::MixedKvsSkewedBatchResetting
    {
        [KvsCommand::MGet, KvsCommand::Put]
    } else {
        [KvsCommand::Get, KvsCommand::Put]
    };

    let read_write_ratio_weights: [f32; 2] = match args.read_write_ratio {
        None => [1.0, 1.0], // won't be used
        Some(w) => [w, 1.0 - w],
    };
    let weighted_distr =
        WeightedIndex::new(&read_write_ratio_weights).expect("failed to create weighted index");
    let input_zipf = {
        let exponent = match args.exponent {
            Some(e) => e as f64,
            // Doesn't matter what value we pick here, as we won't use the zipf generator.
            None => 1.0,
        };

        ZipfDistribution::new(args.num_input_objects.try_into().unwrap(), exponent).unwrap()
    };

    let seed_iter: [u8; 8] = (args.seed + thread_id as u64).to_ne_bytes();
    let nested_seed = [seed_iter; 4];
    let seed: [u8; 32] = nested_seed.flatten().try_into().expect("oops");
    let mut rng: StdRng = SeedableRng::from_seed(seed);

    loop {
        // start signal
        if control_tuple.0.load(Ordering::Relaxed) == true {
            break;
        }
    }

    let mut total_num_requests = 0;
    let start = Instant::now();
    let mut start_instant = Instant::now();
    let mut num_resets = 0;
    loop {
        // stop signal
        if control_tuple.1.load(Ordering::Relaxed) == true {
            break;
        }

        rng = if args.function == LoadGenFunction::MixedKvsSkewedBatchResetting
            || args.function == LoadGenFunction::MixedKvsSkewedDistributedResetting
        {
            let elapsed = start_instant.elapsed().as_secs();

            if elapsed == args.interval_secs {
                info!(log, "Resetting rng after {}s interval", args.interval_secs);

                num_resets += 1;
                start_instant = Instant::now();
                let seed_iter: [u8; 8] =
                    (args.seed + (thread_id * (num_resets + 10)) as u64).to_ne_bytes();
                let nested_seed = [seed_iter; 4];
                let seed: [u8; 32] = nested_seed.flatten().try_into().expect("oops");
                SeedableRng::from_seed(seed)
            } else {
                rng
            }
        } else {
            rng
        };

        let (command, keys, value) = match args.function {
            LoadGenFunction::MixedKvs => {
                let key = format!("key-{}", rng.gen_range(0..args.num_input_objects));
                (
                    commands[weighted_distr.sample(&mut rng)],
                    vec![key],
                    rng.gen_range(0..1000000).to_string(),
                )
            }
            LoadGenFunction::MixedKvsSkewed => {
                let idx = input_zipf.sample(&mut rng);
                let key = format!("key-{}", idx);
                (
                    commands[weighted_distr.sample(&mut rng)],
                    vec![key],
                    rng.gen_range(0..1000000).to_string(),
                )
            }
            LoadGenFunction::MixedKvsSkewedBatchResetting
            | LoadGenFunction::MixedKvsSkewedDistributedResetting => {
                let command = commands[weighted_distr.sample(&mut rng)];
                match command {
                    KvsCommand::MGet => {
                        let mut keys = Vec::with_capacity(args.num_multi_get_keys as usize);
                        for _ in 0..args.num_multi_get_keys {
                            keys.push(format!("key-{}", input_zipf.sample(&mut rng)));
                        }
                        (command, keys, rng.gen_range(0..1000000).to_string())
                    }
                    KvsCommand::Put => (
                        command,
                        vec![format!("key-{}", input_zipf.sample(&mut rng))],
                        rng.gen_range(0..1000000).to_string(),
                    ),
                    _ => unreachable!(),
                }
            }
            _ => panic!("unrecognized function {}", args.function),
        };

        // println!("Sending Request: {:?}", &request_body.args);

        #[cfg(feature = "timing")]
        let request_start = Instant::now();
        let _: () = if args.sut == load_gen_api::Sut::RedisTransactions {
            redis::transaction(&mut con, &keys, |con, _pipe| {
                match command {
                    KvsCommand::Put => con
                        .set(keys[0].to_string(), value.to_string())
                        .expect("failed to set key"),
                    KvsCommand::Get => con
                        .get::<String, Option<String>>(keys[0].to_string())
                        .expect("failed to get key"),
                    KvsCommand::MGet => {
                        let _ = con
                            .mget::<&[String], Vec<String>>(&keys)
                            .expect("failed to mget keys");
                        None
                    }
                };

                Ok(Some(()))
            })
            .expect("failed to set key")
        } else {
            match command {
                KvsCommand::Put => con
                    .set(keys[0].to_string(), value.to_string())
                    .expect("failed to set key"),
                KvsCommand::Get => con
                    .get::<String, Option<String>>(keys[0].to_string())
                    .expect("failed to set key"),
                KvsCommand::MGet => {
                    let _ = con
                        .mget::<Vec<String>, Vec<String>>(keys)
                        .expect("failed to mget keys");
                    None
                }
            };
        };

        #[cfg(feature = "timing")]
        {
            let request_duration = request_start.elapsed();
            info!(
                log,
                "{} request latency: {}us",
                command,
                request_duration.as_micros()
            );
        }
        total_num_requests += 1;
    }

    let total_time = start.elapsed();
    let avg_throughput = (total_num_requests as f64) / total_time.as_secs_f64();

    println!(
        "Thread {} Total Time: {}ms",
        thread_id,
        total_time.as_secs_f64()
    );
    println!(
        "Thread {} Average throughput: {} txns/sec",
        thread_id, avg_throughput
    );

    info!(
        log,
        "Thread {} Total Time: {}ms",
        thread_id,
        total_time.as_secs_f64()
    );
    info!(
        log,
        "Thread {} Average throughput: {} txns/sec", thread_id, avg_throughput
    );

    (thread_id, total_num_requests)
}

fn redis_rmw_workload_loop(
    thread_id: u16,
    hostname: String,
    args: Args,
    client: redis::Client,
    control_tuple: (Arc<AtomicBool>, Arc<AtomicBool>),
) -> (u16, u64) {
    let seed_iter: [u8; 8] = (args.seed + thread_id as u64).to_ne_bytes();
    let nested_seed = [seed_iter; 4];
    let seed: [u8; 32] = nested_seed.flatten().try_into().expect("oops");
    let mut rng: StdRng = SeedableRng::from_seed(seed);

    let drain = match args.output_file {
        Some(p) => {
            let pathbuf = p.clone();
            let path = pathbuf.as_path();
            let filename = path.file_name().unwrap();
            let path = path.with_file_name(format!("{}.{}", filename.to_str().unwrap(), thread_id));
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(true)
                .open(path)
                .unwrap();

            let decorator = slog_term::PlainDecorator::new(file);
            let drain = slog_term::FullFormat::new(decorator).build().fuse();
            slog_async::Async::new(drain)
                .chan_size(4 * 8192 as usize)
                .overflow_strategy(slog_async::OverflowStrategy::Block)
                .build()
                .fuse()
        }
        None => {
            let decorator = slog_term::PlainDecorator::new(std::io::stdout());
            let drain = slog_term::FullFormat::new(decorator).build().fuse();
            slog_async::Async::new(drain)
                .chan_size(4 * 8192 * args.concurrency as usize)
                .overflow_strategy(slog_async::OverflowStrategy::Block)
                .build()
                .fuse()
        }
    };
    let log = slog::Logger::root(drain, o!());

    let mut con = client.get_connection().unwrap();

    let commands = [KvsCommand::Get, KvsCommand::Put];
    let read_write_ratio_weights: [f32; 2] = match args.read_write_ratio {
        None => [1.0, 1.0], // won't be used
        Some(w) => [w, 1.0 - w],
    };
    let weighted_distr =
        WeightedIndex::new(&read_write_ratio_weights).expect("failed to create weighted index");
    let input_zipf = {
        let exponent = match args.exponent {
            Some(e) => e as f64,
            // Doesn't matter what value we pick here, as we won't use the zipf generator.
            None => 1.0,
        };

        ZipfDistribution::new(args.num_input_objects.try_into().unwrap(), exponent).unwrap()
    };

    loop {
        // start signal
        if control_tuple.0.load(Ordering::Relaxed) == true {
            break;
        }
    }

    let mut total_num_requests = 0;
    let start = Instant::now();
    loop {
        // stop signal
        if control_tuple.1.load(Ordering::Relaxed) == true {
            break;
        }

        let (command, key, value) = match args.function {
            LoadGenFunction::ReadModifyWrite => {
                let key = format!("key-{}", rng.gen_range(0..args.num_input_objects));
                (
                    commands[weighted_distr.sample(&mut rng)],
                    key,
                    rng.gen_range(0..1000000).to_string(),
                )
            }
            _ => panic!("unrecognized function {}", args.function),
        };

        // println!("Sending Request: {:?}", &request_body.args);

        #[cfg(feature = "timing")]
        let request_start = Instant::now();
        let _: () = if args.sut == load_gen_api::Sut::RedisTransactions {
            redis::transaction(&mut con, &[key.clone()], |con, pipe| {
                match command {
                    KvsCommand::Put => match con.get::<String, u64>(key.to_string()) {
                        Err(_) => {
                            pipe.set(key.to_string(), 1)
                                .ignore()
                                .query::<()>(con)
                                .expect("failed to initialize key");
                        }
                        Ok(v) => {
                            if v < 100000 {
                                pipe.set(key.to_string(), 2 * v)
                                    .ignore()
                                    .query::<()>(con)
                                    .expect("failed to update key");
                            } else {
                                pipe.set(key.to_string(), v + 1)
                                    .ignore()
                                    .query::<()>(con)
                                    .expect("failed to update key");
                            }
                        }
                    },
                    KvsCommand::Get => {
                        con.get::<String, Option<u64>>(key.to_string())
                            .expect("failed to get key");
                    }
                    _ => unreachable!(),
                };

                Ok(Some(()))
            })
            .expect("failed to set key")
        } else {
            match command {
                KvsCommand::Put => match con.get::<String, u64>(key.clone()) {
                    Err(_) => {
                        let _ = con.set::<String, u64, Option<u64>>(key, 1);
                    }
                    Ok(v) => {
                        if v < 100000 {
                            con.set::<String, u64, ()>(key, (2 * v))
                                .expect("failed to update key");
                        } else {
                            con.set::<String, u64, ()>(key, (v + 1))
                                .expect("failed to update key");
                        }
                    }
                },
                KvsCommand::Get => {
                    con.get::<String, Option<u64>>(key.to_string())
                        .expect("failed to get key");
                }
                _ => unreachable!(),
            }
        };

        #[cfg(feature = "timing")]
        {
            let request_duration = request_start.elapsed();
            info!(log, "request latency: {}us", request_duration.as_micros());
        }
        total_num_requests += 1;
    }

    let total_time = start.elapsed();
    let avg_throughput = (total_num_requests as f64) / total_time.as_secs_f64();

    println!(
        "Thread {} Total Time: {}ms",
        thread_id,
        total_time.as_secs_f64()
    );
    println!(
        "Thread {} Average throughput: {} txns/sec",
        thread_id, avg_throughput
    );

    info!(
        log,
        "Thread {} Total Time: {}ms",
        thread_id,
        total_time.as_secs_f64()
    );
    info!(
        log,
        "Thread {} Average throughput: {} txns/sec", thread_id, avg_throughput
    );

    (thread_id, total_num_requests)
}

fn memcached_workload_loop(
    thread_id: u16,
    hostname: String,
    args: Args,
    client: memcache::Client,
    control_tuple: (Arc<AtomicBool>, Arc<AtomicBool>),
) -> (u16, u64) {
    let drain = match args.output_file {
        Some(p) => {
            let pathbuf = p.clone();
            let path = pathbuf.as_path();
            let filename = path.file_name().unwrap();
            let path = path.with_file_name(format!("{}.{}", filename.to_str().unwrap(), thread_id));
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(true)
                .open(path)
                .unwrap();

            let decorator = slog_term::PlainDecorator::new(file);
            let drain = slog_term::FullFormat::new(decorator).build().fuse();
            slog_async::Async::new(drain)
                .chan_size(4 * 8192 as usize)
                .overflow_strategy(slog_async::OverflowStrategy::Block)
                .build()
                .fuse()
        }
        None => {
            let decorator = slog_term::PlainDecorator::new(std::io::stdout());
            let drain = slog_term::FullFormat::new(decorator).build().fuse();
            slog_async::Async::new(drain)
                .chan_size(4 * 8192 * args.concurrency as usize)
                .overflow_strategy(slog_async::OverflowStrategy::Block)
                .build()
                .fuse()
        }
    };
    let log = slog::Logger::root(drain, o!());

    let commands = if args.function == LoadGenFunction::MixedKvsSkewedDistributedResetting
        || args.function == LoadGenFunction::MixedKvsSkewedBatchResetting
    {
        [KvsCommand::MGet, KvsCommand::Put]
    } else {
        [KvsCommand::Get, KvsCommand::Put]
    };
    // let mut kv_client = client.kv_client();
    let read_write_ratio_weights: [f32; 2] = match args.read_write_ratio {
        None => [1.0, 1.0], // won't be used
        Some(w) => [w, 1.0 - w],
    };
    let weighted_distr =
        WeightedIndex::new(&read_write_ratio_weights).expect("failed to create weighted index");
    let input_zipf = {
        let exponent = match args.exponent {
            Some(e) => e as f64,
            // Doesn't matter what value we pick here, as we won't use the zipf generator.
            None => 1.0,
        };

        ZipfDistribution::new(args.num_input_objects.try_into().unwrap(), exponent).unwrap()
    };

    let seed_iter: [u8; 8] = (args.seed + thread_id as u64).to_ne_bytes();
    let nested_seed = [seed_iter; 4];
    let seed: [u8; 32] = nested_seed.flatten().try_into().expect("oops");
    let mut rng: StdRng = SeedableRng::from_seed(seed);

    loop {
        // start signal
        if control_tuple.0.load(Ordering::Relaxed) == true {
            break;
        }
    }

    let mut total_num_requests = 0;
    let start = Instant::now();
    let mut start_instant = Instant::now();
    let mut num_resets = 0;
    loop {
        // stop signal
        if control_tuple.1.load(Ordering::Relaxed) == true {
            break;
        }

        rng = if args.function == LoadGenFunction::MixedKvsSkewedBatchResetting
            || args.function == LoadGenFunction::MixedKvsSkewedDistributedResetting
        {
            let elapsed = start_instant.elapsed().as_secs();

            if elapsed == args.interval_secs {
                info!(log, "Resetting rng after {}s interval", args.interval_secs);

                num_resets += 1;
                start_instant = Instant::now();
                let seed_iter: [u8; 8] =
                    (args.seed + (thread_id * (num_resets + 10)) as u64).to_ne_bytes();
                let nested_seed = [seed_iter; 4];
                let seed: [u8; 32] = nested_seed.flatten().try_into().expect("oops");
                SeedableRng::from_seed(seed)
            } else {
                rng
            }
        } else {
            rng
        };

        let (command, keys, value) = match args.function {
            LoadGenFunction::MixedKvs => {
                let key = format!("key-{}", rng.gen_range(0..args.num_input_objects));
                (
                    commands[weighted_distr.sample(&mut rng)],
                    vec![key],
                    rng.gen_range(0..1000000),
                )
            }
            LoadGenFunction::MixedKvsSkewed => {
                let idx = input_zipf.sample(&mut rng);
                let key = format!("key-{}", idx);
                (
                    commands[weighted_distr.sample(&mut rng)],
                    vec![key],
                    rng.gen_range(0..1000000),
                )
            }
            LoadGenFunction::MixedKvsSkewedBatchResetting
            | LoadGenFunction::MixedKvsSkewedDistributedResetting => {
                let command = commands[weighted_distr.sample(&mut rng)];
                match command {
                    KvsCommand::MGet => {
                        let mut keys = Vec::with_capacity(args.num_multi_get_keys as usize);
                        for _ in 0..args.num_multi_get_keys {
                            keys.push(format!("key-{}", input_zipf.sample(&mut rng)));
                        }
                        (command, keys, rng.gen_range(0..1000000))
                    }
                    KvsCommand::Put => (
                        command,
                        vec![format!("key-{}", input_zipf.sample(&mut rng))],
                        rng.gen_range(0..1000000),
                    ),
                    _ => unreachable!(),
                }
            }
            _ => panic!("unrecognized function {}", args.function),
        };

        #[cfg(feature = "timing")]
        let request_start = Instant::now();
        match command {
            KvsCommand::Put => {
                client
                    .set::<i32>(keys.get(0).unwrap(), value, 60)
                    .expect("set failed");
            }
            KvsCommand::Get => {
                client.get::<i32>(keys.get(0).unwrap()).expect("get failed");
            }
            KvsCommand::MGet => {
                let keys: Vec<&str> = keys.iter().map(|k| k.as_ref()).collect();
                client.gets::<i32>(&keys).expect("gets failed");
            }
            _ => todo!(),
        }
        #[cfg(feature = "timing")]
        {
            let request_duration = request_start.elapsed();
            info!(
                log,
                "{} request latency: {}us",
                command,
                request_duration.as_micros()
            );
        }
        total_num_requests += 1;
    }

    let total_time = start.elapsed();

    let avg_throughput = (total_num_requests as f64) / total_time.as_secs_f64();

    println!(
        "Thread {} Total Time: {}ms",
        thread_id,
        total_time.as_secs_f64()
    );
    println!(
        "Thread {} Average throughput: {} txns/sec",
        thread_id, avg_throughput
    );

    info!(
        log,
        "Thread {} Total Time: {}ms",
        thread_id,
        total_time.as_secs_f64()
    );
    info!(
        log,
        "Thread {} Average throughput: {} txns/sec", thread_id, avg_throughput
    );

    (thread_id, total_num_requests)
}

fn memcached_rmw_workload_loop(
    thread_id: u16,
    hostname: String,
    args: Args,
    client: memcache::Client,
    control_tuple: (Arc<AtomicBool>, Arc<AtomicBool>),
) -> (u16, u64) {
    let seed_iter: [u8; 8] = (args.seed + thread_id as u64).to_ne_bytes();
    let nested_seed = [seed_iter; 4];
    let seed: [u8; 32] = nested_seed.flatten().try_into().expect("oops");
    let mut rng: StdRng = SeedableRng::from_seed(seed);

    let drain = match args.output_file {
        Some(p) => {
            let pathbuf = p.clone();
            let path = pathbuf.as_path();
            let filename = path.file_name().unwrap();
            let path = path.with_file_name(format!("{}.{}", filename.to_str().unwrap(), thread_id));
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(true)
                .open(path)
                .unwrap();

            let decorator = slog_term::PlainDecorator::new(file);
            let drain = slog_term::FullFormat::new(decorator).build().fuse();
            slog_async::Async::new(drain)
                .chan_size(4 * 8192 as usize)
                .overflow_strategy(slog_async::OverflowStrategy::Block)
                .build()
                .fuse()
        }
        None => {
            let decorator = slog_term::PlainDecorator::new(std::io::stdout());
            let drain = slog_term::FullFormat::new(decorator).build().fuse();
            slog_async::Async::new(drain)
                .chan_size(4 * 8192 * args.concurrency as usize)
                .overflow_strategy(slog_async::OverflowStrategy::Block)
                .build()
                .fuse()
        }
    };
    let log = slog::Logger::root(drain, o!());

    let commands = [KvsCommand::Get, KvsCommand::Put];
    let read_write_ratio_weights: [f32; 2] = match args.read_write_ratio {
        None => [1.0, 1.0], // won't be used
        Some(w) => [w, 1.0 - w],
    };
    let weighted_distr =
        WeightedIndex::new(&read_write_ratio_weights).expect("failed to create weighted index");
    let input_zipf = {
        let exponent = match args.exponent {
            Some(e) => e as f64,
            // Doesn't matter what value we pick here, as we won't use the zipf generator.
            None => 1.0,
        };

        ZipfDistribution::new(args.num_input_objects.try_into().unwrap(), exponent).unwrap()
    };

    loop {
        // start signal
        if control_tuple.0.load(Ordering::Relaxed) == true {
            break;
        }
    }

    let mut total_num_requests = 0;
    let start = Instant::now();
    loop {
        // stop signal
        if control_tuple.1.load(Ordering::Relaxed) == true {
            break;
        }

        let (command, key, value) = match args.function {
            LoadGenFunction::ReadModifyWrite => {
                let key = format!("key-{}", rng.gen_range(0..args.num_input_objects));
                (
                    commands[weighted_distr.sample(&mut rng)],
                    key,
                    rng.gen_range(0..1000000),
                )
            }
            _ => panic!("unrecognized function {}", args.function),
        };

        // println!("Sending Request: {:?}", &request_body.args);

        #[cfg(feature = "timing")]
        let request_start = Instant::now();
        match command {
            KvsCommand::Put => match client.get::<i32>(&key).expect("failed to get key") {
                None => {
                    client
                        .set::<i32>(&key, value, 60)
                        .expect("failed to set key");
                }
                Some(v) => {
                    if v < 100000 {
                        client
                            .set::<i32>(&key, (2 * v), 60)
                            .expect("failed to update key");
                    } else {
                        client
                            .set::<i32>(&key, (v + 1), 60)
                            .expect("failed to update key");
                    }
                }
            },
            KvsCommand::Get => {
                client.get::<i32>(&key).expect("failed to get key");
            }
            _ => unreachable!(),
        }
        #[cfg(feature = "timing")]
        {
            let request_duration = request_start.elapsed();
            info!(log, "request latency: {}us", request_duration.as_micros());
        }
        total_num_requests += 1;
    }

    let total_time = start.elapsed();
    let avg_throughput = (total_num_requests as f64) / total_time.as_secs_f64();

    println!(
        "Thread {} Total Time: {}ms",
        thread_id,
        total_time.as_secs_f64()
    );
    println!(
        "Thread {} Average throughput: {} txns/sec",
        thread_id, avg_throughput
    );

    info!(
        log,
        "Thread {} Total Time: {}ms",
        thread_id,
        total_time.as_secs_f64()
    );
    info!(
        log,
        "Thread {} Average throughput: {} txns/sec", thread_id, avg_throughput
    );

    (thread_id, total_num_requests)
}

fn workload_loop(
    thread_id: u16,
    hostname: String,
    args: Args,
    client: Client,
    control_tuple: (Arc<AtomicBool>, Arc<AtomicBool>),
    multihost: bool,
) -> (u16, u64) {
    if args.function == LoadGenFunction::GraphTraversal {
        return graph_traversal(thread_id, hostname, args, client, control_tuple);
    } else if args.function == LoadGenFunction::SmithWaterman {
        return smith_waterman(thread_id, hostname, args, client, control_tuple);
    } else if args.function == LoadGenFunction::Sorting {
        return sorting(thread_id, hostname, args, client, control_tuple);
    }

    match args.sut {
        load_gen_api::Sut::Magpie => match args.function == LoadGenFunction::MixedKvsMultihost
            || args.function == LoadGenFunction::ReadModifyWriteMultihost
        {
            true => magpie_workload_loop(
                thread_id,
                hostname,
                args,
                client.get_magpie_client(),
                control_tuple,
            ),
            false => match multihost {
                false => magpie_workload_loop(
                    thread_id,
                    hostname,
                    args,
                    client.get_magpie_client(),
                    control_tuple,
                ),
                true => magpie_multihost_workload_loop(
                    thread_id,
                    hostname,
                    args,
                    client.get_magpie_client(),
                    control_tuple,
                ),
            },
        },
        load_gen_api::Sut::Redis | load_gen_api::Sut::RedisTransactions => {
            match args.function == LoadGenFunction::ReadModifyWrite {
                false => redis_workload_loop(
                    thread_id,
                    hostname,
                    args,
                    client.get_redis_client(),
                    control_tuple,
                ),
                true => redis_rmw_workload_loop(
                    thread_id,
                    hostname,
                    args,
                    client.get_redis_client(),
                    control_tuple,
                ),
            }
        }
        load_gen_api::Sut::Memcached => match args.function == LoadGenFunction::ReadModifyWrite {
            true => memcached_rmw_workload_loop(
                thread_id,
                hostname,
                args,
                client.get_memcached_client(),
                control_tuple,
            ),
            false => memcached_workload_loop(
                thread_id,
                hostname,
                args,
                client.get_memcached_client(),
                control_tuple,
            ),
        },
    }
}

fn dump_workload_to_file(thread_id: u16, args: &Args) {
    let seed_iter: [u8; 8] = (args.seed + thread_id as u64).to_ne_bytes();
    let nested_seed = [seed_iter; 4];
    let seed: [u8; 32] = nested_seed.flatten().try_into().expect("oops");
    let mut rng: StdRng = SeedableRng::from_seed(seed);
    let commands = [KvsCommand::Get, KvsCommand::Put];
    let read_write_ratio_weights: [f32; 2] = match args.read_write_ratio {
        None => [1.0, 1.0], // won't be used
        Some(w) => [w, 1.0 - w],
    };
    let weighted_distr =
        WeightedIndex::new(&read_write_ratio_weights).expect("failed to create weighted index");

    let input_zipf = {
        let exponent = match args.exponent {
            Some(e) => e as f64,
            // Doesn't matter what value we pick here, as we won't use the zipf generator.
            None => 1.0,
        };

        ZipfDistribution::new(args.num_input_objects.try_into().unwrap(), exponent).unwrap()
    };

    let drain = match args.output_file {
        Some(ref p) => {
            let pathbuf = p.clone();
            let path = pathbuf.as_path();
            let filename = path.file_name().unwrap();
            let path = path.with_file_name(format!("{}.{}", filename.to_str().unwrap(), thread_id));
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(true)
                .open(path)
                .unwrap();

            let decorator = slog_term::PlainDecorator::new(file);
            let drain = slog_term::CompactFormat::new(decorator)
                .use_custom_timestamp(|_t| Ok(()))
                .build()
                .fuse();
            slog_async::Async::new(drain)
                .chan_size(4 * 8192 as usize)
                .overflow_strategy(slog_async::OverflowStrategy::Block)
                .build()
                .fuse()
        }
        None => {
            let decorator = slog_term::PlainDecorator::new(std::io::stdout());
            let drain = slog_term::FullFormat::new(decorator).build().fuse();
            slog_async::Async::new(drain)
                .chan_size(4 * 8192 * args.concurrency as usize)
                .overflow_strategy(slog_async::OverflowStrategy::Block)
                .build()
                .fuse()
        }
    };
    let log = slog::Logger::root(drain, o!());

    let iteration_print_step: u64 = (0.1 * args.num_iterations as f64).round() as u64;
    for i in 0..args.num_iterations {
        if i % iteration_print_step == 0 {
            println!("At iteration {} of {}", i, args.num_iterations);
        }

        match args.function {
            LoadGenFunction::MixedKvs => {
                let key: usize = rng.gen_range(0..args.num_input_objects) as usize;
                info!(log, "{},", key);
                // let val: usize = rng.gen_range(0..args.num_input_objects) as usize;
                /*
                match commands[weighted_distr.sample(&mut rng)] {
                KvsCommand::Put => {
                request_log_str = "kvs_consumer::put_i32";

                client.post(&url).json(&NandoActivationIntentSerializable {
                name: "kvs_consumer::put_i32".to_string(),
                host_idx: None,
                args: vec![
                NandoArgumentSerializable::Ref(root_object_iptr),
                NandoArgumentSerializable::Value(key.into()),
                NandoArgumentSerializable::Value(rng.gen_range(0..1000000).into()),
                ],
                })
                }
                KvsCommand::Get => {
                request_log_str = "kvs_consumer::get_i32";

                client.post(&url).json(&NandoActivationIntentSerializable {
                name: "kvs_consumer::get_i32".to_string(),
                host_idx: None,
                args: vec![
                NandoArgumentSerializable::Ref(root_object_iptr),
                NandoArgumentSerializable::Value(key.into()),
                ],
                })
                }
                }
                */
            }
            LoadGenFunction::MixedKvsSkewed => {
                let key: usize = input_zipf.sample(&mut rng);
                info!(log, "{},", key);
            }
            _ => panic!("unsupported workload to dump to file: {}", args.function),
        }
    }
}

fn main() {
    let args = parse_args();
    let config = Config::init_from_file(args.config_file_path.clone().unwrap());

    let experiment_runner = ExperimentRunner::get_experiment_runner_mut();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();

    if args.dump_to_file {
        let mut handles = Vec::with_capacity(args.concurrency as usize);
        for thread_id in 1..=args.concurrency {
            let args = args.clone();
            handles.push(std::thread::spawn(move || {
                dump_workload_to_file(thread_id - 1, &args);
            }));
        }

        for handle in handles {
            handle.join().expect("thread failed");
        }
        return;
    }

    if args.local_mode {
        experiment_runner.init_experiment(args, &config.hosts, rt.handle());
        experiment_runner.start_experiment(false);
        experiment_runner.collect_results();
        return;
    }

    let _ = rt.block_on(async move {
        HttpServer::new(|| {
            App::new()
                .service(handlers::init_experiment_handler)
                .service(handlers::start_experiment_at_deadline_handler)
                .service(handlers::get_experiment_finished_handler)
        })
        .bind(("0.0.0.0", config.port))?
        .workers(2)
        .run()
        .await
    });
}
