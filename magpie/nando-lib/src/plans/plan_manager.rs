use std::collections::{HashMap, HashSet};
use std::fs;
use std::hash::Hash;
use std::mem::MaybeUninit;
use std::path::PathBuf;
use std::sync::{Arc, Once};

use dashmap::DashMap;
use execution_definitions::activation::NandoActivation;
use nando_support::{
    activation_intent::{IPtrList, NandoArgument},
    ecb_id::EcbId,
    epic_control,
    iptr::IPtr,
    ObjectId,
};
use ownership_tracker::{HostId, OwnershipTracker, Schedulable};

use crate::nando_scheduler::NandoScheduler;
use crate::plans::baked;
use crate::plans::definitions as plan_definitions;
use crate::plans::host_target_built_ins;

pub(crate) type Location = HostId;
#[derive(Eq, Hash, PartialEq, Clone, Debug)]
pub(crate) enum SubmissionLocation {
    Local,
    UnselectedRemote,
    Remote(Location),
    Remotes(Vec<Location>),
}

impl SubmissionLocation {
    pub fn is_local(&self) -> bool {
        match self {
            Self::Local => true,
            _ => false,
        }
    }

    pub fn get_remote(&self) -> Option<Location> {
        match self {
            Self::Remote(location) => Some(location.clone()),
            _ => None,
        }
    }

    pub fn get_cardinality(&self) -> usize {
        match self {
            Self::Remotes(ref remotes) => remotes.len(),
            _ => 1,
        }
    }
}

pub(crate) struct SubmissionLocationIter<'a> {
    current_idx: usize,
    inner: &'a SubmissionLocation,
}

impl<'a> IntoIterator for &'a SubmissionLocation {
    type Item = SubmissionLocation;
    type IntoIter = SubmissionLocationIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        Self::IntoIter {
            current_idx: 0,
            inner: self,
        }
    }
}

impl<'a> Iterator for SubmissionLocationIter<'a> {
    type Item = SubmissionLocation;

    fn next(&mut self) -> Option<Self::Item> {
        match self.inner {
            SubmissionLocation::Remotes(ref remote_locations) => {
                if remote_locations.len() == self.current_idx {
                    return None;
                }

                self.current_idx += 1;
                Some(SubmissionLocation::Remote(
                    remote_locations[self.current_idx - 1].clone(),
                ))
            }
            l @ _ => match self.current_idx {
                0 => {
                    self.current_idx += 1;
                    Some(l.clone())
                }
                _ => None,
            },
        }
    }
}

#[derive(Debug, Eq, PartialEq, Hash)]
pub(crate) enum OwnershipInformationKey {
    // NOTE keying by task id because I'm lazy
    UnknownObject(EcbId),
    KnownObject(ObjectId),
}

impl OwnershipInformationKey {
    fn get_object_if_known(&self) -> Option<ObjectId> {
        match self {
            Self::KnownObject(oid) => Some(*oid),
            _ => None,
        }
    }

    fn get_task_dep_if_unknown(&self) -> Option<EcbId> {
        match self {
            Self::UnknownObject(tid) => Some(*tid),
            _ => None,
        }
    }
}

impl From<ObjectId> for OwnershipInformationKey {
    fn from(value: ObjectId) -> Self {
        Self::KnownObject(value)
    }
}

