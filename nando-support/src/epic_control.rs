#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use parking_lot::RwLock;

use crate::{
    activation_id::ActivationId,
    activation_intent::{NandoActivationIntent, NandoArgument, NandoResult},
    ecb_id::EcbId,
    iptr::IPtr,
    ArgumentIdx, HostIdx,
};

use crate::ObjectId;
#[cfg(feature = "object-caching")]
use crate::ObjectVersion;

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum TaskStatus {
    Pending,
    PendingUnresolvedDependencies,
    InProgress,
    Success,
    Failure,
}

impl TaskStatus {
    pub fn is_success(&self) -> bool {
        match self {
            Self::Success => true,
            _ => false,
        }
    }

    pub fn is_failed(&self) -> bool {
        match self {
            Self::Failure => true,
            _ => false,
        }
    }
}

#[derive(Clone, Debug)]
pub enum DependencyRef {
    Ptr(Arc<RwLock<ParkableControlBlock>>),
    EcbRef(EcbId),
}

impl DependencyRef {
    // FIXME why does this return an option?
    pub fn get_inner_ecb_id(&self) -> Option<EcbId> {
        match self {
            Self::EcbRef(parent_ecb_id) => Some(*parent_ecb_id),
            Self::Ptr(parent_ptr) => {
                let parent = parent_ptr.read();
                Some(parent.control_block.id)
            }
        }
    }

    pub fn is_ptr(&self) -> bool {
        match self {
            Self::Ptr(_) => true,
            _ => false,
        }
    }
}

#[derive(Clone, Debug)]
pub enum DownstreamTaskDependency {
    None,
    DataDependency(DependencyRef, ArgumentIdx),
    ControlDependency(DependencyRef),
    ParentDependency(DependencyRef),
}

impl DownstreamTaskDependency {
    pub fn is_none(&self) -> bool {
        match self {
            Self::None => true,
            _ => false,
        }
    }

    pub fn is_parent_dependency(&self) -> bool {
        match self {
            Self::ParentDependency(_) => true,
            _ => false,
        }
    }

    pub fn is_data_dependency(&self) -> bool {
        match self {
            Self::DataDependency(_, _) => true,
            _ => false,
        }
    }

    pub fn get_inner_ecb_id(&self) -> Option<EcbId> {
        match self {
            Self::None => None,
            Self::DataDependency(dep_ref, ..)
            | Self::ControlDependency(dep_ref)
            | Self::ParentDependency(dep_ref) => dep_ref.get_inner_ecb_id(),
        }
    }

    pub fn parent_dependency_from(ecb_id: EcbId) -> Self {
        Self::ParentDependency(DependencyRef::EcbRef(ecb_id))
    }

    pub fn control_dependency_from(ecb_id: EcbId) -> Self {
        Self::ControlDependency(DependencyRef::EcbRef(ecb_id))
    }

    pub fn upgrade_to_ptr(&mut self, control_info: Arc<RwLock<ParkableControlBlock>>) {
        let data_dep_idx = match self {
            DownstreamTaskDependency::DataDependency(_, idx) => Some(*idx),
            _ => None,
        };

        *self = match data_dep_idx {
            Some(idx) => {
                DownstreamTaskDependency::DataDependency(DependencyRef::Ptr(control_info), idx)
            }
            None => DownstreamTaskDependency::ControlDependency(DependencyRef::Ptr(control_info)),
        };
    }

