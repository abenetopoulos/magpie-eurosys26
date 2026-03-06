use std::io::Write;
use std::mem::MaybeUninit;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Once};
use std::thread::{self, sleep, JoinHandle};
use std::time::Duration;

use reqwest;

use crate::{workload_loop, Args, Client};

pub struct ExperimentRunner {
    config: Option<Args>,
    join_handles: Vec<JoinHandle<(u16, u64)>>,
    start: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
}

impl ExperimentRunner {
    pub fn new() -> Self {
        Self {
            config: None,
            join_handles: vec![],
            start: Arc::new(AtomicBool::new(false)),
            stop: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn get_experiment_runner() -> &'static ExperimentRunner {
        let experiment_runner = Self::get_experiment_runner_mut();
        experiment_runner
    }

    pub fn get_experiment_runner_mut() -> &'static mut ExperimentRunner {
        static mut INSTANCE: MaybeUninit<ExperimentRunner> = MaybeUninit::uninit();
        static mut ONCE: Once = Once::new();

        unsafe {
            ONCE.call_once(|| {
                INSTANCE.as_mut_ptr().write(ExperimentRunner::new());
            });
        }

        unsafe { &mut *INSTANCE.as_mut_ptr() }
    }

    pub fn init_experiment(
        &mut self,
        args: Args,
        hosts: &Vec<String>,
        handle: &tokio::runtime::Handle,
    ) {
        self.stop.store(false, Ordering::Relaxed);
        self.config = Some(args.clone());
        let experiment_config = self.config.as_ref().unwrap();

        let num_remote_hosts = hosts.len();

        let multihost = hosts.len() > 1;

        for thread_id in 1..=experiment_config.concurrency {
            let args = args.clone();
            let client = match args.sut {
                load_gen_api::Sut::Magpie => {
                    let client_builder: reqwest::blocking::ClientBuilder =
                        reqwest::blocking::Client::builder()
                            .http2_prior_knowledge()
                            .tcp_keepalive(Duration::from_secs(10))
                            .tcp_nodelay(true)
                            .connection_verbose(true)
                            .connect_timeout(std::time::Duration::from_secs(1))
                            .timeout(None)
                            .into();
                    crate::Client::Magpie(
                        client_builder.build().expect("failed to construct client"),
                    )
                }
                load_gen_api::Sut::Redis | load_gen_api::Sut::RedisTransactions => {
                    let hostname = hosts[(thread_id as usize - 1) % num_remote_hosts].clone();
                    let redis_url = format!("redis://{}:6379", hostname);
                    let client = redis::Client::open(redis_url).unwrap();
                    crate::Client::Redis(client)
                }
                load_gen_api::Sut::Memcached => {
                    let memcached_urls: Vec<_> = hosts
                        .iter()
                        .map(|h| format!("memcache://{}:11211?timeout=60&tcp_nodelay=true", h))
                        .collect();
                    let client = memcache::connect(memcached_urls).unwrap();
                    crate::Client::Memcached(client)
                }
            };
            let control_tuple = (Arc::clone(&self.start), Arc::clone(&self.stop));

            let hostname = hosts[(thread_id as usize - 1) % num_remote_hosts].clone();

            self.join_handles.push(
                thread::Builder::new()
                    .name(format!("req-{}", thread_id))
                    .spawn(move || {
                        workload_loop(thread_id, hostname, args, client, control_tuple, multihost)
                    })
                    .unwrap(),
            );
        }
    }

    pub fn start_experiment(&mut self, collect: bool) {
        let Some(ref experiment_config) = self.config else {
            panic!("no experiment configuration found, quitting");
        };

        self.start.store(true, Ordering::Relaxed);
        sleep(Duration::from_secs(experiment_config.request_duration_sec));
        self.stop.store(true, Ordering::Relaxed);

        if collect {
            self.collect_results();
        }

        self.start.store(false, Ordering::Relaxed);
    }

    pub fn is_completed(&self) -> bool {
        // FIXME also include flag to know if output collation is finished.
        let start = self.start.load(Ordering::Relaxed);
        let stop = self.stop.load(Ordering::Relaxed);

        stop && !start
    }

    pub fn collect_results(&mut self) {
        let Some(ref experiment_config) = self.config else {
            panic!("no experiment configuration found, quitting");
        };
        let mut total_requests: u64 = 0;
        for handle in self.join_handles.drain(..) {
            let (_thread_id, client_requests) = handle.join().expect("worker thread failed");
            total_requests += client_requests;
        }

        println!("Number of requests: {}", total_requests);
        println!("Average per-client throughput");
        let cumulative_throughput =
            (total_requests as f64) / experiment_config.request_duration_sec as f64;
        println!(
            "Cumulative Client Throughput: {} txns/sec",
            cumulative_throughput
        );

        let Some(ref p) = experiment_config.output_file else {
            return;
        };

        eprintln!("Aggregating worker output files");
        let mut output_file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(p.clone())
            .unwrap();

        for thread_id in 1..=experiment_config.concurrency {
            let pathbuf = p.clone();
            let path = pathbuf.as_path();
            let filename = path.file_name().unwrap();
            let path = path.with_file_name(format!("{}.{}", filename.to_str().unwrap(), thread_id));

            {
                let mut input_file = std::fs::File::open(&path).unwrap();
                let _ = std::io::copy(&mut input_file, &mut output_file);
            }
            let _ = std::fs::remove_file(path);
        }
        writeln!(output_file, "Number of requests: {}", total_requests);
        writeln!(
            output_file,
            "Cumulative Client Throughput: {} txns/sec",
            cumulative_throughput
        )
        .expect("failed to append cumulative throughput");
    }
}
