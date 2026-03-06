use nando_support::{
    activation_intent::{
        HostId, HostIdx, NandoArgument, NandoArgumentSerializable, SpawnedTaskSerializable,
    },
    ecb_id::EcbId,
    epic_control::SpawnedTask,
};
use serde::{Deserialize, Serialize};
pub use writer::TelemetryEventSender;

pub mod config;
mod writer;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum NotificationKind {
    Control,
    Data(usize, NandoArgumentSerializable),
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum MetricsInformation {
    TasksScheduled(usize),
    WorkerTasksExecuted { executor_id: u16, num_tasks: usize },
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum EventInformation {
    Spawn {
        spawned_task: SpawnedTaskSerializable,
        host: HostId,
    },
    Schedule {
        id: EcbId,
        queue_depth: usize,
        host: HostId,
    },
    Commit {
        id: EcbId,
    },
    Completed {
        completed_task_id: EcbId,
        notifying_task_id: EcbId,
    },
    Notification {
        kind: NotificationKind,
        notification_source: EcbId,
        notification_target: EcbId,
    },
    ArgsUnresolvableLocally {
        intent_name: String,
        unresolvable_argument_idx: usize,
        host: HostId,
    },
    ExecutorDequeue(EcbId),
    WorkerMetrics(HostId, MetricsInformation),
    Executed {
        id: EcbId,
        duration_ns: u128,
    },
    ArgumentsResolved {
        id: EcbId,
        duration_ns: u128,
    },
}

impl EventInformation {
    fn new_spawn(task: &SpawnedTask) -> Self {
        Self::Spawn {
            spawned_task: task.into(),
            host: HostId::default(),
        }
    }

    fn new_unresolvable_arg(intent_name: &str, idx: usize) -> Self {
        Self::ArgsUnresolvableLocally {
            intent_name: intent_name.to_string(),
            unresolvable_argument_idx: idx,
            host: HostId::default(),
        }
    }

    fn new_schedule(id: EcbId, queue_depth: usize) -> Self {
        Self::Schedule {
            id,
            queue_depth,
            host: HostId::default(),
        }
    }

    fn new_commit(id: EcbId) -> Self {
        Self::Commit { id }
    }

    fn new_completed(completed_task_id: EcbId, notifying_task_id: EcbId) -> Self {
        Self::Completed {
            completed_task_id,
            notifying_task_id,
        }
    }

    fn new_data_notification(
        arg_idx: usize,
        value: &NandoArgument,
        notification_source: EcbId,
        notification_target: EcbId,
    ) -> Self {
        Self::Notification {
            kind: NotificationKind::Data(arg_idx, value.into()),
            notification_source,
            notification_target,
        }
    }

    fn new_control_notification(notification_source: EcbId, notification_target: EcbId) -> Self {
        Self::Notification {
            kind: NotificationKind::Control,
            notification_source,
            notification_target,
        }
    }

    fn new_executor_dequeue(task_id: EcbId) -> Self {
        Self::ExecutorDequeue(task_id)
    }

    fn new_scheduler_metrics(num_tasks_scheduled: usize) -> Self {
        Self::WorkerMetrics(
            HostId::default(),
            MetricsInformation::TasksScheduled(num_tasks_scheduled),
        )
    }

    fn new_executor_metrics(executor_id: u16, num_tasks_dequeued: usize) -> Self {
        Self::WorkerMetrics(
            HostId::default(),
            MetricsInformation::WorkerTasksExecuted {
                executor_id,
                num_tasks: num_tasks_dequeued,
            },
        )
    }

    fn new_executed(id: EcbId, duration_ns: u128) -> Self {
        Self::Executed { id, duration_ns }
    }

    fn new_executor_resolution(id: EcbId, duration_ns: u128) -> Self {
        Self::ArgumentsResolved { id, duration_ns }
    }

    pub(crate) fn set_host_id(&mut self, host_id: &HostId) {
        match self {
            Self::Spawn { ref mut host, .. }
            | Self::Schedule { ref mut host, .. }
            | Self::ArgsUnresolvableLocally { ref mut host, .. }
            | Self::WorkerMetrics(ref mut host, _) => *host = host_id.clone(),
            _ => {}
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TelemetryEvent {
    timestamp: jiff::Zoned,
    pub event_information: EventInformation,
}

impl TelemetryEvent {
    pub fn new_spawn(task: &SpawnedTask, ts: jiff::Zoned) -> Self {
        Self {
            timestamp: ts,
            event_information: EventInformation::new_spawn(task),
        }
    }

    pub fn new_unresolvable_arg(intent_name: &str, idx: usize, ts: jiff::Zoned) -> Self {
        Self {
            timestamp: ts,
            event_information: EventInformation::new_unresolvable_arg(intent_name, idx),
        }
    }

    pub fn new_schedule(id: EcbId, queue_depth: usize, ts: jiff::Zoned) -> Self {
        Self {
            timestamp: ts,
            event_information: EventInformation::new_schedule(id, queue_depth),
        }
    }

    pub fn new_commit(id: EcbId, ts: jiff::Zoned) -> Self {
        Self {
            timestamp: ts,
            event_information: EventInformation::new_commit(id),
        }
    }

    pub fn new_completed(
        completed_task_id: EcbId,
        notifying_task_id: EcbId,
        ts: jiff::Zoned,
    ) -> Self {
        Self {
            timestamp: ts,
            event_information: EventInformation::new_completed(
                completed_task_id,
                notifying_task_id,
            ),
        }
    }

    pub fn new_data_notification(
        arg_idx: usize,
        value: &NandoArgument,
        notification_source: EcbId,
        notification_target: EcbId,
        ts: jiff::Zoned,
    ) -> Self {
        Self {
            timestamp: ts,
            event_information: EventInformation::new_data_notification(
                arg_idx,
                value,
                notification_source,
                notification_target,
            ),
        }
    }

    pub fn new_control_notification(
        notification_source: EcbId,
        notification_target: EcbId,
        ts: jiff::Zoned,
    ) -> Self {
        Self {
            timestamp: ts,
            event_information: EventInformation::new_control_notification(
                notification_source,
                notification_target,
            ),
        }
    }

    pub fn new_executor_dequeue(task_id: EcbId, ts: jiff::Zoned) -> Self {
        Self {
            timestamp: ts,
            event_information: EventInformation::new_executor_dequeue(task_id),
        }
    }

    pub fn new_scheduler_metrics(num_tasks_scheduled: usize, ts: jiff::Zoned) -> Self {
        Self {
            timestamp: ts,
            event_information: EventInformation::new_scheduler_metrics(num_tasks_scheduled),
        }
    }

    pub fn new_executor_metrics(
        executor_id: u16,
        num_tasks_dequeued: usize,
        ts: jiff::Zoned,
    ) -> Self {
        Self {
            timestamp: ts,
            event_information: EventInformation::new_executor_metrics(
                executor_id,
                num_tasks_dequeued,
            ),
        }
    }

    pub fn new_executed(id: EcbId, duration_ns: u128, ts: jiff::Zoned) -> Self {
        Self {
            timestamp: ts,
            event_information: EventInformation::new_executed(id, duration_ns),
        }
    }

    pub fn new_executor_resolution(id: EcbId, duration_ns: u128, ts: jiff::Zoned) -> Self {
        Self {
            timestamp: ts,
            event_information: EventInformation::new_executor_resolution(id, duration_ns),
        }
    }

    pub fn get_timestamp(&self) -> jiff::Zoned {
        return self.timestamp.clone();
    }
}

pub fn zoned_timestamp_now() -> jiff::Zoned {
    jiff::Zoned::now()
}

pub fn submit_telemetry_event(handle: &TelemetryEventSender, event: TelemetryEvent) {
    match handle.try_send(event) {
        Ok(()) => {}
        Err(_e) => {
            #[cfg(debug_assertions)]
            eprintln!("failed to submit event, dropping: {_e}");
        }
    }
}

pub fn get_telemetry_handle() -> TelemetryEventSender {
    writer::TelemetryLogManager::get_producer_handle()
}

pub fn init_telemetry_manager(host_id: HostId, host_idx: HostIdx) {
    let _ = writer::TelemetryLogManager::get_telemetry_log_mgr(Some(host_id), Some(host_idx));
}