    pub fn is_ptr(&self) -> bool {
        match self {
            Self::DataDependency(ref dep_ref, _)
            | Self::ControlDependency(ref dep_ref)
            | Self::ParentDependency(ref dep_ref) => dep_ref.is_ptr(),
            Self::None => false,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct PlanningContext {
    pub idx: usize,

    // empty if we are not following a pre-baked physical plan
    pub plan_key: String,
}

impl PlanningContext {
    pub fn is_default(&self) -> bool {
        self.plan_key.is_empty()
    }

    fn unindexed_from_existing(&self) -> Self {
        Self {
            idx: 0,
            plan_key: self.plan_key.clone(),
        }
    }
}

#[derive(Clone, Debug)]
pub enum CombinatorContext {
    NoCombinator,
    Fold { can_subdivide: bool },
}

#[derive(Clone, Debug)]
pub struct SpawnedTask {
    pub id: EcbId,

    pub intent: NandoActivationIntent,
    pub parent_task: DownstreamTaskDependency,
    pub downstream_dependents: Vec<DownstreamTaskDependency>,
    pub upstream_control_dependencies: HashSet<EcbId>,

    #[cfg(feature = "object-caching")]
    pub mask_invalidations: bool,

    pub planning_context: PlanningContext,

    pub combinator_context: CombinatorContext,
}

impl SpawnedTask {
    pub fn new_top_level(id: EcbId, intent: &NandoActivationIntent) -> Self {
        Self {
            id,
            intent: intent.clone(),
            parent_task: DownstreamTaskDependency::None,
            downstream_dependents: Vec::default(),
            upstream_control_dependencies: HashSet::default(),

            #[cfg(feature = "object-caching")]
            mask_invalidations: false,

            planning_context: PlanningContext::default(),

            combinator_context: CombinatorContext::NoCombinator,
        }
    }

    pub fn new_from(other: &Self) -> Self {
        let id = {
            let activation_id = ActivationId::new_subtxn(&other.get_associated_activation_id());
            EcbId::new(other.id.get_host_idx(), activation_id)
        };
        let mut planning_context = other.planning_context.clone();
        planning_context.idx = 0;

        Self {
            id,
            intent: other.intent.clone(),
            parent_task: other.parent_task.clone(),
            downstream_dependents: Vec::default(),
            upstream_control_dependencies: HashSet::default(),

            #[cfg(feature = "object-caching")]
            mask_invalidations: false,

            planning_context,

            combinator_context: other.combinator_context.clone(),
        }
    }

    pub fn get_associated_activation_id(&self) -> ActivationId {
        self.id.get_activation_id()
    }

    pub fn has_pending_control_dependencies(&self) -> bool {
        !self.upstream_control_dependencies.is_empty()
    }

    pub fn get_parent_as_downstream_dependency(&self) -> DownstreamTaskDependency {
        self.parent_task.clone()
    }

    pub fn set_parent_task(&mut self, parent_task: EcbId) {
        self.parent_task =
            DownstreamTaskDependency::ParentDependency(DependencyRef::EcbRef(parent_task));
    }

    pub fn into_ecb(self) -> ECB {
        let sub_ecb_id = self.id;
        ECB {
            id: sub_ecb_id,
            spawned_tasks: vec![],
            result_task: None,
            notifying_tasks: HashSet::default(),
            parent_ecb: self.parent_task,
            downstream_dependents: self.downstream_dependents,
            result: None,
            upstream_control_dependencies: self.upstream_control_dependencies,
            #[cfg(feature = "object-caching")]
            mask_invalidations: self.mask_invalidations,

            planning_context: self.planning_context,
        }
    }

    #[cfg(feature = "object-caching")]
    pub fn should_mask_invalidations(&self) -> bool {
        self.mask_invalidations
    }

    #[inline]
    pub fn should_notify_parent(&self) -> bool {
        self.downstream_dependents.is_empty()
    }

    #[inline]
    pub fn is_schedulable(&self) -> bool {
        !(self.has_pending_control_dependencies() || self.intent.has_unresolved_args())
    }

    #[inline]
    pub fn new_whomstone_task(
        spawner_id: EcbId,
        object_arg: Option<NandoArgument>,
        is_cache: bool,
    ) -> Self {
        let activation_id = ActivationId::new_subtxn(&spawner_id.get_activation_id());
        let sub_ecb_id = EcbId::new(spawner_id.get_host_idx(), activation_id);

        let args = match object_arg {
            None => Vec::default(),
            Some(oa) => vec![oa],
        };

        let intent_name = match is_cache {
            false => "whomstone",
            true => "cache_whomstone",
        };

        Self {
            id: sub_ecb_id,
            intent: NandoActivationIntent {
                host_idx: None,
                name: intent_name.to_string(),
                args,
            },
            parent_task: DownstreamTaskDependency::ParentDependency(DependencyRef::EcbRef(
                spawner_id,
            )),
            downstream_dependents: Vec::default(),
            upstream_control_dependencies: HashSet::default(),

            #[cfg(feature = "object-caching")]
            mask_invalidations: false,

            planning_context: PlanningContext::default(),

            combinator_context: CombinatorContext::NoCombinator,
        }
    }

    #[inline]
    pub fn new_whomstone_multi_task(spawner_id: EcbId, object_refs_arg: NandoArgument) -> Self {
        let activation_id = ActivationId::new_subtxn(&spawner_id.get_activation_id());
        let sub_ecb_id = EcbId::new(spawner_id.get_host_idx(), activation_id);

        Self {
            id: sub_ecb_id,
            intent: NandoActivationIntent {
                host_idx: None,
                name: "whomstone_multi".to_string(),
                args: vec![object_refs_arg],
            },
            parent_task: DownstreamTaskDependency::None,
            downstream_dependents: Vec::default(),
            upstream_control_dependencies: HashSet::default(),

            #[cfg(feature = "object-caching")]
            mask_invalidations: false,

            planning_context: PlanningContext::default(),

            combinator_context: CombinatorContext::NoCombinator,
        }
    }

    pub fn include_move(&mut self) {
        assert!(self.intent.is_whomstone_intent());
        self.intent.name.push_str("_and_move");
    }

    pub fn add_pre_whomstone(&mut self, arg_idx: usize) -> SpawnedTask {
        let spawner_id = match self.parent_task.get_inner_ecb_id() {
            None => self.id,
            Some(id) => id,
        };

        let mut whomstone_task = match self.intent.args.get(arg_idx) {
            None => unreachable!(
                "invalid idx {arg_idx} for intent {} with {} args",
                self.intent.name,
                self.intent.args.len()
            ),
            o @ Some(NandoArgument::Ref(_)) => {
                Self::new_whomstone_task(spawner_id, o.cloned(), false)
            }
            Some(os @ NandoArgument::MRef(_)) => {
                Self::new_whomstone_multi_task(self.id, os.clone())
            }
            Some(NandoArgument::TaskedUnresolvedArgument((_, _))) => {
                Self::new_whomstone_task(spawner_id, None, false)
            }
            Some(_) => unreachable!(
                "non object arg at {arg_idx} for intent {}: {:#?}",
                self.intent.name, self.intent
            ),
        };

        self.upstream_control_dependencies.insert(whomstone_task.id);
        whomstone_task
            .downstream_dependents
            .push(DownstreamTaskDependency::ControlDependency(
                DependencyRef::EcbRef(self.id),
            ));

        whomstone_task
    }

    pub fn add_pre_whomstone_for_object(&mut self, object_iptr: IPtr) -> SpawnedTask {
        let spawner_id = match self.parent_task.get_inner_ecb_id() {
            None => self.id,
            Some(id) => id,
        };
        let mut whomstone_task =
            Self::new_whomstone_task(spawner_id, Some((&object_iptr).into()), false);

        self.upstream_control_dependencies.insert(whomstone_task.id);
        whomstone_task
            .downstream_dependents
            .push(DownstreamTaskDependency::ControlDependency(
                DependencyRef::EcbRef(self.id),
            ));

        whomstone_task
    }

    pub fn add_new_control_dependency(&mut self, control_dependency: EcbId) {
        self.downstream_dependents
            .push(DownstreamTaskDependency::ControlDependency(
                DependencyRef::EcbRef(control_dependency),
            ));
    }

    pub fn add_new_upstream_dependency(&mut self, upstream_dependency: EcbId) {
        self.upstream_control_dependencies
            .insert(upstream_dependency);
    }

    pub fn remove_upstream_dependency(&mut self, upstream_dependency: &EcbId) {
        self.upstream_control_dependencies
            .remove(upstream_dependency);
    }

    pub fn set_intent_host_idx(&mut self, host_idx: HostIdx) {
        self.intent.set_host_idx(host_idx);
    }

    pub fn add_downstream_data_dependency(&mut self, downstream_dependency: &mut Self) {
        let dependency_id = downstream_dependency.id;
        let dependency_intent = &mut downstream_dependency.intent;

        let arg_idx = dependency_intent.args.len();

        dependency_intent
            .args
            .push(NandoArgument::UnresolvedArgument(arg_idx));
        self.downstream_dependents
            .push(DownstreamTaskDependency::DataDependency(
                DependencyRef::EcbRef(dependency_id),
                arg_idx,
            ));
    }

    pub fn add_downstream_data_dependency_with_arg_idx(
        &mut self,
        downstream_dependency_id: EcbId,
        arg_idx: usize,
    ) {
        self.downstream_dependents
            .push(DownstreamTaskDependency::DataDependency(
                DependencyRef::EcbRef(downstream_dependency_id),
                arg_idx,
            ));
    }

    pub fn remove_downstream_dependency(&mut self, downstream_dependency_id: EcbId) {
        self.downstream_dependents
            .retain(|d| d.get_inner_ecb_id().unwrap() != downstream_dependency_id);
    }

    pub fn add_downstream_data_dependency_at_idx(
        &mut self,
        downstream_dependency: &mut Self,
        arg_idx: usize,
    ) {
        let dependency_id = downstream_dependency.id;
        let dependency_intent = &mut downstream_dependency.intent;

        dependency_intent.args[arg_idx] =
            NandoArgument::TaskedUnresolvedArgument((self.id, arg_idx));
        self.downstream_dependents
            .push(DownstreamTaskDependency::DataDependency(
                DependencyRef::EcbRef(dependency_id),
                arg_idx,
            ));
    }

    pub fn update_fold_data_dependency(&mut self, fold_task: &mut Self) -> usize {
        let dependency_id = fold_task.id;
        let dependency_intent = &mut fold_task.intent;

        if dependency_intent.args.len() < 2 {
            unreachable!("insufficient number of arguments in folded task");
        }

        self.downstream_dependents
            .push(DownstreamTaskDependency::DataDependency(
                DependencyRef::EcbRef(dependency_id),
                1,
            ));

        fold_task
            .intent
            .args
            .push(NandoArgument::TaskedUnresolvedArgument((self.id, 1)));

        return fold_task.intent.args.len() - 1;
    }

    #[cfg(feature = "object-caching")]
    pub fn new_spawned_cache_update_task(
        spawner_id: EcbId,
        object_arg: NandoArgument,
        num_caches: usize,
    ) -> Self {
        let activation_id = ActivationId::new_subtxn(&spawner_id.get_activation_id());
        let sub_ecb_id = EcbId::new(spawner_id.get_host_idx(), activation_id);

        Self {
            id: sub_ecb_id,
            intent: NandoActivationIntent {
                host_idx: None,
                name: "update_caches".to_string(),
                args: vec![object_arg, num_caches.into()],
            },
            parent_task: DownstreamTaskDependency::ParentDependency(DependencyRef::EcbRef(
                spawner_id,
            )),
            downstream_dependents: Vec::default(),
            upstream_control_dependencies: HashSet::default(),
            mask_invalidations: false,

            planning_context: PlanningContext::default(),

            combinator_context: CombinatorContext::NoCombinator,
        }
    }

    #[cfg(feature = "object-caching")]
    pub fn new_spawn_cache_task(
        spawner_id: EcbId,
        object_arg: NandoArgument,
        version: u64,
    ) -> Self {
        let activation_id = ActivationId::new_subtxn(&spawner_id.get_activation_id());
        let sub_ecb_id = EcbId::new(spawner_id.get_host_idx(), activation_id);

        Self {
            id: sub_ecb_id,
            intent: NandoActivationIntent {
                host_idx: None,
                name: "spawn_cache".to_string(),
                args: vec![object_arg, version.into()],
            },
            parent_task: DownstreamTaskDependency::ParentDependency(DependencyRef::EcbRef(
                spawner_id,
            )),
            downstream_dependents: Vec::default(),
            upstream_control_dependencies: HashSet::default(),
            mask_invalidations: false,

            planning_context: PlanningContext::default(),
            combinator_context: CombinatorContext::NoCombinator,
        }
    }

    pub fn downgrade_parent_dependency(&mut self) {
        if !self.parent_task.is_ptr() {
            return;
        }

        let parent_ecb_id = self.parent_task.get_inner_ecb_id().unwrap();
        self.parent_task = DownstreamTaskDependency::parent_dependency_from(parent_ecb_id);
    }

    pub fn mark_fold(&mut self) {
        self.combinator_context = CombinatorContext::Fold {
            can_subdivide: true,
        };
    }

    pub fn reset_combinator_context(&mut self) {
        self.combinator_context = CombinatorContext::NoCombinator;
    }

    pub fn is_fold(&self) -> bool {
        match self.combinator_context {
            CombinatorContext::Fold { .. } => true,
            _ => false,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ECB {
    pub id: EcbId,

    // Tasks spawned by this activation
    pub spawned_tasks: Vec<SpawnedTask>,

    result_task: Option<EcbId>,
    // The below sets should be identical for completed subtrees
    pub notifying_tasks: HashSet<EcbId>,

    // (if not top-level transaction) pointer to the control
    // block we should update on completion
    // parent_ecb: Option<EcbId>,
    parent_ecb: DownstreamTaskDependency,

    // pointers to any continuations with a data or control dependency
    // on this task (spawn/yield pair)
    pub downstream_dependents: Vec<DownstreamTaskDependency>,

    pub upstream_control_dependencies: HashSet<EcbId>,

    // FIXME this should contain the list of object ids that we want to mask invalidations for
    // probably, instead of being a simple boolean
    #[cfg(feature = "object-caching")]
    pub mask_invalidations: bool,

    // The result output by this nanotransaction, if any.
    result: Option<NandoResult>,

    pub planning_context: PlanningContext,
}

impl ECB {
    pub fn new_top_level(host_idx: u64, top_level_activation_id: ActivationId) -> Self {
        let ecb_id = EcbId::new(host_idx, top_level_activation_id);

        Self {
            id: ecb_id,
            spawned_tasks: Vec::with_capacity(4),
            result_task: None,
            notifying_tasks: HashSet::default(),
            parent_ecb: DownstreamTaskDependency::None,
            downstream_dependents: Vec::default(),
            result: None,
            upstream_control_dependencies: HashSet::default(),
            #[cfg(feature = "object-caching")]
            mask_invalidations: false,

            planning_context: PlanningContext::default(),
        }
    }

    pub fn new_from_dependency_info(spawned_task_info: &SpawnedTask) -> Self {
        let sub_ecb_id = spawned_task_info.id;

        Self {
            id: sub_ecb_id,
            spawned_tasks: Vec::with_capacity(4),
            result_task: None,
            notifying_tasks: HashSet::default(),
            parent_ecb: spawned_task_info.get_parent_as_downstream_dependency(),
            downstream_dependents: spawned_task_info.downstream_dependents.clone(),
            result: None,
            upstream_control_dependencies: spawned_task_info.upstream_control_dependencies.clone(),
            #[cfg(feature = "object-caching")]
            mask_invalidations: spawned_task_info.mask_invalidations,

            planning_context: spawned_task_info.planning_context.clone(),
        }
    }

    #[inline]
    pub fn get_last_unresolved_arg_idx(&self) -> usize {
        self.spawned_tasks.len()
    }

    pub fn add_new_spawned_task(&mut self, pending_intent: NandoActivationIntent) -> NandoArgument {
        let pending_argument_idx = self.spawned_tasks.len();

        let activation_id = ActivationId::new_subtxn(&self.get_associated_activation_id());
        let sub_ecb_id = EcbId::new(self.id.get_host_idx(), activation_id);
        self.notifying_tasks.insert(sub_ecb_id);
        self.spawned_tasks.push(SpawnedTask {
            id: sub_ecb_id,
            intent: pending_intent,
            parent_task: DownstreamTaskDependency::ParentDependency(DependencyRef::EcbRef(self.id)),
            downstream_dependents: Vec::default(),
            upstream_control_dependencies: HashSet::new(),
            #[cfg(feature = "object-caching")]
            mask_invalidations: self.mask_invalidations,

            planning_context: self.planning_context.unindexed_from_existing(),

            combinator_context: CombinatorContext::NoCombinator,
        });

        NandoArgument::UnresolvedArgument(pending_argument_idx)
    }

    pub fn add_new_spawned_task_and_update_dependencies(
        &mut self,
        pending_intent: NandoActivationIntent,
    ) -> NandoArgument {
        let pending_argument_idx = self.spawned_tasks.len();

        let mut pending_intent = pending_intent;
        let activation_id = ActivationId::new_subtxn(&self.get_associated_activation_id());
        let spawned_task_id = EcbId::new(self.id.get_host_idx(), activation_id);

        for (arg_idx, arg) in pending_intent.args.iter_mut().enumerate() {
            match arg {
                NandoArgument::UnresolvedArgument(dependency_task_idx) => {
                    let dependency_task = &mut self.spawned_tasks[*dependency_task_idx];
                    let dependency_task_ecb_id = dependency_task.id;
                    // Since this task dependency will contain the pending intent as a downstream
                    // dependency, it is not expected to notify the spawning task.
                    self.notifying_tasks.remove(&dependency_task_ecb_id);

                    dependency_task.downstream_dependents.push(
                        DownstreamTaskDependency::DataDependency(
                            DependencyRef::EcbRef(spawned_task_id),
                            arg_idx,
                        ),
                    );
                    *arg =
                        NandoArgument::TaskedUnresolvedArgument((dependency_task_ecb_id, arg_idx));
                }
                _ => (),
            }
        }

        let spawned_task = SpawnedTask {
            id: spawned_task_id,
            intent: pending_intent,
            parent_task: DownstreamTaskDependency::ParentDependency(DependencyRef::EcbRef(self.id)),
            downstream_dependents: Vec::default(),
            upstream_control_dependencies: HashSet::default(),
            #[cfg(feature = "object-caching")]
            mask_invalidations: self.mask_invalidations,

            planning_context: self.planning_context.unindexed_from_existing(),
            combinator_context: CombinatorContext::NoCombinator,
        };

        self.notifying_tasks.insert(spawned_task.id);
        self.spawned_tasks.push(spawned_task);
        NandoArgument::UnresolvedArgument(pending_argument_idx)
    }

    pub fn add_new_spawned_task_with_control_dependencies(
        &mut self,
        pending_intent: NandoActivationIntent,
    ) -> NandoArgument {
        let pending_argument_idx = self.spawned_tasks.len();

        let mut spawned_task = {
            let activation_id = ActivationId::new_subtxn(&self.get_associated_activation_id());
            let sub_ecb_id = EcbId::new(self.id.get_host_idx(), activation_id);
            SpawnedTask {
                id: sub_ecb_id,
                intent: pending_intent,
                parent_task: DownstreamTaskDependency::ParentDependency(DependencyRef::EcbRef(
                    self.id,
                )),
                downstream_dependents: Vec::default(),
                upstream_control_dependencies: HashSet::new(),
                #[cfg(feature = "object-caching")]
                mask_invalidations: self.mask_invalidations,

                planning_context: self.planning_context.unindexed_from_existing(),

                combinator_context: CombinatorContext::NoCombinator,
            }
        };

        match self.spawned_tasks.last_mut() {
            Some(last_intent) => {
                spawned_task
                    .upstream_control_dependencies
                    .insert(last_intent.id);

                // Since this task dependency will contain the pending intent as a downstream
                // dependency, it is not expected to notify the spawning task.
                self.notifying_tasks.remove(&last_intent.id);

                last_intent.downstream_dependents.push(
                    DownstreamTaskDependency::ControlDependency(DependencyRef::EcbRef(
                        spawned_task.id,
                    )),
                );
            }
            None => {
                // it might be the case that the expected upstream control dependency got
                // short-circuited, so we just need to add this as a spawned task and it will get
                // executed on commit of the current txn
            }
        };

        self.notifying_tasks.insert(spawned_task.id);
        self.spawned_tasks.push(spawned_task);

        NandoArgument::UnresolvedArgument(pending_argument_idx)
    }

    pub fn add_continuation_with_control_dependencies(
        &mut self,
        pending_intent: NandoActivationIntent,
        control_dependencies: &Vec<NandoArgument>,
    ) -> NandoArgument {
        let pending_argument_idx = self.spawned_tasks.len();

        let activation_id = ActivationId::new_subtxn(&self.get_associated_activation_id());
        let spawned_task_id = EcbId::new(self.id.get_host_idx(), activation_id);

        let mut spawned_task = SpawnedTask {
            id: spawned_task_id,
            intent: pending_intent,
            // parent_task: Some(self.id),
            parent_task: DownstreamTaskDependency::ParentDependency(DependencyRef::EcbRef(self.id)),
            downstream_dependents: Vec::default(),
            upstream_control_dependencies: HashSet::default(),
            #[cfg(feature = "object-caching")]
            mask_invalidations: self.mask_invalidations,

            planning_context: self.planning_context.unindexed_from_existing(),
            combinator_context: CombinatorContext::NoCombinator,
        };

        for control_dependency in control_dependencies {
            match control_dependency {
                NandoArgument::UnresolvedArgument(dependency_task_idx) => {
                    let dependency_task = &mut self.spawned_tasks[*dependency_task_idx];
                    let dependency_task_ecb_id = dependency_task.id;

                    // Since this task dependency will contain the pending intent as a downstream
                    // dependency, it is not expected to notify the spawning task.
                    self.notifying_tasks.remove(&dependency_task_ecb_id);

                    dependency_task.downstream_dependents.push(
                        DownstreamTaskDependency::ControlDependency(DependencyRef::EcbRef(
                            spawned_task_id,
                        )),
                    );
                    spawned_task
                        .upstream_control_dependencies
                        .insert(dependency_task_ecb_id);
                }
                _ => (),
            }
        }

        self.notifying_tasks.insert(spawned_task.id);
        self.spawned_tasks.push(spawned_task);
        NandoArgument::UnresolvedArgument(pending_argument_idx)
    }

    #[inline]
    pub fn is_top_level(&self) -> bool {
        self.parent_ecb.is_none()
    }

    #[inline]
    pub fn should_notify_parent(&self) -> bool {
        self.downstream_dependents.is_empty()
    }

    #[inline]
    pub fn has_downstream_dependents(&self) -> bool {
        !self.downstream_dependents.is_empty()
    }

    pub fn has_spawned_tasks(&self) -> bool {
        self.spawned_tasks.len() > 0
    }

    pub fn get_downstream_dependents(&self) -> Vec<DownstreamTaskDependency> {
        self.downstream_dependents.clone()
    }

    pub fn set_parent_ecb_id(&mut self, parent_ecb_id: EcbId) {
        self.parent_ecb =
            DownstreamTaskDependency::ParentDependency(DependencyRef::EcbRef(parent_ecb_id));
    }

    pub fn get_parent_ecb_id(&self) -> Option<EcbId> {
        match self.parent_ecb {
            DownstreamTaskDependency::None => None,
            DownstreamTaskDependency::ParentDependency(ref dep_ref) => dep_ref.get_inner_ecb_id(),
            _ => panic!("invalid dependency type in parent slot"),
        }
    }

    pub fn get_parent_as_downstream_dependency(&self) -> DownstreamTaskDependency {
        self.parent_ecb.clone()
    }

    pub fn get_result(&self) -> Option<NandoResult> {
        self.result.clone()
    }

    pub fn set_result(&mut self, result: NandoResult) {
        self.result = Some(result);
    }

    pub fn get_associated_activation_id(&self) -> ActivationId {
        self.id.get_activation_id()
    }

    pub fn get_last_unresolved_arg(&self) -> Option<NandoArgument> {
        // Used when shortcircuiting spawns to make the retrieval of the output of the last yield
        // or spawn easier to fetch
        if self.spawned_tasks.is_empty() {
            return None;
        }

        Some(NandoArgument::UnresolvedArgument(
            self.get_last_unresolved_arg_idx() - 1,
        ))
    }

    pub fn to_task_control_info(&self, intent: NandoActivationIntent) -> SpawnedTask {
        SpawnedTask {
            id: self.id,
            intent,
            parent_task: self.parent_ecb.clone(),
            downstream_dependents: self.downstream_dependents.clone(),
            upstream_control_dependencies: self.upstream_control_dependencies.clone(),
            #[cfg(feature = "object-caching")]
            mask_invalidations: self.mask_invalidations,

            planning_context: self.planning_context.clone(),

            // FIXME
            combinator_context: CombinatorContext::NoCombinator,
        }
    }

    pub fn maybe_set_result_from_task(&mut self, ecb_id: EcbId, result: Option<NandoResult>) {
        if self.result.is_some() || !self.is_result_task(ecb_id) {
            return;
        }

        if let Some(ref r) = self.result {
            if !r.is_nil() {
                return;
            }
        }

        self.result = result;
    }

    #[inline]
    fn is_result_task(&self, ecb_id: EcbId) -> bool {
        match self.result_task {
            None => false,
            Some(result_task_id) => result_task_id == ecb_id,
        }
    }

    pub fn set_result_task(&mut self, result_task_id: EcbId) {
        self.result_task = Some(result_task_id);
    }

    pub fn set_last_spawn_as_result_task(&mut self) {
        if self.result_task.is_some() {
            return;
        }

        self.result_task = match self.spawned_tasks.last() {
            None => None,
            Some(t) => Some(t.id),
        };
    }

    pub fn mark_control_dependency_done(&mut self, control_dependency_id: EcbId) {
        self.upstream_control_dependencies
            .remove(&control_dependency_id);
    }

    pub fn has_pending_control_dependencies(&self) -> bool {
        !self.upstream_control_dependencies.is_empty()
    }

    pub fn add_notifying_task(&mut self, notifying_task_id: EcbId) {
        self.notifying_tasks.insert(notifying_task_id);
    }

    #[cfg(feature = "object-caching")]
    pub fn should_mask_invalidations(&self) -> bool {
        self.mask_invalidations
    }

    #[cfg(feature = "object-caching")]
    pub fn set_mask_invalidations(&mut self, value: bool) {
        self.mask_invalidations = value;
    }

    #[cfg(feature = "object-caching")]
    pub fn insert_invalidation_spawn_sink_task(&mut self, to_invalidate: IPtr) {
        let invalidation_intent = NandoActivationIntent {
            host_idx: None,
            name: "spawn_cache_invalidations".to_string(),
            args: vec![NandoArgument::Ref(to_invalidate)],
        };

        match self.spawned_tasks.is_empty() {
            true => {
                self.add_new_spawned_task(invalidation_intent);
            }
            false => {
                let dependencies_to_wait_for = self
                    .spawned_tasks
                    .iter()
                    .enumerate()
                    .filter(|(_, t)| t.should_notify_parent())
                    .map(|(idx, _)| NandoArgument::UnresolvedArgument(idx))
                    .collect();
                self.add_continuation_with_control_dependencies(
                    invalidation_intent,
                    &dependencies_to_wait_for,
                );
            }
        }

        let invalidation_task = self.spawned_tasks.last_mut().unwrap();
        invalidation_task.mask_invalidations = false;
    }

    pub fn mark_last_task_as_fold(&mut self) {
        match self.spawned_tasks.last_mut() {
            None => {}
            Some(t) => t.mark_fold(),
        }
    }
}

impl From<&SpawnedTask> for ECB {
    fn from(value: &SpawnedTask) -> Self {
        Self {
            id: value.id,
            spawned_tasks: Vec::default(),
            result_task: None,
            notifying_tasks: HashSet::new(),
            parent_ecb: value.parent_task.clone(),
            downstream_dependents: value.downstream_dependents.clone(),
            upstream_control_dependencies: value.upstream_control_dependencies.clone(),
            result: None,
            #[cfg(feature = "object-caching")]
            mask_invalidations: value.mask_invalidations,

            planning_context: value.planning_context.clone(),
        }
    }
}

impl From<&mut SpawnedTask> for ECB {
    fn from(value: &mut SpawnedTask) -> Self {
        Self::from(&*value)
    }
}

#[derive(Clone, Debug)]
pub struct ParkableControlBlock {
    pub control_block: ECB,
    pub intent: NandoActivationIntent,
    pub completed_tasks: HashMap<EcbId, TaskStatus>,
    pub status: TaskStatus,
    pub cache_invalidation_tasks: Option<Vec<SpawnedTask>>,
    pub allocations: HashSet<ObjectId>,
}

impl ParkableControlBlock {
    #[inline]
    pub fn all_tasks_have_joined(&self) -> bool {
        self.control_block.notifying_tasks.len() == self.completed_tasks.len()
    }

    pub fn add_completed_task(&mut self, completed_task_id: EcbId, status: TaskStatus) -> bool {
        if !self
            .control_block
            .notifying_tasks
            .contains(&completed_task_id)
            || self.completed_tasks.contains_key(&completed_task_id)
        {
            return false;
        }
        self.completed_tasks.insert(completed_task_id, status);
        true
    }

    /// Return the status of this task based on subtask results
    pub fn update_status(&mut self) -> TaskStatus {
        match self
            .completed_tasks
            .values()
            .all(|s| s == &TaskStatus::Success)
        {
            false => {
                self.mark_failed();
                TaskStatus::Failure
            }
            true => {
                self.mark_completed();
                TaskStatus::Success
            }
        }
    }

    #[inline]
    pub fn mark_completed(&mut self) {
        self.status = TaskStatus::Success;
    }

    #[inline]
    pub fn mark_failed(&mut self) {
        self.status = TaskStatus::Failure;
    }

    #[inline]
    pub fn get_status(&self) -> TaskStatus {
        self.status
    }

    #[inline]
    pub fn is_immediately_runnable(&self) -> bool {
        if self.status.is_failed() {
            return false;
        }

        !(self.intent.has_unresolved_args()
            || self.control_block.has_pending_control_dependencies())
    }

    pub fn get_notification_targets(&self) -> Vec<DownstreamTaskDependency> {
        let mut notifications = vec![];
        // FIXME @dataflow logic
        let downstream_dependencies = self.control_block.get_downstream_dependents();

        // if we have a downstream dependency, then we are not in the set of notifying
        // tasks for the ecb
        if !downstream_dependencies.is_empty() {
            notifications.extend(downstream_dependencies);
        } else if self.control_block.should_notify_parent() {
            let parent_dependency = self.control_block.get_parent_as_downstream_dependency();
            notifications.push(parent_dependency);
        }

        notifications
    }
}

#[cfg(feature = "object-caching")]
pub fn create_remote_pull_task(source_task: &SpawnedTask, object_target: ObjectId) -> SpawnedTask {
    let pull_activation_id = ActivationId::new_subtxn(&source_task.get_associated_activation_id());
    let sub_ecb_id = EcbId::new(source_task.id.get_host_idx(), pull_activation_id);
    let intent = {
        let mut intent = NandoActivationIntent::new_for("fault_cache_pull".to_string());
        intent.args.push(IPtr::new(object_target, 0, 0).into());
        intent
    };
    let pull_task = SpawnedTask {
        id: sub_ecb_id,
        intent,
        parent_task: source_task.get_parent_as_downstream_dependency(),
        downstream_dependents: vec![DownstreamTaskDependency::ControlDependency(
            DependencyRef::EcbRef(source_task.id),
        )],
        upstream_control_dependencies: HashSet::new(),
        mask_invalidations: false,
        planning_context: PlanningContext::default(),
        combinator_context: CombinatorContext::NoCombinator,
    };

    pull_task
}

#[cfg(feature = "object-caching")]
pub fn create_remote_pull_task_under(task_id: &EcbId) -> SpawnedTask {
    let pull_activation_id = ActivationId::new_subtxn(&task_id.get_activation_id());
    let sub_ecb_id = EcbId::new(task_id.get_host_idx(), pull_activation_id);
    let pull_task = SpawnedTask {
        id: sub_ecb_id,
        intent: NandoActivationIntent::new_for("fault_cache_pull".to_string()),
        parent_task: DownstreamTaskDependency::None,
        downstream_dependents: Vec::default(),
        upstream_control_dependencies: HashSet::new(),
        mask_invalidations: false,
        planning_context: PlanningContext::default(),
        combinator_context: CombinatorContext::NoCombinator,
    };

    pull_task
}

#[cfg(feature = "object-caching")]
pub fn create_invalidation_task(
    partial_task: &SpawnedTask,
    original_object_id: ObjectId,
    cached_object_id: ObjectId,
    cached_version: ObjectVersion,
) -> SpawnedTask {
    let invalidation_activation_id =
        ActivationId::new_subtxn(&partial_task.get_associated_activation_id());
    let sub_ecb_id = EcbId::new(partial_task.id.get_host_idx(), invalidation_activation_id);
    let mut invalidation_task = SpawnedTask {
        id: sub_ecb_id,
        intent: NandoActivationIntent::new_for("invalidate".to_string()),
        parent_task: partial_task.get_parent_as_downstream_dependency(),
        downstream_dependents: Vec::default(),
        upstream_control_dependencies: HashSet::new(),
        mask_invalidations: false,
        planning_context: PlanningContext::default(),
        combinator_context: CombinatorContext::NoCombinator,
    };

    invalidation_task.intent.args.extend_from_slice(&[
        NandoArgument::Ref(IPtr::new(original_object_id, 0, 0)),
        NandoArgument::Ref(IPtr::new(cached_object_id, 0, 0)),
        <u64 as Into<NandoArgument>>::into(cached_version),
    ]);

    invalidation_task
}
