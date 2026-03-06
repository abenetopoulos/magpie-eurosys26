use std::future::Future;
use std::slice::Iter;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::{self, JoinHandle as StdJoinHandle};

use core_affinity;
use futures_util::future::pending as pending_fut;
use rand::{rngs::SmallRng, Rng, SeedableRng};
use tokio::{self, task::JoinHandle as TokioJoinHandle};

use crate::config::Config;
pub use crate::config::RuntimeType;

pub mod config;

pub struct AsyncRuntimeManager {
    rt_type: RuntimeType,
    last_used_rt: AtomicUsize,
    runtimes: Vec<Arc<tokio::runtime::Runtime>>,
    _runtime_threads: Vec<StdJoinHandle<()>>,
}

impl AsyncRuntimeManager {
    pub fn new(config: &Config, num_executor_threads: usize) -> Self {
        let rt_type = match &config.kind {
            None => RuntimeType::TokioMultiThread,
            Some(ref rt_type) => rt_type.clone(),
        };

        println!("Setting up async runtime of type '{:?}'", rt_type);

        let mut runtime_threads = vec![];
        let runtimes = match rt_type {
            RuntimeType::TokioMultiThread => {
                vec![Arc::new(
                    tokio::runtime::Builder::new_multi_thread_alt()
                        .worker_threads(config.num_threads as usize)
                        .enable_all()
                        .build()
                        .unwrap(),
                )]
            }
            RuntimeType::RuntimePerCore => {
                let num_threads = config.num_threads as usize;
                let mut handles = Vec::with_capacity(num_threads);
                let (tx, rx) = mpsc::sync_channel(num_threads);

                for i in 0..config.num_threads {
                    let event_interval = match config.event_interval {
                        None => 61,
                        Some(e) => e,
                    };
                    let global_queue_interval = match config.global_queue_interval {
                        None => 31,
                        Some(e) => e,
                    };
                    let core_id = core_affinity::CoreId {
                        id: num_executor_threads + 1 + i as usize,
                    };

                    let tx = tx.clone();
                    let thread_handle = thread::Builder::new()
                        .name(format!("async-thread-{}", i + 1))
                        .spawn(move || {
                            if !core_affinity::set_for_current(core_id) {
                                eprintln!("failed to set core affinity for async thread {}", i);
                            }

                            let rt = Arc::new(
                                tokio::runtime::Builder::new_current_thread()
                                    .enable_all()
                                    .global_queue_interval(global_queue_interval)
                                    .event_interval(event_interval)
                                    .build()
                                    .unwrap(),
                            );

                            tx.send(Arc::clone(&rt))
                                .expect("failed to send rt handle to manager");

                            // NOTE The below ensures that the carrier thread won't join and that the
                            // wrapped single-threaded tokio runtime will stick around.
                            rt.block_on(async { pending_fut::<()>().await });
                        })
                        .expect(&format!("failed to spawn async thread {}", i));

                    runtime_threads.push(thread_handle);
                }

                while handles.len() < num_threads {
                    handles.push(rx.recv().unwrap());
                }

                handles
            }
        };

        Self {
            rt_type,
            last_used_rt: AtomicUsize::default(),
            runtimes,
            _runtime_threads: runtime_threads,
        }
    }

    fn pick_rt(&self) -> Arc<tokio::runtime::Runtime> {
        match self.rt_type {
            RuntimeType::TokioMultiThread => Arc::clone(&self.runtimes[0]),
            // FIXME for now just do a simple round-robin dispatch for the PoC.
            RuntimeType::RuntimePerCore => {
                // FIXME this might slow us down if contended
                let core = self.last_used_rt.fetch_add(1, Ordering::Relaxed);
                Arc::clone(&self.runtimes[core % self.runtimes.len()])
            }
        }
    }

    fn pick_rt_any(&self) -> Arc<tokio::runtime::Runtime> {
        match self.rt_type {
            RuntimeType::TokioMultiThread => Arc::clone(&self.runtimes[0]),
            RuntimeType::RuntimePerCore => {
                let mut rng = SmallRng::from_entropy();
                let core = rng.gen::<usize>();
                Arc::clone(&self.runtimes[core % self.runtimes.len()])
            }
        }
    }

    pub fn block_on<F: Future>(&self, future: F) -> F::Output {
        let rt = self.pick_rt();
        rt.block_on(future)
    }

    pub fn spawn<F>(&self, future: F) -> TokioJoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        let rt = self.pick_rt();
        rt.spawn(future)
    }

    pub fn spawn_any<F>(&self, future: F) -> TokioJoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        let rt = self.pick_rt_any();
        rt.spawn(future)
    }

    pub fn runtime_iter(&self) -> Iter<'_, Arc<tokio::runtime::Runtime>> {
        self.runtimes.iter()
    }
}
