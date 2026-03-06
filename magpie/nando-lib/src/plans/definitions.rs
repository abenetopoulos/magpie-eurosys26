use nando_support::HostIdx;

use crate::plans::host_target_built_ins::PredefinedTargetFunction;

pub(crate) type TaskIdx = usize;

#[derive(Clone, Debug)]
pub(crate) enum TaskIndex {
    SelfIdx,
    Idx(TaskIdx),
    // TODO we can encapsulate a universal quantifier by changing this from a usize to some kind
    // of enum
    Spawns(usize),
}

#[derive(Clone, Debug)]
pub(crate) enum PlanNodeIdx {
    Any,
    Idx(TaskIdx),
}

#[derive(Clone, Debug)]
pub(crate) enum ObjectArgumentDomain {
    Arguments,
    Objects,
    Allocations,
    NoDomain,
}

#[derive(Clone, Debug)]
pub(crate) struct ObjectArgumentRef {
    // FIXME should be able to say `spawner` / `parent` here but will do for now
    pub node_idx: TaskIndex,
    pub domain: ObjectArgumentDomain,
    pub arg_idx: usize,
}

#[derive(Clone, Debug)]
pub(crate) enum Dependency {
    ObjectDependency(ObjectArgumentRef, ObjectArgumentRef),
    ControlDependency(TaskIndex),
}

#[derive(Clone, Debug)]
pub struct HostTargetEvalCtx<'a> {
    pub hosts: &'a Vec<HostIdx>,
    pub self_idx: TaskIdx,
    pub num_workers: u16,
}

#[derive(Clone, Debug)]
pub(crate) enum HostTarget {
    NextHost,
    HostIdx(usize),
    EvalResult(PredefinedTargetFunction),
    // Owner(Vec<ObjectArgumentRef>),
    Owner(ObjectArgumentRef),
    NonOwners(ObjectArgumentRef),
}

#[derive(Clone, Debug)]
pub(crate) struct LocaticsSpecifier {
    pub target_object: ObjectArgumentRef,
    pub target_host: HostTarget,
}

#[derive(Clone, Debug)]
pub(crate) enum RangeOver {
    NoRange,
    Spawns,
    Hosts,
}

#[derive(Clone, Debug)]
pub(crate) struct RangedLocaticsSpecifier {
    pub range: RangeOver,
    pub specifier: LocaticsSpecifier,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ActionBlock {
    pub dependencies: Vec<Dependency>,
    pub ownership_transfers: Vec<LocaticsSpecifier>,
    pub moves: Vec<LocaticsSpecifier>,
    pub push_copies: Vec<RangedLocaticsSpecifier>,
}

impl ActionBlock {
    pub fn is_default(&self) -> bool {
        self.dependencies.is_empty()
            && self.ownership_transfers.is_empty()
            && self.moves.is_empty()
            && self.push_copies.is_empty()
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct PostActionBlock {
    pub append_to_current_task: bool,
    pub action_block: ActionBlock,
}

impl PostActionBlock {
    pub fn is_default(&self) -> bool {
        self.append_to_current_task == false && self.action_block.is_default()
    }
}

#[derive(Clone, Debug)]
pub(crate) enum ActivationTarget {
    Owner(ObjectArgumentRef),
    EvalResult(PredefinedTargetFunction),
}

#[derive(Clone, Debug)]
pub(crate) struct PhysicalPlanNode {
    pub idx: PlanNodeIdx,
    pub intent_name: String,
    pub schedule_on: ActivationTarget,
    pub pre_actions: ActionBlock,
    pub post_actions: PostActionBlock,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ParsedPhysicalPlan {
    pub nodes: Vec<PhysicalPlanNode>,
}

impl ParsedPhysicalPlan {
    pub fn find_node_for_intent(
        &self,
        intent_name: &str,
        min_idx: TaskIdx,
    ) -> Option<&PhysicalPlanNode> {
        // NOTE Assumes that Any nodes follow all numbered nodes in the plan.
        let start_idx = match self.nodes.binary_search_by_key(&min_idx, |n| match n.idx {
            PlanNodeIdx::Idx(task_idx) => task_idx,
            PlanNodeIdx::Any => TaskIdx::MAX,
        }) {
            Ok(idx) => idx,
            Err(any_block_start_idx) => any_block_start_idx,
        };

        for idx in start_idx..self.nodes.len() {
            if &self.nodes[idx].intent_name == intent_name {
                return Some(&self.nodes[idx]);
            }
        }

        None
    }
}
