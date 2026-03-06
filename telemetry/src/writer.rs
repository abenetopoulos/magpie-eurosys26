use std::fs;
use std::mem::MaybeUninit;
use std::path::Path;
use std::sync::Once;
use std::thread;

use core_affinity;
use crossbeam_channel::{Receiver, Sender, TryRecvError};
use lazy_static::lazy_static;
use nando_support::activation_intent::{HostId, HostIdx};
use serde_json;
use slog::{info, o, Drain};

use crate::*;

lazy_static! {
    static ref TELEMETRY_DIR: &'static Path = Path::new("/tmp/magpie-telemetry/");
}

pub type TelemetryEventSender = Sender<TelemetryEvent>;

pub(crate) struct TelemetryLogManager {
    #[allow(dead_code)]
    current_host_id: HostId,
    #[allow(dead_code)]
    log_queue_recv: Receiver<TelemetryEvent>,
    log_queue_send: TelemetryEventSender,
}

impl TelemetryLogManager {
    pub(crate) fn get_telemetry_log_mgr(
        maybe_host_id: Option<HostId>,
        maybe_host_idx: Option<HostIdx>,
    ) -> &'static TelemetryLogManager {
        static mut INSTANCE: MaybeUninit<TelemetryLogManager> = MaybeUninit::uninit();
        static mut ONCE: Once = Once::new();

        unsafe {
            ONCE.call_once(|| {
                INSTANCE.as_mut_ptr().write(Self::init(
                    maybe_host_id
                        .expect("Cannot instantiate telemetry manager without a valid host id"),
                    maybe_host_idx
                        .expect("Cannot instantiate telemetry manager without a valid host idx"),
                ));
            });
        }

        unsafe { &*INSTANCE.as_ptr() }
    }

    fn set_up_telemetry_dir() -> Result<(), ()> {
        let dir = TELEMETRY_DIR.to_path_buf();

        match fs::create_dir_all(&dir) {
            Ok(()) => Ok(()),
            Err(e) => {
                // FIXME
                panic!("Failed to create telemetry dir {:?}: {e}", dir);
            }
        }
    }

    fn init(host_id: String, host_idx: HostIdx) -> Self {
        let (log_queue_send, log_queue_recv) =
            crossbeam_channel::bounded::<TelemetryEvent>(16 * 8192);

        let (task_drain, metrics_drain) = {
            Self::set_up_telemetry_dir().unwrap();
            let pathbuf = TELEMETRY_DIR.to_path_buf();

            let task_path_buf = {
                let mut pathbuf = pathbuf.clone();
                pathbuf.push(&format!("{}.{}", host_id, host_idx));
                pathbuf
            };

            let metrics_path_buf = {
                let mut pathbuf = pathbuf.clone();
                pathbuf.push(&format!("{}.{}.metrics", host_id, host_idx));
                pathbuf
            };

            let task_file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(true)
                .open(&task_path_buf)
                .unwrap();

            let metrics_file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(true)
                .open(&metrics_path_buf)
                .unwrap();

            (
                {
                    let decorator = slog_term::PlainDecorator::new(task_file);
                    let drain = slog_term::CompactFormat::new(decorator)
                        .use_custom_timestamp(|_t| Ok(()))
                        .build()
                        .fuse();
                    slog_async::Async::new(drain)
                        .chan_size(4 * 8192 as usize)
                        .overflow_strategy(slog_async::OverflowStrategy::Block)
                        .build()
                        .fuse()
                },
                {
                    let decorator = slog_term::PlainDecorator::new(metrics_file);
                    let drain = slog_term::CompactFormat::new(decorator)
                        .use_custom_timestamp(|_t| Ok(()))
                        .build()
                        .fuse();
                    slog_async::Async::new(drain)
                        .chan_size(4 * 8192 as usize)
                        .overflow_strategy(slog_async::OverflowStrategy::Block)
                        .build()
                        .fuse()
                },
            )
        };
        let (task_log, metrics_log) = (
            slog::Logger::root(task_drain, o!()),
            slog::Logger::root(metrics_drain, o!()),
        );

        {
            let log_queue_recv = log_queue_recv.clone();
            let host_id = host_id.clone();
            thread::Builder::new()
                .name("telemetry_thread".to_string())
                .spawn(move || {
                    if !core_affinity::set_for_current(core_affinity::CoreId { id: 0 }) {
                        eprintln!("failed to set core affinity for telemetry thread");
                    }
                    Self::poll_and_log(log_queue_recv, task_log, metrics_log, host_id);
                })
                .expect("failed to spawn telemetry_thread");
        }

        Self {
            current_host_id: host_id,
            log_queue_recv,
            log_queue_send,
        }
    }

    pub(crate) fn get_producer_handle() -> TelemetryEventSender {
        let manager = Self::get_telemetry_log_mgr(None, None);
        manager.log_queue_send.clone()
    }

    fn poll_and_log(
        recv: Receiver<TelemetryEvent>,
        task_log: slog::Logger,
        metrics_log: slog::Logger,
        host_id: HostId,
    ) {
        loop {
            match recv.try_recv() {
                Ok(mut e) => {
                    e.event_information.set_host_id(&host_id);
                    let serialized_event = serde_json::to_string(&e).unwrap();

                    let log = match e.event_information {
                        EventInformation::Spawn { .. } => &task_log,
                        _ => &metrics_log,
                    };
                    info!(log, "{}", serialized_event);
                }
                Err(TryRecvError::Empty) => (),
                Err(TryRecvError::Disconnected) => todo!("executor <-> TM channel disconnected"),
            }
        }
    }
}
