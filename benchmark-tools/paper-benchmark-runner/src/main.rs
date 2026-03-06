use std::path::PathBuf;
use std::process;
use std::time::Duration;

use chrono;
use clap::Parser;
use load_gen_api::{ExperimentSetup, Function, Sut};
use nando_support::{
    activation_intent::{NandoActivationIntentSerializable, NandoArgumentSerializable},
    iptr::IPtr,
};
use ownership_support::ConsolidationIntent;

use config::Config;

mod config;

#[derive(Parser, Debug, Clone)]
pub struct Args {
    #[arg(long)]
    pub config_file_path: Option<PathBuf>,

    #[arg(short('t'), value_enum)]
    pub sut: Option<Sut>,

    #[arg(short('f'), value_enum, default_value = "noop")]
    pub function: Function,

    #[arg(short('o'), long, default_value = "false")]
    pub offline: bool,

    #[arg(short('r'), long)]
    pub root_object: Option<u128>,

    #[arg(short('n'), long)]
    pub num_kvs_buckets: Option<u16>,

    #[arg(short('p'), long)]
    pub num_buckets_per_host: Option<u16>,

    #[arg(long)]
    pub graph_num_partitions: Option<u16>,

    #[arg(long, default_value = "2147483648")]
    pub mv_size_threshold: usize,

    #[arg(long)]
    pub use_hash_placement: bool,

    #[arg(long)]
    pub prompt_before_reset: bool,

    #[arg(long)]
    pub num_graph_chunks: Option<u16>,

    #[arg(long)]
    pub sort_num_elements_per_partition: Option<u64>,

    #[arg(long)]
    pub sort_num_input_partitions: Option<usize>,

    #[arg(long)]
    pub sort_chunk_size: Option<u16>,

    #[arg(long, default_value = "false")]
    pub weak: bool,

    #[arg(long, default_value = "false")]
    pub use_fold: bool,
}

