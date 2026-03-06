#![allow(dead_code)]

use std::collections::HashMap;
use std::mem::MaybeUninit;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Once};

use async_lib::AsyncRuntimeManager;
use async_std::sync::Arc as AsyncArc;
use dashmap::DashMap;
use epic_completion_handle::EpicCompletionHandle;
use execution_definitions::nando_handle::{ActivationOutput, NandoHandleOutputType};
#[cfg(feature = "observability")]
use lazy_static::lazy_static;
use location_manager::{HostId, LocationManager};
use logging::{intent_log, TxnId};
use nando_support::activation_intent::{NandoActivationIntent, NandoArgument, NandoResult};
use nando_support::epic_control;
use nando_support::{ecb_id::EcbId, epic_control::SpawnedTask};
use object_lib::{IPtr, ObjectId, ObjectVersion};
use ownership_tracker::OwnershipTracker;
#[cfg(feature = "observability")]
use prometheus::{register_histogram_vec, HistogramVec};
#[cfg(any(feature = "timing-tm", feature = "timing-tm-wait"))]
use tokio::time::Instant;

use crate::nando_scheduler::{
    ControlRegistryEntry, NandoScheduler, ScheduleResult, TaskCompletionNotification,
};

mod epic_completion_handle;

#[cfg(feature = "observability")]
lazy_static! {
    static ref OPS_TM_FWD_HISTOGRAM: HistogramVec = register_histogram_vec!(
        "transaction_manager_forward_latency_histogram",
        "Latency of task forwarding through TM in microseconds",
        &["num_tasks"],
        vec![10.0, 20.0, 50.0, 100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 10000.0],
    )
    .unwrap();
    static ref OPS_TM_SCHED_HISTOGRAM: HistogramVec = register_histogram_vec!(
        "transaction_manager_schedule_spawned_task_histogram",
        "Latency of task scheduling through TM in microseconds",
        &["intent"],
        vec![10.0, 20.0, 50.0, 100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 10000.0],
    )
    .unwrap();
    static ref OPS_TM_EXEC_HISTOGRAM: HistogramVec = register_histogram_vec!(
        "transaction_manager_execute_spawned_task_histogram",
        "Latency of task execution through TM in microseconds",
        &["intent"],
        vec![10.0, 20.0, 50.0, 100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 10000.0],
    )
    .unwrap();
    static ref EPICS_TM_EXEC_HISTOGRAM: HistogramVec = register_histogram_vec!(
        "transaction_manager_execute_epic_histogram",
        "Latency of epic execution through TM in microseconds",
        &["intent"],
        vec![10.0, 20.0, 50.0, 100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 10000.0],
    )
    .unwrap();
    static ref EPICS_TM_END_HISTOGRAM: HistogramVec = register_histogram_vec!(
        "transaction_manager_execute_epic_ete_histogram",
        "Latency of e2e epic execution on TM in microseconds",
        &["intent"],
        vec![10.0, 20.0, 50.0, 100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 10000.0],
    )
    .unwrap();
    static ref EPICS_TM_NOTIF_HISTOGRAM: HistogramVec = register_histogram_vec!(
        "transaction_manager_execute_epic_notification_histogram",
        "Latency of epic execution up to notification in microseconds",
        &["intent"],
        vec![10.0, 20.0, 50.0, 100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 10000.0],
    )
    .unwrap();
}

pub struct TransactionManager {
    txn_id_counter: AtomicUsize,
    epic_completion_handle_map: DashMap<EcbId, Arc<EpicCompletionHandle>>,
    task_fwd_fn: Box<dyn Fn(SpawnedTask, Option<HostId>) + Send + Sync>,
    tasks_fwd_to_fn:
        Box<dyn Fn(Vec<SpawnedTask>, HashMap<HostId, Vec<EcbId>>, Option<HostId>) + Send + Sync>,
    task_completion_fwd_fn: Box<dyn Fn(TaskCompletionNotification) + Send + Sync>,
    async_rt_manager: Arc<AsyncRuntimeManager>,
    // location_manager: AsyncArc<LocationManager>,
}

