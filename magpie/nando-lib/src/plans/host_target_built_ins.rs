use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use crate::plans::definitions::*;

#[derive(Clone, Debug)]
pub enum PredefinedTargetFunction {
    SelfIdxOffset,
    SplitAlongAntiDiagonal(usize),
    HashTaskIdx,
}

fn get_host_at_task_idx_offset(ctx: &HostTargetEvalCtx) -> HostTarget {
    HostTarget::HostIdx(ctx.hosts[ctx.self_idx % ctx.hosts.len()] as usize)
}

fn split_along_anti_diagonal(ctx: &HostTargetEvalCtx, tasks_along_dimension: usize) -> HostTarget {
    let task_idx = ctx.self_idx;
    let task_row = match (task_idx - 1) % tasks_along_dimension {
        0 => (task_idx - 1) / tasks_along_dimension - 1,
        _ => (task_idx - 1) / tasks_along_dimension,
    };
    let task_column = (task_idx - 1) - (tasks_along_dimension * task_row) - 1;
    let anti_diagonal_idx = task_row + task_column + 1;
    let first_row = match anti_diagonal_idx <= tasks_along_dimension {
        true => 0,
        false => anti_diagonal_idx - tasks_along_dimension,
    };
    let task_group = ((task_row - first_row) as f64 / ctx.num_workers as f64).floor() as usize;
    HostTarget::HostIdx(ctx.hosts[task_group % ctx.hosts.len()] as usize)
}

fn hash_task_idx(ctx: &HostTargetEvalCtx) -> HostTarget {
    let task_idx = ctx.self_idx;
    let task_hash = {
        let mut hasher = DefaultHasher::default();
        task_idx.hash(&mut hasher);
        hasher.finish() as usize
    };

    HostTarget::HostIdx(ctx.hosts[task_hash % ctx.hosts.len()] as usize)
}

pub(crate) fn compute_host_target<'a>(
    using_built_in: &PredefinedTargetFunction,
    ctx: &HostTargetEvalCtx,
) -> HostTarget {
    match using_built_in {
        PredefinedTargetFunction::SelfIdxOffset => get_host_at_task_idx_offset(ctx),
        PredefinedTargetFunction::SplitAlongAntiDiagonal(n) => split_along_anti_diagonal(ctx, *n),
        PredefinedTargetFunction::HashTaskIdx => hash_task_idx(ctx),
    }
}