fn reset_cluster(
    client: &reqwest::blocking::Client,
    config: &config::Config,
    args: &Args,
    num_worker_threads_per_host: usize,
    sut: Sut,
) {
    println!("About to reset cluster");

    match sut {
        Sut::Magpie => {
            if !args.offline {
                let figaro_url = format!(
                    "http://{}:{}/reset_state",
                    config.figaro_host, config.figaro_host_port
                );

                println!("about to reset figaro state");

                // reset figaro ownership state
                let _ = client
                    .put(&figaro_url)
                    .timeout(Duration::from_secs(10))
                    .send()
                    .expect("failed to reset figaro state");

                println!("I suppose this worked?");
            }

            let root_allocation_dir = match config.root_allocation_dir.is_empty() {
                true => "/tmp/magpie".to_string(),
                false => config.root_allocation_dir.clone(),
            };

            let application_string = match args.function {
                Function::SmithWaterman => "smith-waterman".to_string(),
                Function::PageRank | Function::TriangleCount => "nano4r".to_string(),
                Function::GraphTraversal | Function::Sorting => todo!(),
                _ => "kvs-consumer".to_string(),
            };

            let config_file = format!("{}-{application_string}.toml", config.magpie_config);

            // restart magpie instances
            for worker_host in &config.worker_hosts {
                let mut args = vec![
                    format!("{}@{}", config.ssh_user, worker_host),
                    format!(
                        "bash -lc \"MAGPIE_WORKER_THREADS={} MAGPIE_ROOT={} MAGPIE_APPLICATION={} MAGPIE_CONFIG={} MV_SIZE_THRESHOLD={} ROOT_ALLOCATION_DIR={} {}/../utils/restart-local-instance.sh\"",
                        num_worker_threads_per_host,
                        config.magpie_root,
                        application_string,
                        config_file,
                        args.mv_size_threshold,
                        root_allocation_dir,
                        config.magpie_root,
                    ),
                ];

                if !config.worker_key_file_path.is_empty() {
                    args.insert(1, format!("-i{}", config.worker_key_file_path));
                }

                let mut script_proc = process::Command::new("ssh")
                    .args(&args)
                    .stdout(process::Stdio::null())
                    .spawn()
                    .expect(&format!("failed to restart worker '{}'", worker_host));
                script_proc
                    .wait()
                    .expect("failed while waiting for worker restart script to finish");
            }

            // NOTE this will not be enough for large object sets.
            std::thread::sleep(Duration::from_secs(3));

            // fetch mappings for active workers
            if !args.offline {
                for worker_host in &config.worker_hosts {
                    loop {
                        let host_url = format!(
                            "http://{}:52017/activation_router/fetch_host_mapping",
                            worker_host
                        );
                        let resp = client.get(&host_url).timeout(Duration::from_secs(5)).send();
                        if let Ok(resp) = resp {
                            if resp.status().is_success() {
                                break;
                            }
                        }

                        println!("Error while trying to fetch host mapping for {worker_host}, will retry in 5s");
                        std::thread::sleep(Duration::from_secs(5));
                    }
                }
            }

            match args.function {
                Function::MixedKvs
                | Function::MixedKvsSkewed
                | Function::MixedKvsMultihost
                | Function::MixedKvsNando => {
                    // first host is the kvs owner
                    let host_url = format!(
                        "http://{}:52017/activation_router/schedule",
                        config.worker_hosts.get(0).unwrap()
                    );
                    let num_kvs_buckets = match args.num_kvs_buckets {
                        Some(n) => n,
                        None => 16,
                    };
                    let resp = client
                        .post(&host_url)
                        .json(&NandoActivationIntentSerializable {
                            name: "kvs_consumer::init_kvs_consumer".to_string(),
                            host_idx: None,
                            args: vec![
                                NandoArgumentSerializable::Value(num_kvs_buckets.into()),
                                NandoArgumentSerializable::Value(65536u64.into()),
                            ],
                            with_plan: None,
                        })
                        .timeout(Duration::from_secs(5))
                        .send();
                    assert!(resp.is_ok());
                }
                Function::ReadModifyWrite
                | Function::ReadModifyWriteNando
                | Function::ReadModifyWriteMultihost => {
                    // first host is the kvs owner
                    let host_url = format!(
                        "http://{}:52017/activation_router/schedule",
                        config.worker_hosts.get(0).unwrap()
                    );
                    let num_kvs_buckets = match args.num_kvs_buckets {
                        Some(n) => n,
                        None => 16,
                    };
                    let name = if args.offline {
                        "kvs_consumer::init_kvs_consumer_u64".to_string()
                    } else {
                        "kvs_consumer::init_kvs_consumer".to_string()
                    };
                    let resp = client
                        .post(&host_url)
                        .json(&NandoActivationIntentSerializable {
                            name,
                            host_idx: None,
                            args: vec![
                                NandoArgumentSerializable::Value(num_kvs_buckets.into()),
                                NandoArgumentSerializable::Value(65536u64.into()),
                            ],
                            with_plan: None,
                        })
                        .timeout(Duration::from_secs(5))
                        .send();
                    assert!(resp.is_ok());
                }
                Function::PageRank => {
                    let host_url = format!(
                        "http://{}:52017/activation_router/schedule",
                        config.worker_hosts.get(0).unwrap()
                    );
                    let num_partitions = match args.graph_num_partitions {
                        Some(p) => p,
                        None => 2,
                    };
                    let resp = client
                        .post(&host_url)
                        .json(&NandoActivationIntentSerializable {
                            name: "nano4r::parse_huge_graph".to_string(),
                            host_idx: None,
                            args: vec![
                                NandoArgumentSerializable::Value(config.graph_input.clone().into()),
                                NandoArgumentSerializable::Value(num_partitions.into()),
                                NandoArgumentSerializable::Value(false.into()),
                            ],
                            with_plan: None,
                        })
                        .send()
                        .expect("Failed to parse graph");
                }
                Function::TriangleCount => {
                    let host_url = format!(
                        "http://{}:52017/activation_router/schedule",
                        config.worker_hosts.get(0).unwrap()
                    );
                    let num_partitions = match args.graph_num_partitions {
                        Some(p) => p,
                        None => 2,
                    };
                    let resp = client
                        .post(&host_url)
                        .json(&NandoActivationIntentSerializable {
                            name: "nano4r::parse_huge_graph".to_string(),
                            host_idx: None,
                            args: vec![
                                NandoArgumentSerializable::Value(config.graph_input.clone().into()),
                                NandoArgumentSerializable::Value(num_partitions.into()),
                                NandoArgumentSerializable::Value(true.into()),
                            ],
                            with_plan: None,
                        })
                        .send()
                        .expect("Failed to parse graph");
                }
                Function::Sorting => {
                    let host_url = format!(
                        "http://{}:52017/activation_router/schedule",
                        config.worker_hosts.get(0).unwrap()
                    );
                    let num_partitions = match args.sort_num_input_partitions {
                        Some(p) => p,
                        None => 16,
                    };
                    let num_elements_per_partition = match args.sort_num_elements_per_partition {
                        Some(p) => p,
                        None => 1000000,
                    };
                    let resp = client
                        .post(&host_url)
                        .json(&NandoActivationIntentSerializable {
                            name: "sorting_consumer::allocate_and_init_u64_collection".to_string(),
                            host_idx: None,
                            args: vec![
                                NandoArgumentSerializable::Value(num_partitions.into()),
                                NandoArgumentSerializable::Value(num_elements_per_partition.into()),
                                NandoArgumentSerializable::Value(51017.into()),
                            ],
                            with_plan: None,
                        })
                        .send()
                        .expect("Failed to generate sort input");
                }
                _ => {}
            }

            if !args.offline {
                if args.function == Function::Sorting {
                    let chunk_size = match args.sort_chunk_size {
                        None => 8,
                        Some(s) => s,
                    };

                    // first host is the collection object owner
                    let host_url = format!(
                        "http://{}:52017/activation_router/schedule",
                        config.worker_hosts.get(0).unwrap()
                    );
                    let collection_root: u128 =
                        args.root_object.expect("no collection root object id");
                    let resp = client
                        .post(&host_url)
                        .json(&NandoActivationIntentSerializable {
                            name: "sorting_consumer::visit_chunks_u64".to_string(),
                            host_idx: None,
                            args: vec![
                                NandoArgumentSerializable::Ref(
                                    IPtr::new(collection_root, 0, 0).into(),
                                ),
                                NandoArgumentSerializable::Value(chunk_size.into()),
                            ],
                            with_plan: Some("sort-collection-repartition".to_string()),
                        })
                        .send();
                    assert!(resp.is_ok());

                    for worker_host in &config.worker_hosts {
                        let host_url =
                            format!("http://{}:52017/activation_router/schedule", worker_host);

                        let resp = client
                            .post(&host_url)
                            .json(&NandoActivationIntentSerializable {
                                name: "reset_scheduler_state".to_string(),
                                host_idx: None,
                                args: Vec::default(),
                                with_plan: None,
                            })
                            .send();
                        assert!(resp.is_ok());
                    }
                } else if args.function != Function::PageRank
                    && args.function != Function::TriangleCount
                {
                    if config.worker_hosts.len() > 1 && args.num_buckets_per_host.is_some() {
                        std::thread::sleep(Duration::from_secs(
                            config.bucket_objects.len() as u64 / 8,
                        ));
                        println!("Will move bucket objects around");
                        let num_buckets_per_host = args.num_buckets_per_host.unwrap() as usize;
                        let figaro_url = format!(
                            "http://{}:{}/consolidate",
                            config.figaro_host, config.figaro_host_port,
                        );

                        for host_idx in 1..config.worker_hosts.len() {
                            let args: Vec<_> =
                                config.bucket_objects[host_idx * num_buckets_per_host
                                    ..(host_idx + 1) * num_buckets_per_host]
                                    .iter()
                                    .map(|b| b.parse::<u128>().unwrap())
                                    .collect();
                            println!("about to submit consolidate to {figaro_url}");
                            let resp = client
                                .post(&figaro_url)
                                .json(&ConsolidationIntent {
                                    to_host: host_idx as u64,
                                    args,
                                    versions: vec![],
                                })
                                .timeout(Duration::from_secs(30))
                                .send()
                                .expect("failed to consolidate");
                        }
                        std::thread::sleep(Duration::from_secs(3));
                    }

                    // NOTE this means that we're about to run the epic version of the kvs workload
                    if config.worker_hosts.len() > 1
                        && (args.function == Function::MixedKvsMultihost
                            || args.function == Function::ReadModifyWriteMultihost)
                    {
                        println!("Will cache kvs root across hosts");
                        let figaro_cache_url = format!(
                            "http://{}:{}/cache_single_object",
                            config.figaro_host, config.figaro_host_port,
                        );
                        let resp = client
                            .put(&figaro_cache_url)
                            .timeout(Duration::from_secs(600))
                            .json(&ConsolidationIntent {
                                // NOTE this is ignored.
                                to_host: 0u64,
                                args: vec![args.root_object.expect("need root object")],
                                versions: vec![],
                            })
                            .send()
                            .expect("failed to cache owned graph objects");
                        std::thread::sleep(Duration::from_secs(3));
                    }
                } else {
                    // first host is the kvs owner
                    let host_url = format!(
                        "http://{}:52017/activation_router/schedule",
                        config.worker_hosts.get(0).unwrap()
                    );
                    let graph_root: u128 = config
                        .graph_root_object
                        .parse()
                        .expect("no graph root object id");
                    let num_graph_chunks = args.num_graph_chunks.unwrap();
                    assert!(num_graph_chunks > 0);

                    let resp = client
                        .post(&host_url)
                        .json(&NandoActivationIntentSerializable {
                            name: "nano4r::chunk_partitions".to_string(),
                            host_idx: None,
                            args: vec![
                                NandoArgumentSerializable::Ref(IPtr::new(graph_root, 0, 0).into()),
                                NandoArgumentSerializable::Value(num_graph_chunks.into()),
                                NandoArgumentSerializable::Value(false.into()),
                            ],
                            with_plan: Some("graph-repartition".to_string()),
                        })
                        .send();
                    assert!(resp.is_ok());

                    for worker_host in &config.worker_hosts {
                        let host_url =
                            format!("http://{}:52017/activation_router/schedule", worker_host);

                        let resp = client
                            .post(&host_url)
                            .json(&NandoActivationIntentSerializable {
                                name: "reset_scheduler_state".to_string(),
                                host_idx: None,
                                args: Vec::default(),
                                with_plan: None,
                            })
                            .send();
                        assert!(resp.is_ok());
                    }
                }
            }
        }
        Sut::Redis | Sut::RedisTransactions => {
            let util_script = if config.worker_hosts.len() == 1 {
                format!("{}/../utils/restart-redis-instance.sh", config.magpie_root)
            } else {
                format!(
                    "{}/../utils/restart-redis-cluster-instance.sh",
                    config.magpie_root
                )
            };

            for worker_host in &config.worker_hosts {
                let mut args = vec![
                    format!("{}@{}", config.ssh_user, worker_host),
                    format!("bash -lc \"{}\"", util_script,),
                ];

                if !config.worker_key_file_path.is_empty() {
                    args.insert(1, format!("-i{}", config.worker_key_file_path));
                }
                println!("about to restart redis on {}", worker_host);
                let mut script_proc = process::Command::new("ssh")
                    .args(args)
                    .stdout(process::Stdio::null())
                    .spawn()
                    .expect(&format!("failed to restart worker '{}'", worker_host));
                script_proc
                    .wait()
                    .expect("failed while waiting for redis restart");
            }

            std::thread::sleep(Duration::from_secs(5));
        }
        Sut::Memcached => {
            let util_script = format!("{}/../utils/restart-memcached.sh", config.magpie_root);

            for worker_host in &config.worker_hosts {
                let mut args = vec![
                    format!("{}@{}", config.ssh_user, worker_host),
                    format!("bash -lc \"{}\"", util_script,),
                ];

                if !config.worker_key_file_path.is_empty() {
                    args.insert(1, format!("-i{}", config.worker_key_file_path));
                }
                println!("about to restart memcached on {}", worker_host);
                let mut script_proc = process::Command::new("ssh")
                    .args(args)
                    .stdout(process::Stdio::null())
                    .spawn()
                    .expect(&format!("failed to restart worker '{}'", worker_host));
                script_proc
                    .wait()
                    .expect("failed while waiting for memcached restart");
            }

            std::thread::sleep(Duration::from_secs(5));
        }
    }
    println!("Done resetting cluster");
}