impl TransactionManager {
    fn new(
        async_rt_manager: Arc<AsyncRuntimeManager>,
        _location_manager: AsyncArc<LocationManager>,
    ) -> Self {
        Self {
            txn_id_counter: AtomicUsize::new(1),
            epic_completion_handle_map: DashMap::new(),
            task_fwd_fn: Box::new(|_spawned_task: SpawnedTask, _host_id: Option<HostId>| {
                panic!("tm's task_fwd_fn has not been set");
            }),
            tasks_fwd_to_fn: Box::new(
                |_spawned_tasks: Vec<SpawnedTask>,
                 _spawned_task_locations: HashMap<HostId, Vec<EcbId>>,
                 _host_id: Option<HostId>| {
                    panic!("tm's task_fwd_to_fn has not been set");
                },
            ),
            task_completion_fwd_fn: Box::new(
                |_task_completion_notification: TaskCompletionNotification| {
                    panic!("tm's task_completion_fwd_fn has not been set");
                },
            ),
            async_rt_manager,
            // location_manager,
        }
    }

    pub fn get_transaction_manager(
        maybe_rt_manager: Option<Arc<AsyncRuntimeManager>>,
        maybe_location_manager: Option<AsyncArc<LocationManager>>,
    ) -> &'static TransactionManager {
        let transaction_manager =
            Self::get_transaction_manager_mut(maybe_rt_manager, maybe_location_manager);
        transaction_manager
    }

    pub fn get_transaction_manager_mut(
        maybe_rt_manager: Option<Arc<AsyncRuntimeManager>>,
        maybe_location_manager: Option<AsyncArc<LocationManager>>,
    ) -> &'static mut TransactionManager {
        static mut INSTANCE: MaybeUninit<TransactionManager> = MaybeUninit::uninit();
        static mut ONCE: Once = Once::new();

        unsafe {
            ONCE.call_once(|| {
                let async_rt_manager = maybe_rt_manager.expect(
                    "cannot instantiate transaction manager without a valid runtime manager instance",
                );
                let location_manager = maybe_location_manager.expect(
                    "cannot instantiate transaction manager without an active LocationManager instance"
                );

                INSTANCE
                    .as_mut_ptr()
                    .write(TransactionManager::new(async_rt_manager, location_manager));
            });
        }

        unsafe { &mut *INSTANCE.as_mut_ptr() }
    }

    pub fn set_task_fwd_fn(
        &mut self,
        task_fwd_fn: Box<dyn Fn(SpawnedTask, Option<HostId>) + Send + Sync>,
    ) {
        self.task_fwd_fn = task_fwd_fn;
    }

    pub fn set_tasks_fwd_to_fn(
        &mut self,
        tasks_fwd_to_fn: Box<
            dyn Fn(Vec<SpawnedTask>, HashMap<HostId, Vec<EcbId>>, Option<HostId>) + Send + Sync,
        >,
    ) {
        self.tasks_fwd_to_fn = tasks_fwd_to_fn;
    }

    pub fn set_task_completion_fwd_fn(
        &mut self,
        task_completion_fwd_fn: Box<dyn Fn(TaskCompletionNotification) + Send + Sync>,
    ) {
        self.task_completion_fwd_fn = task_completion_fwd_fn;
    }

    pub async fn execute_nando(
        &self,
        activation_intent: NandoActivationIntent,
        await_epic_result: bool,
        with_plan: Option<String>,
    ) -> Result<
        (Vec<ActivationOutput>, Vec<(ObjectId, ObjectVersion)>),
        (String, Option<ActivationOutput>),
    > {
        #[cfg(feature = "timing-tm")]
        let start = Instant::now();
        #[cfg(feature = "timing-tm")]
        let intent_name = activation_intent.name.clone();

        let txn_id: TxnId = self
            .txn_id_counter
            .fetch_add(1, Ordering::Relaxed)
            .try_into()
            .unwrap();
        let _run_intent_log_entry = intent_log::IntentLogEntry::new(
            txn_id,
            intent_log::IntentKind::UserTransactionExecution,
            intent_log::IntentLogEntryStatus::Start,
        );
        // TODO append to intent log

        let nando_scheduler = NandoScheduler::get_nando_scheduler(None, None);
        #[cfg(feature = "observability")]
        let intent_name = activation_intent.name.clone();
        #[cfg(feature = "observability")]
        let transaction_start_ts = std::time::Instant::now();
        let execution_result =
            match nando_scheduler.schedule_activation(txn_id, activation_intent, with_plan) {
                Ok(ScheduleResult::Nando(nando_handle)) => {
                    #[cfg(feature = "timing-tm")]
                    {
                        let duration = start.elapsed();
                        println!(
                            "submitting {intent_name} took {}us ({}ns)",
                            duration.as_micros(),
                            duration.as_nanos()
                        );
                    }

                    let _success_intent_log_entry = intent_log::IntentLogEntry::new(
                        txn_id,
                        intent_log::IntentKind::UserTransactionExecution,
                        intent_log::IntentLogEntryStatus::Success,
                    );
                    // TODO synchronously append to intent log
                    #[cfg(feature = "timing-tm-wait")]
                    let wait_start = Instant::now();

                    match nando_handle.await {
                        Ok(NandoHandleOutputType::Result(r, o)) => {
                            #[cfg(feature = "timing-tm-wait")]
                            {
                                let wait_duration = wait_start.elapsed();
                                println!(
                                    "spent {}us ({}ns) waiting for completion",
                                    wait_duration.as_micros(),
                                    wait_duration.as_nanos()
                                );
                            }
                            (r, o)
                        }
                        Ok(_) => {
                            panic!("invalid handle output type for individual nanotransaction")
                        }
                        Err(e) => return Err((e.to_string(), None)),
                    }
                }
                Ok(ScheduleResult::Epic(ecb_handle)) => {
                    let ecb_id = ecb_handle
                        .get_ecb_id()
                        .expect("failed to get ecb id from epic handle");
                    let completion_handle = Arc::new(EpicCompletionHandle::new_triggered(ecb_id));

                    match self
                        .epic_completion_handle_map
                        .insert(ecb_id, Arc::clone(&completion_handle))
                        .is_none()
                    {
                        true => {}
                        false => {
                            // if this happens, it means that we just overwrote an entry that
                            // corresponded to a "complete" epic (added by the submission thread while
                            // we were processing the handle etc.), so we also need to mark our entry
                            // as completed. Given that we have yet to notify the client about the
                            // epic, we do not need to call notify (as there are no waiters).
                            completion_handle.mark_completed();
                        }
                    }

                    match ecb_handle.await {
                        Ok(NandoHandleOutputType::Processing(ecb_id, cacheable_deps)) => {
                            if await_epic_result {
                                completion_handle.await_completion().await;
                                if completion_handle.get_status().await.is_failed() {
                                    return Err((
                                        "Unknown error".to_string(),
                                        Some(ActivationOutput::Epic(ecb_id)),
                                    ));
                                }
                            }

                            #[cfg(feature = "observability")]
                            {
                                let epic_duration_tm = transaction_start_ts.elapsed();
                                match *completion_handle.completion_timestamp.read() {
                                    None => {}
                                    Some(completion_ts) => {
                                        EPICS_TM_END_HISTOGRAM
                                            .with_label_values(&[&intent_name])
                                            .observe(epic_duration_tm.as_micros() as f64);
                                        EPICS_TM_NOTIF_HISTOGRAM
                                            .with_label_values(&[&intent_name])
                                            .observe(
                                                completion_handle
                                                    .pre_notification_timestamp
                                                    .read()
                                                    .unwrap()
                                                    .duration_since(transaction_start_ts.into())
                                                    .as_micros()
                                                    as f64,
                                            );
                                        EPICS_TM_EXEC_HISTOGRAM
                                            .with_label_values(&[&intent_name])
                                            .observe(
                                                completion_ts
                                                    .duration_since(transaction_start_ts.into())
                                                    .as_micros()
                                                    as f64,
                                            );
                                    }
                                }
                            }
                            (vec![ActivationOutput::Epic(ecb_id)], cacheable_deps)
                        }
                        Ok(NandoHandleOutputType::Result(r, o)) => {
                            completion_handle.mark_completed();
                            (r, o)
                        }
                        Err(e) => {
                            return Err((e.to_string(), Some(ActivationOutput::Epic(ecb_id))))
                        }
                    }
                }
                Err(e) => return Err((e, None)),
            };

        Ok(execution_result)
    }

    pub async fn execute_spawned_task(
        &self,
        spawned_task: epic_control::SpawnedTask,
    ) -> Result<(Vec<ActivationOutput>, Vec<(ObjectId, ObjectVersion)>), String> {
        let txn_id: TxnId = self
            .txn_id_counter
            .fetch_add(1, Ordering::Relaxed)
            .try_into()
            .unwrap();

        let nando_scheduler = NandoScheduler::get_nando_scheduler(None, None);
        #[cfg(feature = "observability")]
        let intent = spawned_task.intent.name.clone();
        #[cfg(feature = "observability")]
        let sched_start = tokio::time::Instant::now();
        let execution_result =
            match nando_scheduler.schedule_externally_spawned_task(txn_id, spawned_task) {
                Ok(ScheduleResult::Epic(ecb_handle)) => {
                    #[cfg(feature = "observability")]
                    {
                        let sched_duration = sched_start.elapsed().as_micros() as f64;
                        OPS_TM_SCHED_HISTOGRAM
                            .with_label_values(&[&intent])
                            .observe(sched_duration);
                    }
                    let ecb_id = ecb_handle
                        .get_ecb_id()
                        .expect("failed to get ecb id from epic handle");
                    let completion_handle = Arc::new(EpicCompletionHandle::new_triggered(ecb_id));

                    match self
                        .epic_completion_handle_map
                        .insert(ecb_id, Arc::clone(&completion_handle))
                    {
                        None => {}
                        Some(_e) => {
                            // if this happens, it means that we just overwrote an entry that
                            // corresponded to a "complete" epic (added by the submission thread while
                            // we were processing the handle etc.), so we also need to mark our entry
                            // as completed. Given that we have yet to notify the client about the
                            // epic, we do not need to call notify (as there are no waiters).
                            completion_handle.mark_completed();
                        }
                    }

                    // Immediately notify of schedule
                    match ecb_handle.await {
                        Ok(NandoHandleOutputType::Processing(ecb_id, cacheable_deps)) => {
                            (vec![ActivationOutput::Epic(ecb_id)], cacheable_deps)
                        }
                        Ok(NandoHandleOutputType::Result(r, o)) => (r, o),
                        Err(e) => return Err(e.to_string()),
                    }
                }
                Err(e) => return Err(e),
                _ => return Err("unexpected result kind when spawning epic subtask".to_string()),
            };
        #[cfg(feature = "observability")]
        {
            let exec_duration = sched_start.elapsed().as_micros() as f64;
            OPS_TM_EXEC_HISTOGRAM
                .with_label_values(&[&intent])
                .observe(exec_duration);
        }

        Ok(execution_result)
    }

    pub fn store_spawned_task(&self, spawned_task: epic_control::SpawnedTask) {
        let nando_scheduler = NandoScheduler::get_nando_scheduler(None, None);
        nando_scheduler.store_externally_spawned_task(spawned_task);
    }

    pub async fn whomstone_object(&self, object_id: ObjectId) -> Result<ObjectVersion, String> {
        #[cfg(debug_assertions)]
        println!("got request to whomstone {object_id}");
        let txn_id: TxnId = self
            .txn_id_counter
            .fetch_add(1, Ordering::Relaxed)
            .try_into()
            .unwrap();
        let _migrate_intent_log_entry = intent_log::IntentLogEntry::new(
            txn_id,
            intent_log::IntentKind::OwnershipChange,
            intent_log::IntentLogEntryStatus::Start,
        );

        let ownership_change_intent = NandoActivationIntent {
            host_idx: None,
            name: "whomstone".to_string(),
            args: vec![NandoArgument::Ref(IPtr {
                object_id,
                offset: 0,
                size: 0,
            })],
        };

        let nando_scheduler = NandoScheduler::get_nando_scheduler(None, None);
        let ownership_change_result =
            match nando_scheduler.schedule_activation(txn_id, ownership_change_intent, None) {
                Ok(ScheduleResult::Epic(ecb_handle)) => {
                    let ecb_id = ecb_handle
                        .get_ecb_id()
                        .expect("failed to get ecb id from epic handle");
                    let completion_handle = Arc::new(EpicCompletionHandle::new_triggered(ecb_id));

                    match self
                        .epic_completion_handle_map
                        .insert(ecb_id, Arc::clone(&completion_handle))
                        .is_none()
                    {
                        true => {}
                        false => {
                            // if this happens, it means that we just overwrote an entry that
                            // corresponded to a "complete" epic (added by the submission thread while
                            // we were processing the handle etc.), so we also need to mark our entry
                            // as completed. Given that we have yet to notify the client about the
                            // epic, we do not need to call notify (as there are no waiters).
                            completion_handle.mark_completed();
                        }
                    }

                    let _success_intent_log_entry = intent_log::IntentLogEntry::new(
                        txn_id,
                        intent_log::IntentKind::OwnershipChange,
                        intent_log::IntentLogEntryStatus::Success,
                    );
                    // TODO synchronously append to intent log

                    #[cfg(debug_assertions)]
                    println!(
                        "tm will await transaction handle of whomstoning txn {}",
                        txn_id
                    );

                    match ecb_handle.await {
                        Ok(NandoHandleOutputType::Processing(ecb_id, _cacheable_deps)) => {
                            completion_handle.await_completion().await;
                            let nando_scheduler = NandoScheduler::get_nando_scheduler(None, None);
                            match nando_scheduler.get_ecb_result_and_status(ecb_id) {
                                None => return Err(format!("no ECB found for ecb id {}", ecb_id)),
                                Some((r, _)) => r,
                            }
                        }
                        Ok(NandoHandleOutputType::Result(r, _)) => {
                            let Some(ActivationOutput::Result(v)) = r.get(0) else {
                                panic!("unexpected result");
                            };

                            Some(v.clone())
                        }
                        Err(e) => return Err(e.to_string()),
                    }
                }
                Ok(ScheduleResult::Nando(_)) => panic!("shouldn't happen"),
                Err(_) => todo!(),
            };

        // let Some(ActivationOutput::Result(NandoResult::Value(v))) = ownership_change_result.pop()
        let Some(NandoResult::Value(v)) = ownership_change_result else {
            panic!("no whomstone version in result of ownership change");
        };

        Ok((&v).try_into().unwrap())
    }

    pub fn mark_epic_completed(
        &self,
        root_ecb_id: EcbId,
        completion_timestamp: Option<std::time::Instant>,
        status: epic_control::TaskStatus,
    ) {
        // Ideally we would be using DashMap's `Entry` API here, but because we need to trigger
        // notifications in the event that we find an entry in the map, we would end up holding
        // an exclusive lock for the duration of the notification task. With the logic below we
        // can avoid holding a lock because we're cloning an `Arc`, but we might end up racing
        // with the frontend task that triggered us and might end up overwriting the handle it
        // wrote.
        let entry = match self.epic_completion_handle_map.get(&root_ecb_id) {
            None => match self.epic_completion_handle_map.insert(
                root_ecb_id,
                Arc::new(EpicCompletionHandle::new_completed(root_ecb_id)),
            ) {
                None => return,
                Some(e) => {
                    // A frontend thread inserted a completion handle in the map that it might
                    // be awaiting completion on, so we need to process this entry.
                    Arc::clone(&e)
                }
            },
            Some(e) => Arc::clone(&e),
        };

        match status.is_failed() {
            false => entry.mark_completed_and_notify(completion_timestamp),
            true => entry.mark_failed_and_notify(completion_timestamp),
        }
    }

    pub async fn get_epic_result_and_status(
        &self,
        ecb_id: EcbId,
    ) -> Result<(Option<NandoResult>, epic_control::TaskStatus), String> {
        let completion_handle = {
            // let guard = self.epic_completion_handle_map.guard();
            match self.epic_completion_handle_map.get(&ecb_id) {
                // match self.epic_completion_handle_map.get(&ecb_id, &guard) {
                Some(ref e) => Arc::clone(e),
                None => return Err(format!("no handle state found for ecb with id {}", ecb_id)),
            }
        };

        completion_handle.await_completion().await;

        let nando_scheduler = NandoScheduler::get_nando_scheduler(None, None);
        match nando_scheduler.get_ecb_result_and_status(ecb_id) {
            None => Err(format!("no ECB found for ecb id {}", ecb_id)),
            Some(res_status) => Ok(res_status),
        }
    }

    pub fn forward_task_completion(
        &self,
        task_completion_notification: TaskCompletionNotification,
    ) {
        let rt_manager = Arc::clone(&self.async_rt_manager);
        rt_manager.spawn(async move {
            let transaction_manager = Self::get_transaction_manager(None, None);
            (transaction_manager.task_completion_fwd_fn)(task_completion_notification);
        });
    }

    pub fn schedule_parkable_entry(&self, entry: ControlRegistryEntry) {
        let nando_scheduler = NandoScheduler::get_nando_scheduler(None, None);
        nando_scheduler.schedule_parkable_entry(entry);
    }

    pub async fn handle_task_completion(
        &self,
        completed_task: EcbId,
        tasks_to_notify: Vec<(
            epic_control::DownstreamTaskDependency,
            epic_control::TaskStatus,
            Option<NandoResult>,
        )>,
    ) -> Vec<ControlRegistryEntry> {
        let nando_scheduler = NandoScheduler::get_nando_scheduler(None, None);
        let task_completion_result =
            nando_scheduler.handle_subtask_completion(completed_task, &tasks_to_notify, true);

        self.forward_spawned_tasks(&task_completion_result.spawned_tasks_to_fwd);
        for notification_to_fwd in task_completion_result.external_tasks_to_notify {
            (self.task_completion_fwd_fn)(notification_to_fwd);
        }

        task_completion_result.schedulable_tasks
    }

    pub fn forward_spawned_task(&self, spawned_task: SpawnedTask) {
        (self.task_fwd_fn)(spawned_task, None);
    }

    pub fn forward_spawned_tasks(&self, spawned_tasks: &Vec<SpawnedTask>) {
        let num_tasks = spawned_tasks.len();
        if num_tasks == 0 {
            return;
        }

        #[cfg(feature = "observability")]
        let fwd_start = std::time::Instant::now();
        (self.tasks_fwd_to_fn)(spawned_tasks.clone(), HashMap::default(), None);
        #[cfg(feature = "observability")]
        {
            let fwd_duration = fwd_start.elapsed();
            OPS_TM_FWD_HISTOGRAM
                .with_label_values(&[&num_tasks.to_string()])
                .observe(fwd_duration.as_micros() as f64);
        }
    }

    pub fn forward_spawned_task_to(&self, spawned_task: SpawnedTask, host_id: HostId) {
        #[cfg(feature = "observability")]
        let fwd_start = std::time::Instant::now();
        (self.task_fwd_fn)(spawned_task, Some(host_id));
        #[cfg(feature = "observability")]
        {
            let fwd_duration = fwd_start.elapsed();
            OPS_TM_FWD_HISTOGRAM
                .with_label_values(&[&num_tasks.to_string()])
                .observe(fwd_duration.as_micros() as f64);
        }
    }

    pub fn forward_spawned_tasks_to(
        &self,
        spawned_tasks: Vec<SpawnedTask>,
        spawned_task_locations: HashMap<HostId, Vec<EcbId>>,
        host_id: HostId,
    ) {
        let num_tasks = spawned_tasks.len();
        if num_tasks == 0 {
            return;
        }

        #[cfg(feature = "observability")]
        let fwd_start = std::time::Instant::now();
        (self.tasks_fwd_to_fn)(spawned_tasks, spawned_task_locations, Some(host_id));
        #[cfg(feature = "observability")]
        {
            let fwd_duration = fwd_start.elapsed();
            OPS_TM_FWD_HISTOGRAM
                .with_label_values(&[&num_tasks.to_string()])
                .observe(fwd_duration.as_micros() as f64);
        }
    }

    pub fn publish_objects(&self, objects_to_publish: Vec<(ObjectId, ObjectVersion)>) {
        if objects_to_publish.is_empty() {
            return;
        }

        let ownership_tracker = Arc::new(OwnershipTracker::get_ownership_tracker(None));
        if objects_to_publish
            .iter()
            .any(|(oid, _)| !ownership_tracker.object_is_owned(*oid))
        {
            todo!("attempt to publish an unowned object")
        }

        self.async_rt_manager.block_on(async move {
            let iptrs: Vec<IPtr> = objects_to_publish
                .iter()
                .map(|o| IPtr::new(o.0, 0, 0))
                .collect();
            if iptrs.len() == 1 {
                let iptr = iptrs.get(0).unwrap();
                ownership_tracker.publish_object(iptr).await;
                return;
            }

            ownership_tracker.publish_objects(&iptrs).await;
        });
    }
}
