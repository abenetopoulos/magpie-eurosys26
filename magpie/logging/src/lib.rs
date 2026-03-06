#![feature(cell_leak, extract_if)]
#![allow(dead_code)]
use std::fs;
use std::io::Write;
use std::mem::MaybeUninit;
use std::path::Path;
use std::sync::Once;
use std::thread;
#[cfg(any(feature = "timing", feature = "timing-log"))]
use std::time::Instant;

use crossbeam_channel::{Receiver, RecvError, Sender};
use execution_definitions::activation::NandoActivation;
use nando_support::log_entry::TransactionLogEntry;
use nando_support::nando_metadata::NandoKind;
use rtrb::{Producer, PushError};

use crate::config::Config;

pub mod config;
pub mod intent_log;

pub type TxnId = u128;

// FIXME this value just corresponds to typical fs block size, but it should actually be set after
// measuring performance etc. This might actually be too low.
// FIXME make this configurable through env or flag.
const MAX_BUFFER_SIZE_BYTES: usize = 4096;

// Similar in function to the other singletons in the system, responsible for maintaining
// all logs and log-related resources (backing files etc.). At the very least it contains
// an intent log (in the future) and an operation log.
pub struct LogManager {
    buffer: Vec<u8>,
    // TODO rotate files when reaching certain size
    log_file: Box<fs::File>,

    current_log_file_number: u32,
    current_log_sn: u128,

    log_queue_recv: Receiver<NandoActivation>,
    log_queue_snd: Sender<NandoActivation>,
}

// FIXME error handling
impl LogManager {
    pub fn new(config: Config) -> Self {
        let (log_queue_snd, log_queue_recv) = crossbeam_channel::bounded::<NandoActivation>(
            512 * config.num_executor_threads as usize,
        );

        // FIXME rotating log files
        let log_path = Path::new("/tmp/magpie-logs/");
        let current_log_file_number = 0;
        let log_file_path_str = &format!("transaction-log.{}", current_log_file_number);
        let log_file_name = Path::new(log_file_path_str);

        match fs::create_dir_all(log_path) {
            Ok(()) => (),
            Err(e) => {
                eprintln!("Failed while creating transaction log directory: {}", e);
                panic!("Failed while creating transaction log directory");
            }
        }

        let log_file_path = log_path.join(log_file_name);
        let mut file_options = fs::OpenOptions::new();
        file_options
            .read(true)
            .write(false)
            .create(true)
            .append(true)
            .truncate(false);

        match file_options.open(&log_file_path) {
            Ok(f) => Self {
                buffer: vec![],
                log_file: Box::new(f),
                current_log_file_number,
                current_log_sn: 0,
                log_queue_recv,
                log_queue_snd,
            },
            Err(e) => {
                eprintln!(
                    "Failed while opening transaction log file {}: {}",
                    log_file_path.to_str().unwrap(),
                    e
                );
                panic!("Failed while creating transaction log directory");
            }
        }
    }