fn init_experiment(
    client: &reqwest::blocking::Client,
    config: &config::Config,
    setup: &ExperimentSetup,
    load_gen_hosts: &[String],
    total_clients: u16,
) {
    println!(
        "About to init experiment on {} load gen hosts",
        load_gen_hosts.len()
    );

    let mut remaining_clients = total_clients;
    for (idx, load_gen_host) in load_gen_hosts.iter().enumerate() {
        let host_url = format!(
            "http://{}:{}/init_experiment",
            load_gen_host, config.load_gen_port
        );

        let mut setup = setup.clone();
        setup.output_file += &format!(".{idx}");

        // NOTE this is to support increments of less than `max_clients_per_load_gen_host` when
        // using multiple load gen hosts.
        if total_clients > config.max_clients_per_load_gen_host {
            assert!(remaining_clients > 0);
            if idx > 0 {
                if remaining_clients < config.max_clients_per_load_gen_host {
                    setup.concurrency = remaining_clients;
                }
            }
            remaining_clients -= setup.concurrency;
        }

        println!(
            "Host {host_url} about to be set up with {} clients",
            setup.concurrency
        );

        loop {
            let resp = client
                .post(&host_url)
                .json(&setup)
                .timeout(Duration::from_secs(60))
                .send();

            if !resp.is_ok() {
                println!("Got an error response while trying to init: {:?}", resp);
                println!("will sleep for 5 secs and try again");

                std::thread::sleep(Duration::from_secs(5));

                continue;
            }

            break;
        }
    }
    println!("Done initting experiment");
}