impl From<EcbId> for OwnershipInformationKey {
    fn from(value: EcbId) -> Self {
        Self::UnknownObject(value)
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
enum PushCopyTargetObject {
    ResolvedObject(ObjectId),
    UnresolvedObject(usize, Option<EcbId>),
}

impl PushCopyTargetObject {
    fn get_object_id(&self) -> Option<ObjectId> {
        match self {
            Self::ResolvedObject(oid) => Some(*oid),
            Self::UnresolvedObject(_, _) => None,
        }
    }
}

#[derive(Debug)]
pub(crate) struct OwnershipInformation {
    current_owner: SubmissionLocation,
    target_host: SubmissionLocation,
    task: epic_control::SpawnedTask,
}

pub(crate) struct PhysicalPlanManager {
    parsed_plans: DashMap<String, plan_definitions::ParsedPhysicalPlan>,
    // NOTE for now this only holds the latest index we've handed out.
    planning_progress: DashMap<EcbId, usize>,
    num_workers: u16,
}

macro_rules! assign_to {
    ($map:ident, $st:ident, $sched:expr, $location:expr, $task_locations:ident) => {{
        $task_locations.insert($st.id, $location.clone());
        match $map.get_mut(&$location) {
            None => {
                $map.insert($location, vec![($st, $sched)]);
            }
            Some(tasks) => tasks.push(($st, $sched)),
        }
    }};
}

macro_rules! get_current_owner {
    ($ownership_tracker:ident, $object:ident) => {{
        match $ownership_tracker.object_is_owned($object) {
            false => {
                SubmissionLocation::Remote($ownership_tracker.get_remote_owner($object).unwrap())
            }
            true => SubmissionLocation::Local,
        }
    }};
}

impl PhysicalPlanManager {
    fn new(plan_directory_path: Option<PathBuf>, num_workers: u16) -> Self {
        let mut plans_map = Self::load_baked_plans();

        if let Some(plan_directory_path) = plan_directory_path {
            let plan_directory = fs::read_dir(&plan_directory_path).expect(&format!(
                "failed to read physical plan dir {}",
                plan_directory_path.display()
            ));

            Self::parse_plans(plan_directory, &mut plans_map);
        }

        Self {
            parsed_plans: plans_map,
            planning_progress: DashMap::new(),
            num_workers,
        }
    }

    fn load_baked_plans() -> DashMap<String, plan_definitions::ParsedPhysicalPlan> {
        let baked_plans = DashMap::new();

        for (key, plan) in baked::get_pre_baked() {
            #[cfg(debug_assertions)]
            println!("About to insert pre-parsed plan {}", key);
            baked_plans.insert(key.to_string(), plan);
        }

        baked_plans
    }

    fn parse_plans(
        plan_directory: fs::ReadDir,
        parsed_plans: &mut DashMap<String, plan_definitions::ParsedPhysicalPlan>,
    ) {
        for maybe_plan in plan_directory {
            let Ok(maybe_plan) = maybe_plan else {
                continue;
            };

            let plan_key = maybe_plan
                .file_name()
                .clone()
                .into_string()
                .expect("failed to convert key to str");
            let plan_path = maybe_plan.path();
            match plan_path.extension() {
                None => {
                    #[cfg(debug_assertions)]
                    println!("Skipping {}, missing extension", plan_key);
                    continue;
                }
                Some(e) => {
                    if e.to_str().unwrap() != "pp" {
                        #[cfg(debug_assertions)]
                        println!("Skipping {}, invalid extension (not `.pp`)", plan_key);
                        continue;
                    }
                }
            }

            // TODO parse
            #[cfg(debug_assertions)]
            println!("About to parse {}", plan_key);
            let parsed_plan = plan_definitions::ParsedPhysicalPlan::default();
            parsed_plans.insert(plan_key, parsed_plan);
        }
    }

    pub fn get_physical_plan_manager(
        plan_directory: Option<PathBuf>,
        num_workers: Option<u16>,
    ) -> &'static Arc<PhysicalPlanManager> {
        static mut INSTANCE: MaybeUninit<Arc<PhysicalPlanManager>> = MaybeUninit::uninit();
        static mut ONCE: Once = Once::new();

        unsafe {
            ONCE.call_once(|| {
                let num_workers = num_workers.expect(
                    "cannot instantiate physical plan manager without number of worker threads",
                );
                INSTANCE
                    .as_mut_ptr()
                    .write(Arc::new(PhysicalPlanManager::new(
                        plan_directory,
                        num_workers,
                    )));
            });
        }

        unsafe { &*INSTANCE.as_ptr() }
    }

    pub fn process_post_and_next_steps(
        &self,
        committed_task: &mut epic_control::ECB,
        committed_activation: &NandoActivation,
        local_executor_at_capacity: bool,
    ) -> HashMap<SubmissionLocation, Vec<(epic_control::SpawnedTask, Option<Schedulable>)>> {
        let committed_task_intent = &committed_activation.activation_intent;

        let planning_context = &committed_task.planning_context;
        let committed_task_idx = planning_context.idx;

        if planning_context.plan_key.is_empty() {
            return self.next_step_without_plan(committed_task, local_executor_at_capacity);
        }

        let plan = match self.parsed_plans.get(&planning_context.plan_key) {
            None => panic!("could not find plan with key {}", planning_context.plan_key),
            Some(p) => p,
        };

        let (post_ownership_info, post_task_map) =
            match plan.find_node_for_intent(&committed_task_intent.name, committed_task_idx) {
                Some(node) => self.process_post(
                    committed_task,
                    committed_activation,
                    committed_task_idx,
                    &node.post_actions,
                ),
                None => (HashMap::default(), HashMap::default()),
            };

        self.next_step_from_plan(&committed_task, &plan, post_ownership_info, post_task_map)
    }

    fn process_post(
        &self,
        committed_task: &mut epic_control::ECB,
        committed_activation: &NandoActivation,
        committed_task_idx: usize,
        post_action_block: &plan_definitions::PostActionBlock,
    ) -> (
        HashMap<OwnershipInformationKey, OwnershipInformation>,
        HashMap<SubmissionLocation, Vec<(epic_control::SpawnedTask, Option<Schedulable>)>>,
    ) {
        if post_action_block.is_default() {
            return (HashMap::default(), HashMap::default());
        }

        let post_action_block = &post_action_block.action_block;

        let ownership_tracker = OwnershipTracker::get_ownership_tracker(None);
        let self_host_idx = ownership_tracker.get_host_idx().unwrap();
        let ordered_hosts_list = ownership_tracker.get_ordered_host_list();

        let next_host_id = match ownership_tracker.get_next_host() {
            Some(h) => SubmissionLocation::Remote(h),
            None => SubmissionLocation::Local,
        };

        #[cfg(feature = "object-caching")]
        let mut pending_task_map: HashMap<
            SubmissionLocation,
            Vec<(epic_control::SpawnedTask, Option<Schedulable>)>,
        > = HashMap::with_capacity(8);

        #[cfg(not(feature = "object-caching"))]
        let pending_task_map: HashMap<
            SubmissionLocation,
            Vec<(epic_control::SpawnedTask, Option<Schedulable>)>,
        > = HashMap::with_capacity(8);

        let mut ownership_change_tasks: HashMap<OwnershipInformationKey, OwnershipInformation> =
            HashMap::with_capacity(8);

        let mut target_objects = HashSet::new();
        for ownership_transfer in &post_action_block.ownership_transfers {
            let target_host = &ownership_transfer.target_host;
            let target_host = match target_host {
                plan_definitions::HostTarget::NextHost => next_host_id.clone(),
                plan_definitions::HostTarget::HostIdx(_idx) => {
                    todo!("HostIdx in next_step");
                }
                plan_definitions::HostTarget::EvalResult(built_in) => {
                    let host_target_eval_ctx = plan_definitions::HostTargetEvalCtx {
                        hosts: &ordered_hosts_list,
                        self_idx: committed_task_idx,
                        num_workers: self.num_workers,
                    };
                    let target_host = host_target_built_ins::compute_host_target(
                        &built_in,
                        &host_target_eval_ctx,
                    );

                    if let plan_definitions::HostTarget::HostIdx(host_idx) = target_host {
                        match host_idx == self_host_idx as usize {
                            false => SubmissionLocation::Remote(
                                ownership_tracker
                                    .get_host_id_for_idx(host_idx as u64)
                                    .unwrap(),
                            ),
                            true => SubmissionLocation::Local,
                        }
                    } else {
                        unreachable!();
                    }
                }
                plan_definitions::HostTarget::Owner(ref object_ref) => {
                    let target_object = match object_ref.domain {
                        plan_definitions::ObjectArgumentDomain::Arguments => {
                            match object_ref.node_idx {
                                plan_definitions::TaskIndex::SelfIdx => committed_activation
                                    .get_object_arg_at_idx(object_ref.arg_idx)
                                    .expect("failed to get object arg at idx"),
                                _ => todo!("non-self idx in post"),
                            }
                        }
                        _ => todo!("non-arg domain in post"),
                    };

                    match ownership_tracker.object_is_owned(target_object) {
                        true => SubmissionLocation::Local,
                        false => SubmissionLocation::Remote(
                            ownership_tracker.get_remote_owner(target_object).unwrap(),
                        ),
                    }
                }
                th @ _ => todo!("unhandled host target in post ownership transfer: {:?}", th),
            };

            // skip checking target_object.node_idx and assume own arguments are always the
            // target of the object ref value
            let argument_idx = ownership_transfer.target_object.arg_idx;

            match ownership_transfer.target_object.node_idx {
                plan_definitions::TaskIndex::Spawns(spawned_task_idx) => {
                    let epic_spawned_task = committed_task
                        .spawned_tasks
                        .get_mut(spawned_task_idx)
                        .unwrap();
                    match ownership_transfer.target_object.domain {
                        plan_definitions::ObjectArgumentDomain::NoDomain => {
                            let object_to_transfer = epic_spawned_task.intent.args[argument_idx]
                                .get_object_id()
                                .expect("no object id for object argument");
                            target_objects.insert(object_to_transfer);
                            let current_owner =
                                get_current_owner!(ownership_tracker, object_to_transfer);
                            let whomstone_task = epic_spawned_task.add_pre_whomstone(argument_idx);
                            // whomstone_task.set_intent_host_idx(self_host_idx);
                            ownership_change_tasks.insert(
                                object_to_transfer.into(),
                                OwnershipInformation {
                                    target_host: target_host.clone(),
                                    current_owner: current_owner,
                                    task: whomstone_task,
                                },
                            );
                        }
                        plan_definitions::ObjectArgumentDomain::Objects => {
                            let objects_to_transfer =
                                epic_spawned_task.intent.args[argument_idx].get_object_ids();

                            if ownership_tracker.objects_are_owned(&objects_to_transfer)
                                && target_host.is_local()
                            {
                                continue;
                            }

                            // FIXME @multi-ws we need to update some definitions to make working with
                            // multi-object whomstone tasks easier, so for now we'll just to multiple
                            // single-object whomstones.
                            for object_to_transfer in &objects_to_transfer {
                                let object_to_transfer = *object_to_transfer;

                                // whomstone_task.set_intent_host_idx(self_host_idx);
                                let object_owner =
                                    match ownership_tracker.object_is_owned(object_to_transfer) {
                                        false => SubmissionLocation::Remote(
                                            ownership_tracker
                                                .get_remote_owner(object_to_transfer)
                                                .unwrap(),
                                        ),
                                        true => {
                                            if target_host.is_local() {
                                                continue;
                                            } else {
                                                target_host.clone()
                                            }
                                        }
                                    };

                                let whomstone_task = epic_spawned_task
                                    .add_pre_whomstone_for_object(IPtr::new(
                                        object_to_transfer,
                                        0,
                                        0,
                                    ));

                                ownership_change_tasks.insert(
                                    object_to_transfer.into(),
                                    OwnershipInformation {
                                        target_host: target_host.clone(),
                                        current_owner: object_owner,
                                        task: whomstone_task,
                                    },
                                );
                            }

                            target_objects.extend(objects_to_transfer);
                        }
                        _ => unreachable!(
                            "ownership transfer in pre with invalid object domain: {:?}",
                            ownership_transfer
                        ),
                    }
                }
                plan_definitions::TaskIndex::SelfIdx => {
                    todo!("self idx in post ownership transfer");
                }
                plan_definitions::TaskIndex::Idx(_other_task_idx) => {
                    todo!("lookup past executed tasks");
                }
            }
        }

        for object_move in &post_action_block.moves {
            let argument_idx = object_move.target_object.arg_idx;

            match object_move.target_object.node_idx {
                plan_definitions::TaskIndex::Spawns(spawned_task_idx) => {
                    let epic_spawned_task = committed_task
                        .spawned_tasks
                        .get_mut(spawned_task_idx)
                        .unwrap();
                    match object_move.target_object.domain {
                        plan_definitions::ObjectArgumentDomain::NoDomain => {
                            let object_to_move = epic_spawned_task.intent.args[argument_idx]
                                .get_object_id()
                                .expect("no object id for object argument");

                            if let Some(ref mut ownership_info) =
                                ownership_change_tasks.get_mut(&object_to_move.into())
                            {
                                let whomstone_task = &mut ownership_info.task;
                                // NOTE here we assume that if we have pre-tasks for an object as part
                                // of a nanotransaction entry in the plan, the target of the ownership
                                // change is the same as the target of the object move for the given
                                // object.
                                whomstone_task.include_move();
                            } else {
                                todo!("post move without corresponding ownership transfer");
                            }

                            if !target_objects.contains(&object_to_move) {
                                target_objects.insert(object_to_move);
                            }
                        }
                        plan_definitions::ObjectArgumentDomain::Objects => {
                            let objects_to_move =
                                epic_spawned_task.intent.args[argument_idx].get_object_ids();

                            // FIXME @multi-ws
                            for object_to_move in &objects_to_move {
                                let object_to_move = *object_to_move;
                                if let Some(ref mut ownership_info) =
                                    ownership_change_tasks.get_mut(&object_to_move.into())
                                {
                                    let whomstone_task = &mut ownership_info.task;
                                    // NOTE here we assume that if we have pre-tasks for an object as part
                                    // of a nanotransaction entry in the plan, the target of the ownership
                                    // change is the same as the target of the object move for the given
                                    // object.
                                    whomstone_task.include_move();
                                } else {
                                    let _object_owner =
                                        match ownership_tracker.object_is_owned(object_to_move) {
                                            false => SubmissionLocation::Remote(
                                                ownership_tracker
                                                    .get_remote_owner(object_to_move)
                                                    .unwrap(),
                                            ),
                                            true => continue,
                                        };
                                    todo!("post move without corresponding ownership transfer");
                                }

                                if !target_objects.contains(&object_to_move) {
                                    target_objects.insert(object_to_move);
                                }
                            }
                        }
                        _ => unreachable!(
                            "object move in pre with invalid object domain: {:?}",
                            object_move
                        ),
                    }
                }
                plan_definitions::TaskIndex::SelfIdx => {
                    todo!("self idx in post move");
                }
                plan_definitions::TaskIndex::Idx(_other_task_idx) => {
                    todo!("other task index in post move");
                }
            }
        }

        #[cfg(feature = "object-caching")]
        for push_copy in &post_action_block.push_copies {
            let target_host = &push_copy.specifier.target_host;
            let target_host = match target_host {
                plan_definitions::HostTarget::NextHost => next_host_id.clone(),
                plan_definitions::HostTarget::HostIdx(_idx) => {
                    todo!("HostIdx in post push_copies");
                }
                plan_definitions::HostTarget::EvalResult(built_in) => {
                    let host_target_eval_ctx = plan_definitions::HostTargetEvalCtx {
                        hosts: &ordered_hosts_list,
                        self_idx: committed_task_idx,
                        num_workers: self.num_workers,
                    };
                    let target_host = host_target_built_ins::compute_host_target(
                        &built_in,
                        &host_target_eval_ctx,
                    );

                    if let plan_definitions::HostTarget::HostIdx(host_idx) = target_host {
                        match host_idx == self_host_idx as usize {
                            false => SubmissionLocation::Remote(
                                ownership_tracker
                                    .get_host_id_for_idx(host_idx as u64)
                                    .unwrap(),
                            ),
                            true => SubmissionLocation::Local,
                        }
                    } else {
                        unreachable!();
                    }
                }
                plan_definitions::HostTarget::Owner(ref object_ref) => {
                    let target_object = match object_ref.domain {
                        plan_definitions::ObjectArgumentDomain::Arguments => {
                            match object_ref.node_idx {
                                plan_definitions::TaskIndex::SelfIdx => committed_activation
                                    .get_object_arg_at_idx(object_ref.arg_idx)
                                    .expect("failed to get object arg at idx"),
                                _ => todo!("non-self idx in post"),
                            }
                        }
                        _ => todo!("non-arg domain in post"),
                    };

                    match ownership_tracker.object_is_owned(target_object) {
                        true => SubmissionLocation::Local,
                        false => SubmissionLocation::Remote(
                            ownership_tracker.get_remote_owner(target_object).unwrap(),
                        ),
                    }
                }
                plan_definitions::HostTarget::NonOwners(ref object_ref) => {
                    let target_objects = match &object_ref.domain {
                        plan_definitions::ObjectArgumentDomain::Arguments => {
                            match object_ref.node_idx {
                                plan_definitions::TaskIndex::SelfIdx => vec![committed_activation
                                    .get_object_arg_at_idx(object_ref.arg_idx)
                                    .expect("failed to get object arg at idx")],
                                _ => todo!("non-self idx in post"),
                            }
                        }
                        plan_definitions::ObjectArgumentDomain::Objects => {
                            match object_ref.node_idx {
                                plan_definitions::TaskIndex::SelfIdx => {
                                    committed_activation.get_object_args_at_idx(object_ref.arg_idx)
                                }
                                _ => todo!("non-self idx in post"),
                            }
                        }
                        d @ _ => todo!("unhandled domain in post: {:?}", d),
                    };

                    let mut remotes_set: HashSet<HostId> = HashSet::new();
                    // FIXME this needs to also use the updated ownership map we have stored
                    // locally since the owners of the target object may have changed.
                    for target_object in target_objects.into_iter() {
                        for remote in ownership_tracker.get_non_owners(target_object) {
                            remotes_set.insert(remote);
                        }
                    }
                    SubmissionLocation::Remotes(remotes_set.into_iter().collect())
                }
            };

            // we use the cardinality of the target_host set to figure out the number of caches to
            // spawn (check arguments of update_caches), and then we add one downstream
            // `whomstone_and_move` task (as a data dependency) to the update_caches task for each
            // one of those caches. The idea is that the committed task here (that contains post
            // actions) will only notify its dependents of its completion once we've smeared the
            // object caches around.

            // skip checking target_object.node_idx and assume own arguments are always the
            // target of the object ref value
            let argument_idx = push_copy.specifier.target_object.arg_idx;
            let objects_to_transfer = committed_activation.get_object_args_at_idx(argument_idx);

            for object_to_transfer in objects_to_transfer.into_iter() {
                let original_object_owner =
                    get_current_owner!(ownership_tracker, object_to_transfer);
                let num_caches = target_host.get_cardinality();
                let mut cache_update_task =
                    epic_control::SpawnedTask::new_spawned_cache_update_task(
                        committed_task.id,
                        IPtr::new(object_to_transfer, 0, 0).into(),
                        num_caches,
                    );

                for location in target_host.into_iter() {
                    if location == original_object_owner {
                        continue;
                    }

                    let mut move_task = epic_control::SpawnedTask::new_whomstone_task(
                        committed_task.id,
                        None,
                        true,
                    );
                    let move_task_id = move_task.id;
                    move_task.set_parent_task(committed_task.id);
                    committed_task.add_notifying_task(move_task_id);
                    move_task.include_move();

                    cache_update_task.add_downstream_data_dependency(&mut move_task);
                    move_task
                        .intent
                        .args
                        .push(IPtr::new(object_to_transfer, 0, 0).into());

                    if let Some(target_location) = location.get_remote() {
                        move_task.intent.args.push(target_location.into());
                    }

                    let move_task_information = OwnershipInformation {
                        current_owner: original_object_owner.clone(),
                        target_host: location,
                        task: move_task,
                    };
                    ownership_change_tasks.insert(
                        OwnershipInformationKey::UnknownObject(move_task_id),
                        move_task_information,
                    );
                }

                match pending_task_map.get_mut(&original_object_owner) {
                    None => {
                        pending_task_map
                            .insert(original_object_owner, vec![(cache_update_task, None)]);
                    }
                    Some(e) => e.push((cache_update_task, None)),
                }
            }
        }

        (ownership_change_tasks, pending_task_map)
    }

    fn group_by_owner(
        objects: Vec<ObjectId>,
        ownership_tracker: &OwnershipTracker,
        ownership_change_tasks: &HashMap<OwnershipInformationKey, OwnershipInformation>,
    ) -> HashMap<SubmissionLocation, Vec<ObjectId>> {
        let mut groups: HashMap<SubmissionLocation, Vec<ObjectId>> =
            HashMap::with_capacity(objects.len());

        for object in &objects {
            let owner =
                match ownership_change_tasks.get(&OwnershipInformationKey::KnownObject(*object)) {
                    None => match ownership_tracker.object_is_owned(*object) {
                        true => SubmissionLocation::Local,
                        false => match ownership_tracker.get_remote_owner(*object) {
                            Some(h) => SubmissionLocation::Remote(h),
                            None => todo!(),
                        },
                    },
                    Some(oi) => oi.target_host.clone(),
                };

            groups
                .entry(owner)
                .and_modify(|os| os.push(*object))
                .or_insert(vec![*object]);
        }

        groups
    }

    // TODO speed, logging
    pub fn next_step_from_plan(
        &self,
        committed_task: &epic_control::ECB,
        plan: &plan_definitions::ParsedPhysicalPlan,
        mut ownership_change_tasks: HashMap<OwnershipInformationKey, OwnershipInformation>,
        mut pending_task_map: HashMap<
            SubmissionLocation,
            Vec<(epic_control::SpawnedTask, Option<Schedulable>)>,
        >,
    ) -> HashMap<SubmissionLocation, Vec<(epic_control::SpawnedTask, Option<Schedulable>)>> {
        let for_tasks = &committed_task.spawned_tasks;
        let ownership_tracker = OwnershipTracker::get_ownership_tracker(None);

        #[cfg(feature = "object-caching")]
        let mut object_copies: HashMap<
            (PushCopyTargetObject, SubmissionLocation),
            epic_control::SpawnedTask,
        > = HashMap::with_capacity(8);

        let mut task_location_map: HashMap<EcbId, SubmissionLocation> =
            HashMap::with_capacity(for_tasks.len());

        let self_host_idx = ownership_tracker.get_host_idx().unwrap();
        let ordered_hosts_list = ownership_tracker.get_ordered_host_list();

        let next_host_id = match ownership_tracker.get_next_host() {
            Some(h) => SubmissionLocation::Remote(h),
            None => SubmissionLocation::Local,
        };

        for task in for_tasks {
            let mut target_objects = HashSet::new();
            let mut epic_spawned_task = task.clone();

            let root_activation_id = epic_spawned_task.id.get_root_id();

            let idx_for_activation = match self.planning_progress.get_mut(&root_activation_id) {
                Some(ref mut e) => {
                    *e.value_mut() += 1;
                    *e.value()
                }
                None => {
                    // FIXME this will be messed up when we spill to other hosts
                    self.planning_progress
                        .insert(root_activation_id, committed_task.planning_context.idx + 1);
                    committed_task.planning_context.idx + 1
                }
            };

            epic_spawned_task.planning_context.idx = idx_for_activation;

            #[cfg(debug_assertions)]
            println!(
                "index for activation {}: {idx_for_activation}",
                task.intent.name
            );

            let node = match plan
                .find_node_for_intent(&epic_spawned_task.intent.name, idx_for_activation)
            {
                None => {
                    let spawned_task_meta =
                        NandoScheduler::get_nando_metadata_external(&task.intent.name).unwrap();
                    let (submission_location, schedulable) = match ownership_tracker
                        .args_are_resolvable_locally(
                            spawned_task_meta.kind.is_read_only(),
                            spawned_task_meta.mutable_argument_indices,
                            Some(&task.intent.name),
                            &mut epic_spawned_task.intent.args,
                        ) {
                        Schedulable::Immediately => {
                            (SubmissionLocation::Local, Some(Schedulable::Immediately))
                        }
                        after @ Schedulable::AfterWaitingForArrival(_) => {
                            (SubmissionLocation::Local, Some(after))
                        }
                        Schedulable::ArgsUnresolvableLocally => {
                            // FIXME ownership transfers etc.
                            let object_refs = match task.intent.is_invalidation_intent() {
                                false => task.intent.get_object_references(),
                                true => {
                                    vec![task.intent.args.get(1).unwrap().get_object_id().unwrap()]
                                }
                            };
                            match ownership_tracker.compute_activation_site(&object_refs) {
                                Some(host_id) => (SubmissionLocation::Remote(host_id), None),
                                None => (SubmissionLocation::UnselectedRemote, None),
                            }
                        }
                    };

                    task_location_map.insert(epic_spawned_task.id, submission_location.clone());
                    match pending_task_map.get_mut(&submission_location) {
                        None => {
                            pending_task_map.insert(
                                submission_location,
                                vec![(epic_spawned_task, schedulable)],
                            );
                        }
                        Some(e) => e.push((epic_spawned_task, schedulable)),
                    }

                    continue;
                }
                Some(n) => n,
            };

            // TODO @fold Check if divisible. If not then do nothing else
            if epic_spawned_task.is_fold() {
                if !epic_spawned_task.is_schedulable() {
                    pending_task_map
                        .insert(SubmissionLocation::Local, vec![(epic_spawned_task, None)]);
                    continue;
                }

                let object_references = epic_spawned_task.intent.args[1].clone();
                let object_ids = object_references.get_object_ids();
                let groups =
                    Self::group_by_owner(object_ids, ownership_tracker, &ownership_change_tasks);

                if groups.len() == 1 {
                    todo!("only one owner for fold args");
                }

                let submission_location = match node.schedule_on {
                    plan_definitions::ActivationTarget::EvalResult(ref built_in) => {
                        let host_target_eval_ctx = plan_definitions::HostTargetEvalCtx {
                            hosts: &ordered_hosts_list,
                            self_idx: idx_for_activation,
                            num_workers: self.num_workers,
                        };
                        let target_host = host_target_built_ins::compute_host_target(
                            &built_in,
                            &host_target_eval_ctx,
                        );

                        if let plan_definitions::HostTarget::HostIdx(host_idx) = target_host {
                            match host_idx == self_host_idx as usize {
                                false => SubmissionLocation::Remote(
                                    ownership_tracker
                                        .get_host_id_for_idx(host_idx as u64)
                                        .unwrap(),
                                ),
                                true => SubmissionLocation::Local,
                            }
                        } else {
                            unreachable!();
                        }
                    }
                    plan_definitions::ActivationTarget::Owner(_) => SubmissionLocation::Local,
                };

                if groups.contains_key(&submission_location) {
                    epic_spawned_task.intent.args[1] = NandoArgument::MRef(IPtrList::default());
                }

                for (location, objects) in &groups {
                    let iptr_list: Vec<IPtr> = objects
                        .into_iter()
                        .map(|oid| IPtr::new(*oid, 0, 0))
                        .collect();
                    let objects_arg = IPtrList::from_vec(iptr_list);
                    let mut unfolded_task = epic_control::SpawnedTask::new_from(&epic_spawned_task);

                    unfolded_task.intent.args.clear();

                    // force accumulator to be created
                    unfolded_task.intent.args.push(NandoArgument::get_nil());
                    unfolded_task
                        .intent
                        .args
                        .push(NandoArgument::MRef(objects_arg));

                    let tasked_unresolved_idx =
                        unfolded_task.update_fold_data_dependency(&mut epic_spawned_task);

                    if *location == submission_location {
                        task_location_map.insert(unfolded_task.id, location.clone());
                        match pending_task_map.get_mut(&location) {
                            None => {
                                pending_task_map
                                    .insert(location.clone(), vec![(unfolded_task, None)]);
                            }
                            Some(e) => e.push((unfolded_task, None)),
                        };
                        continue;
                    }

                    // TODO add unresolved object in whomstone task arg list.
                    let mut whomstone_task =
                        epic_spawned_task.add_pre_whomstone(tasked_unresolved_idx);
                    whomstone_task.include_move();
                    unfolded_task.add_downstream_data_dependency(&mut whomstone_task);

                    ownership_change_tasks.insert(
                        unfolded_task.id.into(),
                        OwnershipInformation {
                            current_owner: location.clone(),
                            target_host: submission_location.clone(),
                            task: whomstone_task,
                        },
                    );

                    task_location_map.insert(unfolded_task.id, location.clone());
                    match pending_task_map.get_mut(&location) {
                        None => {
                            pending_task_map.insert(location.clone(), vec![(unfolded_task, None)]);
                        }
                        Some(e) => e.push((unfolded_task, None)),
                    };
                }
            }

            for ownership_transfer in &node.pre_actions.ownership_transfers {
                let target_host = &ownership_transfer.target_host;
                let target_host = match target_host {
                    plan_definitions::HostTarget::NextHost => next_host_id.clone(),
                    plan_definitions::HostTarget::HostIdx(host_idx) => {
                        match *host_idx == self_host_idx as usize {
                            false => SubmissionLocation::Remote(
                                ownership_tracker
                                    .get_host_id_for_idx(*host_idx as u64)
                                    .unwrap(),
                            ),
                            true => SubmissionLocation::Local,
                        }
                    }
                    plan_definitions::HostTarget::EvalResult(built_in) => {
                        let host_target_eval_ctx = plan_definitions::HostTargetEvalCtx {
                            hosts: &ordered_hosts_list,
                            self_idx: idx_for_activation,
                            num_workers: self.num_workers,
                        };
                        let target_host = host_target_built_ins::compute_host_target(
                            &built_in,
                            &host_target_eval_ctx,
                        );

                        if let plan_definitions::HostTarget::HostIdx(host_idx) = target_host {
                            match host_idx == self_host_idx as usize {
                                false => SubmissionLocation::Remote(
                                    ownership_tracker
                                        .get_host_id_for_idx(host_idx as u64)
                                        .unwrap(),
                                ),
                                true => SubmissionLocation::Local,
                            }
                        } else {
                            unreachable!();
                        }
                    }
                    plan_definitions::HostTarget::Owner(ref object_ref) => {
                        let target_object = match object_ref.domain {
                            plan_definitions::ObjectArgumentDomain::Arguments => {
                                match object_ref.node_idx {
                                    plan_definitions::TaskIndex::SelfIdx => {
                                        epic_spawned_task.intent.args[object_ref.arg_idx]
                                            .get_object_id()
                                            .expect("no object id for object argument")
                                    }
                                    _ => todo!("non-self idx in post"),
                                }
                            }
                            _ => todo!("non-arg domain in post"),
                        };

                        match ownership_tracker.object_is_owned(target_object) {
                            true => SubmissionLocation::Local,
                            false => SubmissionLocation::Remote(
                                ownership_tracker.get_remote_owner(target_object).unwrap(),
                            ),
                        }
                    }
                    th @ _ => todo!("unhandled host target in pre ownership transfer: {:?}", th),
                };

                // skip checking target_object.node_idx and assume own arguments are always the
                // target of the object ref value
                let argument_idx = ownership_transfer.target_object.arg_idx;

                match ownership_transfer.target_object.domain {
                    plan_definitions::ObjectArgumentDomain::Arguments
                    | plan_definitions::ObjectArgumentDomain::NoDomain => {
                        let arg = &epic_spawned_task.intent.args[argument_idx];
                        let object_to_transfer = match arg.is_nil() || arg.is_unresolved() {
                            true => {
                                if arg.is_nil() {
                                    continue;
                                }

                                let Some(source_task) = arg.get_unresolved_arg_task() else {
                                    continue;
                                };
                                let source_task_location = match task_location_map.get(&source_task)
                                {
                                    Some(l) => l.clone(),
                                    None => unreachable!("upstream task location unknown"),
                                };

                                let whomstone_task =
                                    epic_spawned_task.add_pre_whomstone(argument_idx);
                                ownership_change_tasks.insert(
                                    source_task.into(),
                                    OwnershipInformation {
                                        current_owner: source_task_location,
                                        target_host,
                                        task: whomstone_task,
                                    },
                                );

                                continue;
                            }
                            false => {
                                println!("might crash {:?}", arg);
                                arg.get_object_id()
                                    .expect("no object id for object argument")
                            }
                        };

                        let current_owner =
                            get_current_owner!(ownership_tracker, object_to_transfer);
                        if current_owner == target_host {
                            continue;
                        }

                        target_objects.insert(object_to_transfer);
                        let whomstone_task = epic_spawned_task.add_pre_whomstone(argument_idx);

                        // whomstone_task.set_intent_host_idx(self_host_idx);
                        ownership_change_tasks.insert(
                            object_to_transfer.into(),
                            OwnershipInformation {
                                current_owner: current_owner,
                                target_host,
                                task: whomstone_task,
                            },
                        );
                    }
                    plan_definitions::ObjectArgumentDomain::Objects => {
                        let objects_to_transfer =
                            epic_spawned_task.intent.args[argument_idx].get_object_ids();

                        // FIXME @multi-ws we need to update some definitions to make working with
                        // multi-object whomstone tasks easier, so for now we'll just to multiple
                        // single-object whomstones.
                        for object_to_transfer in &objects_to_transfer {
                            let object_to_transfer = *object_to_transfer;
                            if ownership_tracker.object_is_owned(object_to_transfer)
                                && target_host.is_local()
                            {
                                continue;
                            }

                            let current_owner = match ownership_tracker
                                .get_remote_owner(object_to_transfer)
                            {
                                None => match ownership_tracker.object_is_owned(object_to_transfer)
                                {
                                    false => SubmissionLocation::UnselectedRemote,
                                    true => SubmissionLocation::Local,
                                },
                                Some(r) => SubmissionLocation::Remote(r),
                            };

                            if current_owner == target_host {
                                continue;
                            }

                            let whomstone_task = epic_spawned_task
                                .add_pre_whomstone_for_object(IPtr::new(object_to_transfer, 0, 0));

                            ownership_change_tasks.insert(
                                object_to_transfer.into(),
                                OwnershipInformation {
                                    current_owner: current_owner,
                                    target_host: target_host.clone(),
                                    task: whomstone_task,
                                },
                            );

                            target_objects.insert(object_to_transfer);
                        }
                    }
                    _ => unreachable!(
                        "ownership transfer in pre with invalid object domain: {:?}",
                        ownership_transfer
                    ),
                }
            }

            let activation_target_host = match node.schedule_on {
                plan_definitions::ActivationTarget::Owner(ref object_ref) => {
                    // assume ownership of all objects in refs is identical.
                    match object_ref.domain {
                        plan_definitions::ObjectArgumentDomain::Arguments
                        | plan_definitions::ObjectArgumentDomain::NoDomain => {
                            let target_object_ref = object_ref;
                            let object_to_move = epic_spawned_task.intent.args
                                [target_object_ref.arg_idx]
                                .get_object_id()
                                .expect("no object id for object argument");
                            match ownership_change_tasks.get(&object_to_move.into()) {
                                None => match ownership_tracker.object_is_owned(object_to_move) {
                                    false => {
                                        match ownership_tracker.get_remote_owner(object_to_move) {
                                            Some(h) => SubmissionLocation::Remote(h),
                                            // NOTE activation router should be able to find a location
                                            // for this by asking the scheduler for the location of the
                                            // host.
                                            None => SubmissionLocation::UnselectedRemote,
                                        }
                                    }
                                    true => SubmissionLocation::Local,
                                },
                                Some(OwnershipInformation { target_host, .. }) => {
                                    target_host.clone()
                                }
                            }
                        }
                        plan_definitions::ObjectArgumentDomain::Objects => {
                            let target_object_ref = object_ref;
                            let objects_to_move = epic_spawned_task.intent.args
                                [target_object_ref.arg_idx]
                                .get_object_ids();
                            let sample_object = objects_to_move[0];
                            match ownership_change_tasks.get(&sample_object.into()) {
                                None => match ownership_tracker.object_is_owned(sample_object) {
                                    false => {
                                        match ownership_tracker.get_remote_owner(sample_object) {
                                            Some(remote_owner) => {
                                                SubmissionLocation::Remote(remote_owner)
                                            }
                                            None => SubmissionLocation::UnselectedRemote,
                                        }
                                    }
                                    true => SubmissionLocation::Local,
                                },
                                Some(OwnershipInformation { target_host, .. }) => {
                                    target_host.clone()
                                }
                            }
                        }
                        _ => todo!(),
                    }
                }
                plan_definitions::ActivationTarget::EvalResult(ref built_in) => {
                    let host_target_eval_ctx = plan_definitions::HostTargetEvalCtx {
                        hosts: &ordered_hosts_list,
                        self_idx: idx_for_activation,
                        num_workers: self.num_workers,
                    };
                    let target_host = host_target_built_ins::compute_host_target(
                        &built_in,
                        &host_target_eval_ctx,
                    );

                    if let plan_definitions::HostTarget::HostIdx(host_idx) = target_host {
                        match host_idx == self_host_idx as usize {
                            false => SubmissionLocation::Remote(
                                ownership_tracker
                                    .get_host_id_for_idx(host_idx as u64)
                                    .unwrap(),
                            ),
                            true => SubmissionLocation::Local,
                        }
                    } else {
                        unreachable!();
                    }
                }
            };

            #[cfg(feature = "object-caching")]
            for object_copy in &node.pre_actions.push_copies {
                let src_objects: Vec<PushCopyTargetObject> = match object_copy.range {
                    plan_definitions::RangeOver::NoRange => {
                        match object_copy.specifier.target_object.domain {
                            plan_definitions::ObjectArgumentDomain::Arguments
                            | plan_definitions::ObjectArgumentDomain::Objects
                            | plan_definitions::ObjectArgumentDomain::NoDomain => {
                                let argument_idx = object_copy.specifier.target_object.arg_idx;
                                match epic_spawned_task.intent.args[argument_idx].is_unresolved() {
                                    false => epic_spawned_task.intent.args[argument_idx]
                                        .get_object_ids()
                                        .into_iter()
                                        .map(|oid| PushCopyTargetObject::ResolvedObject(oid))
                                        .collect(),
                                    true => {
                                        let arg = &epic_spawned_task.intent.args[argument_idx];
                                        vec![PushCopyTargetObject::UnresolvedObject(
                                            arg.get_unresolved_arg_idx().unwrap(),
                                            arg.get_unresolved_arg_task(),
                                        )]
                                    }
                                }
                            }
                            _ => todo!(),
                        }
                    }
                    _ => todo!(),
                };

                if src_objects.is_empty() {
                    continue;
                }

                let copy_target = match object_copy.specifier.target_host {
                    plan_definitions::HostTarget::NextHost => next_host_id.clone(),
                    plan_definitions::HostTarget::Owner(ref object_ref) => {
                        let target_object = match object_ref.domain {
                            plan_definitions::ObjectArgumentDomain::Arguments => {
                                match object_ref.node_idx {
                                    plan_definitions::TaskIndex::SelfIdx => {
                                        epic_spawned_task.intent.args[object_ref.arg_idx]
                                            .get_object_id()
                                            .expect("no object id for object argument")
                                    }
                                    _ => todo!("non-self idx in post"),
                                }
                            }
                            _ => todo!("non-arg domain in post"),
                        };

                        match ownership_tracker.object_is_owned(target_object) {
                            true => SubmissionLocation::Local,
                            false => SubmissionLocation::Remote(
                                ownership_tracker.get_remote_owner(target_object).unwrap(),
                            ),
                        }
                    }
                    plan_definitions::HostTarget::NonOwners(_) => {
                        let src_object = src_objects
                            .get(0)
                            .cloned()
                            .unwrap()
                            .get_object_id()
                            .expect("cannot compute non-owner of unresolved object");
                        SubmissionLocation::Remotes(ownership_tracker.get_non_owners(src_object))
                    }
                    plan_definitions::HostTarget::EvalResult(ref built_in) => {
                        let host_target_eval_ctx = plan_definitions::HostTargetEvalCtx {
                            hosts: &ordered_hosts_list,
                            self_idx: idx_for_activation,
                            num_workers: self.num_workers,
                        };
                        let target_host = host_target_built_ins::compute_host_target(
                            &built_in,
                            &host_target_eval_ctx,
                        );

                        if let plan_definitions::HostTarget::HostIdx(host_idx) = target_host {
                            match host_idx == self_host_idx as usize {
                                false => SubmissionLocation::Remote(
                                    ownership_tracker
                                        .get_host_id_for_idx(host_idx as u64)
                                        .unwrap(),
                                ),
                                true => SubmissionLocation::Local,
                            }
                        } else {
                            unreachable!();
                        }
                    }
                    _ => todo!(
                        "push_copy target host {:?}",
                        object_copy.specifier.target_host
                    ),
                };

                for src_object in src_objects.into_iter() {
                    match src_object {
                        ref s @ PushCopyTargetObject::ResolvedObject(src_object) => {
                            let original_object_owner =
                                get_current_owner!(ownership_tracker, src_object);
                            if original_object_owner == copy_target {
                                continue;
                            }

                            match object_copies.get_mut(&(*s, copy_target.clone())) {
                                Some(push_copy_task) => {
                                    push_copy_task.add_new_control_dependency(epic_spawned_task.id);
                                    epic_spawned_task
                                        .add_new_upstream_dependency(push_copy_task.id);
                                }
                                None => {
                                    let mut spawn_cache_task =
                                        epic_control::SpawnedTask::new_spawn_cache_task(
                                            committed_task.id,
                                            IPtr::new(src_object, 0, 0).into(),
                                            0,
                                        );
                                    let mut move_task =
                                        epic_control::SpawnedTask::new_whomstone_task(
                                            committed_task.id,
                                            None,
                                            true,
                                        );
                                    move_task.include_move();

                                    if let Some(copy_target_remote) = copy_target.get_remote() {
                                        match ownership_tracker.get_cache_id_for_object_host_pair(
                                            src_object,
                                            copy_target_remote,
                                        ) {
                                            None => {
                                                spawn_cache_task
                                                    .add_downstream_data_dependency(&mut move_task);
                                            }
                                            Some(c) => {
                                                spawn_cache_task.intent.args.push(c.into());
                                                move_task
                                                    .intent
                                                    .args
                                                    .push(IPtr::new(c, 0, 0).into());
                                                spawn_cache_task
                                                    .add_new_control_dependency(move_task.id);
                                                move_task.add_new_upstream_dependency(
                                                    spawn_cache_task.id,
                                                );
                                            }
                                        }
                                    } else {
                                        spawn_cache_task
                                            .add_downstream_data_dependency(&mut move_task);
                                    }

                                    move_task
                                        .intent
                                        .args
                                        .push(IPtr::new(src_object, 0, 0).into());

                                    move_task.add_new_control_dependency(epic_spawned_task.id);
                                    epic_spawned_task.add_new_upstream_dependency(move_task.id);

                                    task_location_map
                                        .insert(spawn_cache_task.id, original_object_owner.clone());
                                    match pending_task_map.get_mut(&original_object_owner) {
                                        None => {
                                            pending_task_map.insert(
                                                original_object_owner,
                                                vec![(spawn_cache_task, None)],
                                            );
                                        }
                                        Some(e) => e.push((spawn_cache_task, None)),
                                    }

                                    object_copies
                                        .insert((s.clone(), copy_target.clone()), move_task);
                                }
                            }
                        }
                        PushCopyTargetObject::UnresolvedObject(arg_idx, Some(task_id)) => {
                            let upstream_task_location = match task_location_map.get(&task_id) {
                                Some(l) => {
                                    if *l == copy_target {
                                        continue;
                                    }

                                    l.clone()
                                }
                                None => unreachable!("upstream task location unknown"),
                            };

                            // NOTE to make the arg idx not matter for lookup
                            let lookup_key =
                                PushCopyTargetObject::UnresolvedObject(0, Some(task_id));
                            match object_copies.get_mut(&(lookup_key, copy_target.clone())) {
                                Some(push_copy_task) => {
                                    push_copy_task.add_downstream_data_dependency_at_idx(
                                        &mut epic_spawned_task,
                                        arg_idx,
                                    );

                                    match pending_task_map.get_mut(&upstream_task_location) {
                                        Some(e) => {
                                            for (task, _) in e.iter_mut() {
                                                if task.id != task_id {
                                                    continue;
                                                }
                                                task.remove_downstream_dependency(
                                                    epic_spawned_task.id,
                                                );
                                                break;
                                            }
                                        }
                                        None => unreachable!(),
                                    }
                                }
                                None => {
                                    let mut spawn_cache_task =
                                        epic_control::SpawnedTask::new_spawn_cache_task(
                                            committed_task.id,
                                            NandoArgument::TaskedUnresolvedArgument((task_id, 0)),
                                            0,
                                        );
                                    let mut move_task =
                                        epic_control::SpawnedTask::new_whomstone_task(
                                            committed_task.id,
                                            None,
                                            true,
                                        );
                                    move_task.include_move();

                                    spawn_cache_task.add_downstream_data_dependency(&mut move_task);
                                    move_task.add_downstream_data_dependency_at_idx(
                                        &mut epic_spawned_task,
                                        arg_idx,
                                    );

                                    match pending_task_map.get_mut(&upstream_task_location) {
                                        Some(e) => {
                                            for (task, _) in e.iter_mut() {
                                                if task.id != task_id {
                                                    continue;
                                                }

                                                task.add_downstream_data_dependency_with_arg_idx(
                                                    spawn_cache_task.id,
                                                    0,
                                                );
                                                task.remove_downstream_dependency(
                                                    epic_spawned_task.id,
                                                );

                                                // NOTE to append the original object id
                                                task.add_downstream_data_dependency(&mut move_task);

                                                break;
                                            }

                                            e.push((spawn_cache_task, None))
                                        }
                                        None => unreachable!(),
                                    }

                                    object_copies
                                        .insert((lookup_key, copy_target.clone()), move_task);
                                }
                            }
                        }
                        PushCopyTargetObject::UnresolvedObject(_arg_idx, None) => {
                            unreachable!("non-tasked unresolved arg in push-copy")
                        }
                    }
                }
            }

            #[cfg(not(feature = "object-caching"))]
            if !node.pre_actions.push_copies.is_empty() {
                /*
                eprintln!(
                    "Ignoring copy tasks in {:?}, object-caching feature flag is not enabled",
                    node
                );
                */
            }

            for object_move in &node.pre_actions.moves {
                let argument_idx = object_move.target_object.arg_idx;

                match object_move.target_object.domain {
                    plan_definitions::ObjectArgumentDomain::Arguments
                    | plan_definitions::ObjectArgumentDomain::NoDomain => {
                        let arg = &epic_spawned_task.intent.args[argument_idx];
                        let object_to_move = match arg.is_nil() || arg.is_unresolved() {
                            true => {
                                if arg.is_nil() {
                                    continue;
                                }

                                let Some(source_task) = arg.get_unresolved_arg_task() else {
                                    continue;
                                };

                                match task_location_map.get(&source_task) {
                                    Some(l) => {
                                        if *l == activation_target_host {
                                            let _ =
                                                ownership_change_tasks.remove(&source_task.into());
                                            continue;
                                        }

                                        source_task.into()
                                    }
                                    None => unreachable!("upstream task location unknown"),
                                }
                            }
                            false => arg
                                .get_object_id()
                                .expect("no object id for object argument")
                                .into(),
                        };

                        // FIXME for tasked unresolved objects this should also probably include
                        // the target ids, but for now it should work.
                        if let Some(ref mut ownership_info) =
                            ownership_change_tasks.get_mut(&object_to_move)
                        {
                            let mut whomstone_task = &mut ownership_info.task;
                            // NOTE here we assume that if we have pre-tasks for an object as part
                            // of a nanotransaction entry in the plan, the target of the ownership
                            // change is the same as the target of the object move for the given
                            // object.
                            whomstone_task.include_move();
                            if arg.is_unresolved() {
                                let Some(task_id) = object_to_move.get_task_dep_if_unknown() else {
                                    unreachable!();
                                };
                                let Some(upstream_task_location) = task_location_map.get(&task_id)
                                else {
                                    unreachable!();
                                };

                                match pending_task_map.get_mut(&upstream_task_location) {
                                    Some(e) => {
                                        for (task, _) in e.iter_mut() {
                                            if task.id != task_id {
                                                continue;
                                            }

                                            task.add_downstream_data_dependency(
                                                &mut whomstone_task,
                                            );

                                            break;
                                        }
                                    }
                                    None => unreachable!(),
                                }
                            }
                        } else {
                            let Some(object_id) = object_to_move.get_object_if_known() else {
                                unreachable!();
                            };

                            if ownership_tracker.object_is_owned(object_id) {
                                continue;
                            }
                            todo!("move without ownership transfer in pre");
                        }

                        if let Some(object_to_move) = object_to_move.get_object_if_known() {
                            if !target_objects.contains(&object_to_move) {
                                target_objects.insert(object_to_move);
                            }
                        }
                    }
                    plan_definitions::ObjectArgumentDomain::Objects => {
                        let objects_to_move =
                            epic_spawned_task.intent.args[argument_idx].get_object_ids();

                        // FIXME @multi-ws
                        for object_to_move in &objects_to_move {
                            let object_to_move = *object_to_move;

                            if ownership_tracker.object_is_owned(object_to_move)
                                && activation_target_host.is_local()
                            {
                                continue;
                            }

                            if let Some(ref mut ownership_info) =
                                ownership_change_tasks.get_mut(&object_to_move.into())
                            {
                                let whomstone_task = &mut ownership_info.task;
                                // NOTE here we assume that if we have pre-tasks for an object as part
                                // of a nanotransaction entry in the plan, the target of the ownership
                                // change is the same as the target of the object move for the given
                                // object.
                                whomstone_task.include_move();
                            } else {
                                let current_owner = match ownership_tracker
                                    .get_remote_owner(object_to_move)
                                {
                                    None => match ownership_tracker.object_is_owned(object_to_move)
                                    {
                                        false => SubmissionLocation::UnselectedRemote,
                                        true => SubmissionLocation::Local,
                                    },
                                    Some(r) => SubmissionLocation::Remote(r),
                                };

                                if activation_target_host == current_owner {
                                    continue;
                                }

                                todo!("move without corresponding ownership transfer");
                            }

                            if !target_objects.contains(&object_to_move) {
                                target_objects.insert(object_to_move);
                            }
                        }
                    }
                    _ => unreachable!(
                        "object move in pre with invalid object domain: {:?}",
                        object_move
                    ),
                }
            }

            for target_object in target_objects {
                let locatics_task = ownership_change_tasks
                    .remove(&target_object.into())
                    .expect("no stored info for processed object");

                let task_owner = match locatics_task.current_owner.is_local() {
                    true => &locatics_task.target_host,
                    false => &locatics_task.current_owner,
                };
                match pending_task_map.get_mut(task_owner) {
                    None => {
                        task_location_map.insert(locatics_task.task.id, task_owner.clone());
                        pending_task_map
                            .insert(task_owner.clone(), vec![(locatics_task.task, None)]);
                    }
                    Some(e) => e.push((locatics_task.task, None)),
                }
            }

            #[cfg(not(feature = "object-caching"))]
            let schedulable = None;

            #[cfg(feature = "object-caching")]
            let schedulable = {
                let spawned_task_meta =
                    NandoScheduler::get_nando_metadata_external(&epic_spawned_task.intent.name)
                        .unwrap();
                let schedulable =
                    match activation_target_host.is_local() && epic_spawned_task.is_schedulable() {
                        true => Some(ownership_tracker.args_are_resolvable_locally(
                            spawned_task_meta.kind.is_read_only(),
                            spawned_task_meta.mutable_argument_indices,
                            Some(&epic_spawned_task.intent.name),
                            &mut epic_spawned_task.intent.args,
                        )),
                        false => None,
                    };

                if committed_task.should_mask_invalidations()
                    || spawned_task_meta.invalidate_on_completion.is_some()
                {
                    epic_spawned_task.mask_invalidations = true;
                }

                schedulable
            };

            if epic_spawned_task
                .intent
                .name
                .contains("inter_partition_rank_update")
            {
                println!("check {:?}: {:#?}", schedulable, epic_spawned_task);
            }

            task_location_map.insert(epic_spawned_task.id, activation_target_host.clone());
            match pending_task_map.get_mut(&activation_target_host) {
                None => {
                    pending_task_map.insert(
                        activation_target_host,
                        vec![(epic_spawned_task, schedulable)],
                    );
                }
                Some(e) => e.push((epic_spawned_task, schedulable)),
            };
        }

        #[cfg(feature = "object-caching")]
        for ((src_object, _), move_task) in object_copies.into_iter() {
            let move_task_location = match src_object {
                PushCopyTargetObject::ResolvedObject(src_object) => {
                    get_current_owner!(ownership_tracker, src_object)
                }
                PushCopyTargetObject::UnresolvedObject(_arg_idx, Some(task_id)) => {
                    match task_location_map.get(&task_id) {
                        Some(l) => l.clone(),
                        None => unreachable!("upstream task location unknown"),
                    }
                }
                _ => unreachable!(),
            };

            match pending_task_map.get_mut(&move_task_location) {
                None => {
                    pending_task_map.insert(move_task_location, vec![(move_task, None)]);
                }
                Some(e) => e.push((move_task, None)),
            }
        }

        // Place any leftover ownership transfer tasks into appropriate collection.
        for (_, locatics_task) in ownership_change_tasks.into_iter() {
            // FIXME @fold if the current owner is the current host, we want to actually
            // schedule this under the "target_host" to make sure that this is handled locally
            // by the activation router
            match pending_task_map.get_mut(&locatics_task.current_owner) {
                None => {
                    pending_task_map.insert(
                        locatics_task.current_owner,
                        vec![(locatics_task.task, None)],
                    );
                }
                Some(e) => e.push((locatics_task.task, None)),
            }
        }

        // Prepend spawn_cache tasks
        for (location, tasks) in pending_task_map.iter_mut() {
            if location.is_local() {
                // we will reorder these tasks in the local scheduler
                continue;
            }

            let tmp_vec: Vec<(epic_control::SpawnedTask, Option<Schedulable>)> =
                tasks.drain(..).collect();
            let (cache_tasks, non_cache_tasks): (
                Vec<(epic_control::SpawnedTask, Option<Schedulable>)>,
                Vec<(epic_control::SpawnedTask, Option<Schedulable>)>,
            ) = tmp_vec
                .into_iter()
                .partition(|t| t.0.intent.is_spawn_cache_intent());
            tasks.extend(cache_tasks);
            tasks.extend(non_cache_tasks);
        }

        pending_task_map
    }

    pub fn next_step_without_plan(
        &self,
        committed_task_ecb: &mut epic_control::ECB,
        at_capacity: bool,
    ) -> HashMap<SubmissionLocation, Vec<(epic_control::SpawnedTask, Option<Schedulable>)>> {
        let ownership_tracker = OwnershipTracker::get_ownership_tracker(None);

        let mut unprocessed_downstream_data_dependencies: HashMap<EcbId, HashSet<EcbId>> =
            HashMap::with_capacity(8);
        let mut assigned_task_locations: HashMap<EcbId, SubmissionLocation> =
            HashMap::with_capacity(8);
        let mut pending_task_map: HashMap<
            SubmissionLocation,
            Vec<(epic_control::SpawnedTask, Option<Schedulable>)>,
        > = HashMap::with_capacity(8);

        // FIXME this consumes spawned task in ecb making them invisible post-facto if all we have
        // is the top-level ecb
        for mut spawned_task in committed_task_ecb.spawned_tasks.drain(..) {
            #[cfg(feature = "object-caching")]
            {
                if spawned_task.intent.is_invalidation_intent() {
                    // Invalidations should always be forwarded, as the owning host should
                    // not have any live caches of the target object.
                    let invalidation_target = spawned_task
                        .intent
                        .args
                        .get(1)
                        .unwrap()
                        .get_object_id()
                        .expect("no object id available for invalidation target");
                    let cache_owner = get_current_owner!(ownership_tracker, invalidation_target);

                    assign_to!(
                        pending_task_map,
                        spawned_task,
                        None,
                        cache_owner,
                        assigned_task_locations
                    );
                    continue;
                }

                if committed_task_ecb.mask_invalidations && !spawned_task.mask_invalidations {
                    #[cfg(debug_assertions)]
                    println!(
                        "Found a spawned task without masked invalidations: {}",
                        spawned_task.intent.name
                    );
                    spawned_task.mask_invalidations = true;
                }
            }

            for downstream_dependency in &spawned_task.downstream_dependents {
                if !downstream_dependency.is_data_dependency() {
                    continue;
                }

                unprocessed_downstream_data_dependencies
                    .entry(downstream_dependency.get_inner_ecb_id().unwrap())
                    .and_modify(|es| {
                        es.insert(spawned_task.id);
                    })
                    .or_insert_with(|| {
                        let mut set = HashSet::with_capacity(4);
                        set.insert(spawned_task.id);

                        set
                    });
            }

            if !spawned_task.is_schedulable() {
                // in case of a spawned task with both data and control dependencies, we prefer to
                // consider the data dependencies as they might imply data movement (control
                // dependencies should be "ligher-weight").
                let maybe_data_dependencies =
                    unprocessed_downstream_data_dependencies.remove(&spawned_task.id);
                let dependency_iterator = match maybe_data_dependencies {
                    None => spawned_task.upstream_control_dependencies.iter(),
                    Some(ref deps) => deps.iter(),
                };

                let submission_location = {
                    let mut location = None;
                    let mut should_submit_locally = true;

                    if spawned_task.intent.is_set_caching_permissible() {
                        // to follow the appropriate path in the subsequent match
                        should_submit_locally = true;
                    } else {
                        for task_id in dependency_iterator {
                            let dependency_location = assigned_task_locations
                                .get(task_id)
                                .expect("unknown dependency");
                            match location {
                                None => {
                                    location = Some(dependency_location);
                                    should_submit_locally = dependency_location.is_local();
                                }
                                Some(l) => {
                                    if l != dependency_location {
                                        // will be parked locally
                                        should_submit_locally = true;
                                        break;
                                    }
                                }
                            }
                        }
                    }

                    match should_submit_locally {
                        #[cfg(not(feature = "object-caching"))]
                        true => SubmissionLocation::Local,
                        #[cfg(feature = "object-caching")]
                        true => match spawned_task.intent.is_set_caching_permissible() {
                            false => SubmissionLocation::Local,
                            true => {
                                let target_object = spawned_task
                                    .intent
                                    .args
                                    .get(0)
                                    .unwrap()
                                    .get_object_id()
                                    .expect(
                                        "no object id in object arg of set_caching_permissible",
                                    );
                                get_current_owner!(ownership_tracker, target_object)
                            }
                        },
                        false => location.unwrap().clone(),
                    }
                };

                #[cfg(debug_assertions)]
                println!(
                    "Will submit '{}' ({:?}) to {:?}",
                    spawned_task.intent.name, spawned_task.id, submission_location,
                );

                assign_to!(
                    pending_task_map,
                    spawned_task,
                    None,
                    submission_location,
                    assigned_task_locations
                );
                continue;
            }

            let spawned_task_meta =
                NandoScheduler::get_nando_metadata_external(&spawned_task.intent.name).unwrap();

            #[cfg(feature = "object-caching")]
            if !spawned_task.mask_invalidations
                && (spawned_task_meta.invalidate_on_completion.is_some()
                    || committed_task_ecb.mask_invalidations)
            {
                spawned_task.mask_invalidations = true;
            }

            // NOTE the below logic is what we will eventually replace with some policy that relies
            // on a cost model.
            let task_is_schedulable = ownership_tracker.args_are_resolvable_locally(
                spawned_task_meta.kind.is_read_only(),
                spawned_task_meta.mutable_argument_indices,
                Some(&spawned_task.intent.name),
                &mut spawned_task.intent.args,
            );

            match &task_is_schedulable {
                Schedulable::Immediately => {
                    if !at_capacity {
                        #[cfg(debug_assertions)]
                        println!(
                            "Will submit '{}' ({:?}) to {:?}",
                            spawned_task.intent.name,
                            spawned_task.id,
                            SubmissionLocation::Local,
                        );

                        assign_to!(
                            pending_task_map,
                            spawned_task,
                            Some(task_is_schedulable),
                            SubmissionLocation::Local,
                            assigned_task_locations
                        );
                    } else {
                        // we're at capacity, should add object moves towards target host.
                        // NOTE this is where policy comes in. For now we'll just choose the next host as
                        // the target, but we should have something more sophisticated.
                        let target_host_location = match ownership_tracker.get_next_host() {
                            Some(h) => SubmissionLocation::Remote(h),
                            None => SubmissionLocation::Local,
                        };

                        let object_argument_indices = spawned_task.intent.get_object_arg_indices();
                        for argument_idx in object_argument_indices {
                            let mut whomstone_task = spawned_task.add_pre_whomstone(argument_idx);
                            whomstone_task.include_move();

                            assign_to!(
                                pending_task_map,
                                whomstone_task,
                                None,
                                target_host_location.clone(),
                                assigned_task_locations
                            );
                        }

                        assign_to!(
                            pending_task_map,
                            spawned_task,
                            None,
                            target_host_location,
                            assigned_task_locations
                        );
                    }
                }
                Schedulable::ArgsUnresolvableLocally => {
                    // we don't have dependencies locally, should push to owner and also make sure all
                    // dependencies are available on the target host
                    let object_refs = spawned_task.intent.get_object_references();
                    let submission_location =
                        match ownership_tracker.compute_activation_site(&object_refs) {
                            Some(host_id) => SubmissionLocation::Remote(host_id),
                            None => todo!(
                                "could not find an activation site for task '{}'",
                                spawned_task.intent.name
                            ),
                        };

                    // TODO also add potential movement tasks for objects we own.

                    assign_to!(
                        pending_task_map,
                        spawned_task,
                        None,
                        submission_location,
                        assigned_task_locations
                    );
                }
                Schedulable::AfterWaitingForArrival(_) => {
                    assign_to!(
                        pending_task_map,
                        spawned_task,
                        Some(task_is_schedulable),
                        SubmissionLocation::Local,
                        assigned_task_locations
                    );
                }
            }
        }

        pending_task_map
    }
}