    pub fn get_txn_log_manager(maybe_config: Option<Config>) -> &'static LogManager {
        Self::get_txn_log_manager_mut(maybe_config) as &LogManager
    }

    fn get_txn_log_manager_mut(maybe_config: Option<Config>) -> &'static mut LogManager {
        static mut INSTANCE: MaybeUninit<LogManager> = MaybeUninit::uninit();
        static mut ONCE: Once = Once::new();

        unsafe {
            ONCE.call_once(|| {
                INSTANCE
                    .as_mut_ptr()
                    .write(LogManager::new(maybe_config.expect(
                        "Cannot instantiate log manager without a valid configuration instance",
                    )));
            });
        }

        unsafe { &mut *INSTANCE.as_mut_ptr() }
    }

    pub fn init_persistence_loop(
        &self,
        scheduler_queue_send_handles: Vec<Producer<NandoActivation>>,
    ) {
        let log_queue_recv = self.log_queue_recv.clone();
        let num_cores = core_affinity::get_core_ids()
            .expect("failed to get list of core ids")
            .len();
        let core_id = core_affinity::CoreId {
            id: (num_cores - 1),
        };
        thread::Builder::new()
            .name("logging-thread".to_string())
            .spawn(move || {
                if !core_affinity::set_for_current(core_id) {
                    eprintln!("failed to set core affinity for logging thread");
                }
                Self::persist_loop(log_queue_recv, scheduler_queue_send_handles);
            })
            .expect("failed to spawn logging thread");
    }

    pub fn get_log_queue_sender(&self) -> crossbeam_channel::Sender<NandoActivation> {
        self.log_queue_snd.clone()
    }

    pub fn append(&mut self, log_entry: &mut TransactionLogEntry) {
        let serialized_entry = self.serialize_log_entry(log_entry);

        if self.buffer.len() + serialized_entry.len() > MAX_BUFFER_SIZE_BYTES {
            self.flush_buffer();
        }

        self.buffer.extend_from_slice(&serialized_entry);
    }

    pub fn shutdown(&self) {
        Self::get_txn_log_manager_mut(None).flush_buffer();
    }

    fn flush_buffer(&mut self) {
        match self.log_file.write_all(&self.buffer) {
            Ok(()) => {
                self.log_file.flush().expect("failed to flush log file");
                self.buffer = Vec::with_capacity(MAX_BUFFER_SIZE_BYTES);
            }
            Err(e) => {
                eprintln!("Failed while flushing buffer to active log file: {}", e);
                panic!("Failed while flushing buffer to active log file");
            }
        }
    }

    fn serialize_log_entry(&mut self, log_entry: &mut TransactionLogEntry) -> Vec<u8> {
        // FIXME although the below serialization logic allows for straightforward reading back
        // from a transaction log, it can generate serialized entries with a lot of zeroes. We can
        // probably do better, but it will do for now.
        let mut serialized_log_entry = vec![];

        let entry_sequence_number = self.current_log_sn;
        self.current_log_sn += 1;

        serialized_log_entry.extend_from_slice(&entry_sequence_number.to_ne_bytes());

        let num_images: u64 = log_entry.images.len().try_into().unwrap();
        let num_read_dependencies: u64 = log_entry.read_set.len().try_into().unwrap();
        let num_write_dependencies: u64 = log_entry.write_set.len().try_into().unwrap();

        serialized_log_entry.extend_from_slice(&num_images.to_ne_bytes());
        serialized_log_entry.extend_from_slice(&num_read_dependencies.to_ne_bytes());
        serialized_log_entry.extend_from_slice(&num_write_dependencies.to_ne_bytes());

        for (_, image_vec) in &log_entry.images {
            for image in image_vec {
                serialized_log_entry.extend_from_slice(&image.as_byte_array());
            }
        }
        log_entry.images.clear();

        for read_object_version in &log_entry.read_set {
            serialized_log_entry.extend_from_slice(&read_object_version.as_byte_array());
        }
        log_entry.read_set.clear();

        for (write_object_version, _) in &log_entry.write_set {
            serialized_log_entry.extend_from_slice(&write_object_version.as_byte_array());
        }
        log_entry.write_set = log_entry
            .write_set
            .extract_if(|(_, allocated)| *allocated)
            .collect();

        serialized_log_entry
    }

    fn persist_loop(
        log_queue_recv: crossbeam_channel::Receiver<NandoActivation>,
        mut scheduler_queue_send_handles: Vec<Producer<NandoActivation>>,
    ) {
        let mut last_completion_thread_idx: usize = 0;
        let num_channels = scheduler_queue_send_handles.len();

        loop {
            match log_queue_recv.recv() {
                Ok(a) => {
                    #[cfg(feature = "timing-log")]
                    let start = Instant::now();
                    #[cfg(feature = "timing-log")]
                    let intent_name = a.activation_intent.name.clone();

                    #[cfg(feature = "timing")]
                    {
                        let timestamp = Instant::now();
                        a.set_timing_entity("exec_to_logger_queue_end", timestamp);
                        a.set_timing_entity("logging_start", timestamp);
                    }

                    #[cfg(not(feature = "no-persist"))]
                    {
                        if !(a.is_pending() || a.is_failed()) {
                            match a.meta.kind {
                                NandoKind::ReadOnly => (),
                                _ => {
                                    // NOTE ideally we would just drop the log entry and forward the
                                    // activation along to cut back on the amount of data we have to move.
                                    // For now this method that clears image data should do.
                                    let mut log_entry = a.activation_log_entry.borrow_mut();
                                    Self::get_txn_log_manager_mut(None).append(&mut log_entry);
                                }
                            }
                        }
                    }

                    #[cfg(feature = "timing")]
                    {
                        let timestamp = Instant::now();
                        a.set_timing_entity("logging_end", timestamp);
                        a.set_timing_entity("logger_to_sched_queue_start", timestamp);
                    }

                    let scheduler_queue_send = {
                        let idx = last_completion_thread_idx % num_channels;
                        last_completion_thread_idx += 1;
                        unsafe { scheduler_queue_send_handles.get_unchecked_mut(idx) }
                    };

                    match scheduler_queue_send.push(a) {
                        Ok(()) => (),
                        Err(PushError::Full(_)) => todo!("log manager <-> scheduler queue is full"),
                    }

                    #[cfg(feature = "timing-log")]
                    {
                        let duration = start.elapsed();
                        println!(
                            "appending entry to log for {intent_name} took {}us ({}ns)",
                            duration.as_micros(),
                            duration.as_nanos()
                        );
                    }
                }
                Err(e @ RecvError) => todo!("executor <-> TM channel disconnected: {e}"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use object_lib::IPtr;

    use crate::config::Config;
    use crate::{LogManager, TransactionLogEntry};

    #[test]
    #[ignore]
    fn append_test() {
        let mut log_manager = LogManager::new(Config {
            num_executor_threads: 2,
        });

        let mut log_entry = TransactionLogEntry::new(123, Some(1));
        log_entry.add_object_to_read_set(123, 1);
        log_entry.add_object_to_read_set(230, 9);
        log_entry.add_object_to_read_set(456, 9);
        log_entry.add_object_to_write_set(456, 10);

        log_manager.append(&log_entry);
        log_manager.flush_buffer();
        assert!(true);
    }

    #[test]
    #[ignore]
    fn perf_test() {
        let num_iterations = 1_000_000;
        let mut log_manager = LogManager::new(Config {
            num_executor_threads: 2,
        });

        let mut total_append_duration = Duration::default();
        let mut max_append_duration = Duration::ZERO;
        let start = Instant::now();
        let first_iptr = IPtr::new(123, 0, 4);
        let second_iptr = IPtr::new(456, 0, 4);

        for i in 1..num_iterations {
            let mut log_entry = TransactionLogEntry::new(i, Some(1));
            log_entry.add_object_to_read_set(123, 1);
            log_entry.add_new_pre_image(&first_iptr, &42u32.to_ne_bytes());
            log_entry.add_object_to_write_set(456, 10);
            log_entry.add_new_pre_image(&second_iptr, &42u32.to_ne_bytes());
            log_entry.add_new_post_image_if_changed(&second_iptr, &16u32.to_ne_bytes());

            let append_start = Instant::now();
            log_manager.append(&log_entry);
            let append_duration = append_start.elapsed();
            total_append_duration += append_duration;

            if append_duration > max_append_duration {
                max_append_duration = append_duration;
            }
        }
        let duration = start.elapsed();

        eprintln!(
            "\nTime for {num_iterations} iterations: {}ms ({}us)",
            duration.as_millis(),
            duration.as_micros()
        );
        eprintln!(
            "Avg time per append: {}us ({:.3}ns)",
            total_append_duration.as_micros() / num_iterations,
            total_append_duration.as_nanos() / num_iterations
        );
        eprintln!(
            "Log operation duration: {}us (max {}us)",
            total_append_duration.as_micros(),
            max_append_duration.as_micros()
        );
    }
}