fn run_experiment(
    client: &reqwest::blocking::Client,
    config: &config::Config,
    load_gen_hosts: &[String],
) {
    let deadline = {
        let now_utc = chrono::offset::Utc::now();
        // 5 secs should be enough time for us to set up every host.
        now_utc + chrono::Duration::new(5, 0).unwrap()
    };

    println!("About to run experiment at {}", deadline);
    let deadline_request = load_gen_api::ExperimentDeadline { deadline };

    for (idx, load_gen_host) in load_gen_hosts.iter().enumerate() {
        let host_url = format!(
            "http://{}:{}/start_experiment_at_deadline",
            load_gen_host, config.load_gen_port
        );
        let resp = client
            .post(&host_url)
            .json(&deadline_request)
            .timeout(Duration::from_secs(1))
            .send();
        if !resp.is_ok() {
            println!(
                "Got an error response while trying to run experiment: {:?}",
                resp
            );
        }
    }
    println!("All load gen hosts have been set up, will sleep");
}

fn await_experiment_completion(
    client: &reqwest::blocking::Client,
    config: &config::Config,
    load_gen_hosts: &[String],
) {
    println!("About to await experiment completion");
    for (idx, load_gen_host) in load_gen_hosts.iter().enumerate() {
        let host_url = format!(
            "http://{}:{}/experiment_finished",
            load_gen_host, config.load_gen_port
        );
        loop {
            let resp = match client
                .get(&host_url)
                .timeout(Duration::from_secs(60))
                .send()
            {
                Ok(r) => r
                    .json::<load_gen_api::ExperimentFinished>()
                    .expect("failed to parse experiment status result"),
                Err(e) => panic!("error response in experiment status call: {:?}", e),
            };

            if resp.finished {
                break;
            }

            std::thread::sleep(Duration::from_secs(1));
        }
    }
    println!("Done awaiting completion");
}

fn parse_args() -> Args {
    let mut args = Args::parse();

    if args.config_file_path.is_none() {
        args.config_file_path = Some("config.toml".into());
    }

    args
}

fn main() {
    let args = parse_args();
    let config = Config::init_from_file(args.config_file_path.clone().unwrap());

    let num_load_gen_hosts: u16 = config.load_gen_hosts.len().try_into().unwrap();

    let client_builder: reqwest::blocking::ClientBuilder = reqwest::blocking::Client::builder()
        .timeout(None)
        .tcp_nodelay(true)
        .into();
    let client = client_builder.build().expect("failed to construct client");

    let rw_ratios = match args.function {
        // Function::MixedKvs | Function::MixedKvsSkewed => vec![0.90, 0.50, 0.00],
        Function::MixedKvs
        | Function::MixedKvsSkewed
        | Function::MixedKvsMultihost
        | Function::MixedKvsNando => {
            vec![0.90, 0.50]
        }
        Function::ReadModifyWrite
        | Function::ReadModifyWriteMultihost
        | Function::ReadModifyWriteNando => vec![0.90, 0.50],
        _ => vec![1.0],
    };

    let exponents = match args.function {
        Function::MixedKvsSkewed => vec![0.99, 2.0, 3.0],
        _ => vec![1.0],
    };
    let suts = match args.sut {
        Some(s) => vec![s],
        // None => vec![Sut::Magpie, Sut::Redis, Sut::RedisTransactions, Sut::Memcached],
        None => vec![Sut::Magpie, Sut::Memcached],
    };

    let output_path = match config.output_path.is_empty() {
        false => config.output_path.clone(),
        true => "/tmp".to_string(),
    };

    let bucket_objects: Vec<u128> = config
        .bucket_objects
        .iter()
        .map(|b| b.parse::<u128>().unwrap())
        .collect();

    if args.function == Function::PageRank || args.function == Function::TriangleCount {
        let mut s = String::new();
        for num_worker_threads in &config.num_worker_threads {
            for iteration in 0..config.num_iterations {
                reset_cluster(&client, &config, &args, *num_worker_threads, Sut::Magpie);
                println!("cluster reset, press any button after experiment is over");
                let _ = std::io::stdin().read_line(&mut s).unwrap();
            }
        }

        return;
    }

    if args.function == Function::Sorting {
        let mut num_completed_experiments = 0;
        let load_gen_hosts = &config.load_gen_hosts[0..1];

        if args.weak {
            let total_experiments_sut = config.num_worker_threads.len();
            for num_worker_threads in &config.num_worker_threads {
                let formatted_num_threads = match config.worker_hosts.len() {
                    1 => *num_worker_threads,
                    w @ _ => w * *num_worker_threads,
                };

                let output_file = format!(
                    "{}/magpie-n{}-{}-weak",
                    output_path,
                    formatted_num_threads,
                    args.function.to_string(),
                );
                let output_file = match args.use_fold {
                    false => format!("{}-nofold", output_file),
                    true => format!("{}-fold", output_file),
                };
                let output_file = match args.use_hash_placement {
                    false => format!("{}-nohash", output_file),
                    true => format!("{}-hash", output_file),
                };

                let setup = ExperimentSetup {
                    sut: Sut::Magpie,
                    concurrency: 1,
                    seed: config.seed,
                    request_duration_sec: 10,
                    function: args.function,
                    num_input_objects: config.num_input_objects,
                    read_write_ratio: None,
                    exponent: None,
                    output_file,
                    target_hosts: config.worker_hosts.clone(),
                    root_object: args.root_object,
                    bucket_objects: bucket_objects.clone(),
                    num_buckets_per_host: args.num_buckets_per_host.clone(),
                    plan: match config.worker_hosts.len() > 1 {
                        false => None,
                        true => match args.use_hash_placement {
                            true => match args.use_fold {
                                false => Some(format!("sort-collection-bulk-hash")),
                                true => Some(format!("sort-collection-fold-hash")),
                            },
                            false => match args.use_fold {
                                false => Some(format!("sort-collection-bulk-any")),
                                true => Some(format!("sort-collection-fold-any")),
                            },
                        },
                    },
                    string_size_kb: None,
                    num_chunks: None,

                    graph_file: None,
                    interval_secs: None,
                    num_multi_get_keys: None,

                    sort_num_output_partitions: Some(formatted_num_threads),
                    use_fold: args.use_fold,
                };

                let mut args = args.clone();
                args.sort_num_input_partitions = Some(formatted_num_threads);
                args.sort_chunk_size =
                    Some((formatted_num_threads / config.worker_hosts.len()) as u16);

                println!("Experiment setup: {:#?}", setup);
                for i in 0..config.num_iterations {
                    let mut setup = setup.clone();
                    setup.output_file += &format!("-{}", i);

                    println!(
                        "About to set up iteration {} / {} of experiment {num_completed_experiments} / {total_experiments_sut}",
                        i + 1,
                        config.num_iterations,
                    );
                    reset_cluster(&client, &config, &args, *num_worker_threads, Sut::Magpie);
                    init_experiment(&client, &config, &setup, load_gen_hosts, 1);
                    run_experiment(&client, &config, load_gen_hosts);
                    await_experiment_completion(&client, &config, load_gen_hosts);
                    println!(
                        "Done with iteration {} / {} of experiment {num_completed_experiments} / {total_experiments_sut}",
                        i + 1,
                        config.num_iterations,
                    );

                    if args.prompt_before_reset && i + 1 != config.num_iterations {
                        let mut s = String::new();
                        println!("Waiting for confirmation before next iteration; press any button to continue");
                        let _ = std::io::stdin().read_line(&mut s).unwrap();
                    }
                }
            }
        } else {
            let total_experiments_sut =
                config.sort_num_input_partitions.len() * config.sort_num_output_partitions.len();
            let num_worker_threads = *config.num_worker_threads.get(0).unwrap();
            let formatted_num_threads = match config.worker_hosts.len() {
                1 => num_worker_threads,
                w @ _ => w * num_worker_threads,
            };

            for num_input_partitions in &config.sort_num_input_partitions {
                for num_output_partitions in &config.sort_num_output_partitions {
                    let output_file = format!(
                        "{}/magpie-n{}-{}-i{}-o{}",
                        output_path,
                        formatted_num_threads,
                        args.function.to_string(),
                        num_input_partitions,
                        num_output_partitions
                    );

                    let output_file = match args.use_fold {
                        false => format!("{}-nofold", output_file),
                        true => format!("{}-fold", output_file),
                    };
                    let output_file = match args.use_hash_placement {
                        false => format!("{}-nohash", output_file),
                        true => format!("{}-hash", output_file),
                    };

                    let setup = ExperimentSetup {
                        sut: Sut::Magpie,
                        concurrency: 1,
                        seed: config.seed,
                        request_duration_sec: 10,
                        function: args.function,
                        num_input_objects: config.num_input_objects,
                        read_write_ratio: None,
                        exponent: None,
                        output_file,
                        target_hosts: config.worker_hosts.clone(),
                        root_object: args.root_object,
                        bucket_objects: bucket_objects.clone(),
                        num_buckets_per_host: args.num_buckets_per_host.clone(),
                        plan: match config.worker_hosts.len() > 1 {
                            false => None,
                            true => match args.use_hash_placement {
                                true => match args.use_fold {
                                    false => Some(format!("sort-collection-bulk-hash")),
                                    true => Some(format!("sort-collection-fold-hash")),
                                },
                                false => match args.use_fold {
                                    false => Some(format!("sort-collection-bulk-any")),
                                    true => Some(format!("sort-collection-fold-any")),
                                },
                            },
                        },
                        string_size_kb: None,
                        num_chunks: None,

                        graph_file: None,
                        interval_secs: None,
                        num_multi_get_keys: None,

                        sort_num_output_partitions: Some(*num_output_partitions),
                        use_fold: args.use_fold,
                    };

                    let mut args = args.clone();
                    args.sort_num_input_partitions = Some(*num_input_partitions);
                    args.sort_chunk_size =
                        Some((*num_input_partitions / config.worker_hosts.len()) as u16);

                    println!("Experiment setup: {:#?}", setup);
                    for i in 0..config.num_iterations {
                        let mut setup = setup.clone();
                        setup.output_file += &format!("-{}", i);

                        println!(
                            "About to set up iteration {} / {} of experiment {num_completed_experiments} / {total_experiments_sut}",
                            i + 1,
                            config.num_iterations,
                        );
                        reset_cluster(&client, &config, &args, num_worker_threads, Sut::Magpie);
                        init_experiment(&client, &config, &setup, load_gen_hosts, 1);
                        run_experiment(&client, &config, load_gen_hosts);
                        await_experiment_completion(&client, &config, load_gen_hosts);
                        println!(
                            "Done with iteration {} / {} of experiment {num_completed_experiments} / {total_experiments_sut}",
                            i + 1,
                            config.num_iterations,
                        );

                        if args.prompt_before_reset && i + 1 != config.num_iterations {
                            let mut s = String::new();
                            println!("Waiting for confirmation before next iteration; press any button to continue");
                            let _ = std::io::stdin().read_line(&mut s).unwrap();
                        }
                    }
                }
            }
        }

        return;
    }

    if args.function == Function::SmithWaterman {
        let mut num_completed_experiments = 0;
        let num_worker_threads = *config.num_worker_threads.get(0).unwrap();
        let total_experiments_sut = config.num_concurrent_clients.len();

        let load_gen_hosts = &config.load_gen_hosts[0..1];

        // This is the cumulative number of worker threads across all worker hosts,
        // used only in the output file names.
        let formatted_num_threads = match config.worker_hosts.len() {
            1 => num_worker_threads,
            w @ _ => w * num_worker_threads,
        };
        for size_kb in &config.sizes_kb {
            for num_chunks in &config.num_chunks {
                let output_file = format!(
                    "{}/magpie-n{}-{}-{}kb-{}",
                    output_path,
                    formatted_num_threads,
                    args.function.to_string(),
                    size_kb,
                    num_chunks,
                );
                let output_file = match args.use_hash_placement {
                    false => output_file,
                    true => format!("{}-hash", output_file),
                };
                let setup = ExperimentSetup {
                    sut: Sut::Magpie,
                    concurrency: 1,
                    seed: config.seed,
                    request_duration_sec: 10,
                    function: args.function,
                    num_input_objects: config.num_input_objects,
                    read_write_ratio: None,
                    exponent: None,
                    output_file,
                    target_hosts: config.worker_hosts.clone(),
                    root_object: args.root_object,
                    bucket_objects: bucket_objects.clone(),
                    num_buckets_per_host: args.num_buckets_per_host.clone(),
                    plan: match config.worker_hosts.len() > 1 {
                        false => None,
                        true => match args.use_hash_placement {
                            true => Some(format!("sw-non-step-hash")),
                            false => Some(format!("sw-{num_chunks}-non-step")),
                        },
                    },
                    string_size_kb: Some(*size_kb),
                    num_chunks: Some(*num_chunks),

                    graph_file: None,
                    interval_secs: None,
                    num_multi_get_keys: None,
                    sort_num_output_partitions: None,
                    use_fold: false,
                };

                println!("Experiment setup: {:#?}", setup);
                for i in 0..config.num_iterations {
                    // Append iteration number to output files
                    let mut setup = setup.clone();
                    setup.output_file += &format!("-{}", i);

                    println!(
                        "About to set up iteration {} / {} of experiment {num_completed_experiments} / {total_experiments_sut}",
                        i + 1,
                        config.num_iterations,
                    );
                    reset_cluster(&client, &config, &args, num_worker_threads, Sut::Magpie);
                    init_experiment(&client, &config, &setup, load_gen_hosts, 1);
                    run_experiment(&client, &config, load_gen_hosts);
                    // std::thread::sleep(Duration::from_secs(setup.request_duration_sec));
                    await_experiment_completion(&client, &config, load_gen_hosts);
                    println!(
                        "Done with iteration {} / {} of experiment {num_completed_experiments} / {total_experiments_sut}",
                        i + 1,
                        config.num_iterations,
                    );

                    if args.prompt_before_reset && i + 1 != config.num_iterations {
                        let mut s = String::new();
                        println!("Waiting for confirmation before next iteration; press any button to continue");
                        let _ = std::io::stdin().read_line(&mut s).unwrap();
                    }
                }

                num_completed_experiments += 1;
            }
        }

        return;
    }

    for sut in &suts {
        let mut num_completed_experiments = 0;

        let variant_suffix = match sut {
            Sut::Magpie => "mvthresh",
            _ => "",
        };

        // The set of worker threads __per host__.
        let num_worker_threads = match sut {
            Sut::Magpie => match config.worker_hosts.len() > 1 {
                true => vec![16],
                false => config.num_worker_threads.clone(),
            },
            Sut::Memcached => vec![16],
            _ => vec![1],
        };

        let total_experiments_sut = num_worker_threads.len()
            * exponents.len()
            * rw_ratios.len()
            * config.num_concurrent_clients.len();

        for num_worker_threads in &num_worker_threads {
            for rw_ratio in &rw_ratios {
                for exponent in &exponents {
                    for num_clients in &config.num_concurrent_clients {
                        let (concurrency_per_host, load_gen_hosts) =
                            if *num_clients > config.max_clients_per_load_gen_host {
                                let max_clients_per_load_gen_host =
                                    config.max_clients_per_load_gen_host;
                                let max_worker_idx = {
                                    let maybe_max_idx =
                                        (*num_clients / max_clients_per_load_gen_host) as usize;

                                    if *num_clients % max_clients_per_load_gen_host == 0 {
                                        maybe_max_idx
                                    } else {
                                        maybe_max_idx + 1
                                    }
                                };
                                (
                                    max_clients_per_load_gen_host,
                                    &config.load_gen_hosts[0..max_worker_idx],
                                )
                            } else {
                                (*num_clients, &config.load_gen_hosts[0..1])
                            };

                        let variant_name = match variant_suffix.is_empty() {
                            true => sut.to_string(),
                            false => format!("{}_{}", sut, variant_suffix),
                        };

                        // This is the cumulative number of worker threads across all worker hosts,
                        // used only in the output file names.
                        let formatted_num_threads = match config.worker_hosts.len() {
                            1 => *num_worker_threads,
                            w @ _ => w * *num_worker_threads,
                        };
                        let setup = ExperimentSetup {
                            sut: *sut,
                            concurrency: concurrency_per_host,
                            seed: config.seed,
                            request_duration_sec: 10,
                            function: args.function,
                            num_input_objects: config.num_input_objects,
                            read_write_ratio: Some(*rw_ratio),
                            exponent: Some(*exponent),
                            output_file: format!(
                                "{}/{}-n{}c{}-{}-rw-{:.2}",
                                output_path,
                                variant_name,
                                formatted_num_threads,
                                num_clients,
                                args.function.to_string(),
                                rw_ratio,
                            ),
                            target_hosts: config.worker_hosts.clone(),
                            root_object: args.root_object,
                            bucket_objects: bucket_objects.clone(),
                            num_buckets_per_host: args.num_buckets_per_host.clone(),

                            graph_file: None,
                            plan: None,
                            string_size_kb: None,
                            num_chunks: None,
                            interval_secs: None,
                            num_multi_get_keys: None,
                            sort_num_output_partitions: None,
                            use_fold: false,
                        };

                        println!("Experiment setup: {:#?}", setup);

                        for i in 0..config.num_iterations {
                            // Append iteration number to output files
                            let mut setup = setup.clone();
                            setup.output_file += &format!("-{}", i);

                            println!(
                                "About to set up iteration {} / {} of experiment {num_completed_experiments} / {total_experiments_sut}",
                                i + 1,
                                config.num_iterations,
                            );
                            reset_cluster(&client, &config, &args, *num_worker_threads, *sut);
                            init_experiment(&client, &config, &setup, load_gen_hosts, *num_clients);
                            run_experiment(&client, &config, load_gen_hosts);
                            std::thread::sleep(Duration::from_secs(setup.request_duration_sec));
                            await_experiment_completion(&client, &config, load_gen_hosts);
                            println!(
                                "Done with iteration {} / {} of experiment {num_completed_experiments} / {total_experiments_sut}",
                                i + 1,
                                config.num_iterations,
                            );
                        }

                        num_completed_experiments += 1;
                    }
                }
            }
        }
    }
}
