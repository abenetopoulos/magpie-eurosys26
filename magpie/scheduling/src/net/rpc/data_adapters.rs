use execution_definitions::nando_handle::ActivationOutput;
use nando_lib::nando_scheduler::TaskCompletionNotification;
use nando_support::{activation_intent, epic_control, iptr::IPtr};
use ownership_support as ownership;

use crate::net::rpc::worker_rpc_client::worker_rpc::{self as worker_rpc_client};
use crate::net::rpc::worker_rpc_server::worker_rpc::{self, NandoArgumentKind, ScalarValueKind};

impl From<&worker_rpc::IPtr> for IPtr {
    fn from(value: &worker_rpc::IPtr) -> Self {
        Self {
            object_id: value.object_id.parse().expect("failed to parse object id"),
            offset: value.offset,
            size: value.size,
        }
    }
}

impl From<&IPtr> for worker_rpc::IPtr {
    fn from(value: &IPtr) -> Self {
        Self {
            object_id: value.object_id.to_string(),
            offset: value.offset,
            size: value.size,
        }
    }
}

impl From<&epic_control::TaskStatus> for worker_rpc::NandoStatusKind {
    fn from(value: &epic_control::TaskStatus) -> Self {
        match value {
            epic_control::TaskStatus::Success => Self::Success,
            epic_control::TaskStatus::Failure => Self::Error,
            _ => Self::Other,
        }
    }
}

impl From<&worker_rpc::NandoStatusKind> for epic_control::TaskStatus {
    fn from(value: &worker_rpc::NandoStatusKind) -> Self {
        match value {
            worker_rpc::NandoStatusKind::Success => Self::Success,
            worker_rpc::NandoStatusKind::Error => Self::Failure,
            worker_rpc::NandoStatusKind::Other | worker_rpc::NandoStatusKind::RecomputedSite => {
                panic!()
            }
        }
    }
}

impl From<&bool> for worker_rpc::ScalarValue {
    fn from(value: &bool) -> Self {
        worker_rpc::ScalarValue {
            kind: ScalarValueKind::Bool.into(),
            i32_value: None,
            u64_value: None,
            string_value: None,
            bool_value: Some(*value),
            array_values: Vec::default(),
        }
    }
}

impl From<&activation_intent::NumberType> for worker_rpc::ScalarValue {
    fn from(value: &activation_intent::NumberType) -> Self {
        match value {
            activation_intent::NumberType::NumberI32(n) => worker_rpc::ScalarValue {
                kind: ScalarValueKind::I32.into(),
                i32_value: Some(*n),
                u64_value: None,
                string_value: None,
                bool_value: None,
                array_values: Vec::default(),
            },
            activation_intent::NumberType::NumberU8(n) => worker_rpc::ScalarValue {
                kind: ScalarValueKind::U8.into(),
                i32_value: None,
                u64_value: Some((*n).into()),
                string_value: None,
                bool_value: None,
                array_values: Vec::default(),
            },
            activation_intent::NumberType::NumberU64(n) => worker_rpc::ScalarValue {
                kind: ScalarValueKind::U64.into(),
                i32_value: None,
                u64_value: Some(*n),
                string_value: None,
                bool_value: None,
                array_values: Vec::default(),
            },
            activation_intent::NumberType::NumberUS(n) => worker_rpc::ScalarValue {
                kind: ScalarValueKind::Usize.into(),
                i32_value: None,
                u64_value: None,
                string_value: Some(n.to_string()),
                bool_value: None,
                array_values: Vec::default(),
            },
            activation_intent::NumberType::NumberU128(n) => worker_rpc::ScalarValue {
                kind: ScalarValueKind::U128.into(),
                i32_value: None,
                u64_value: None,
                string_value: Some(n.to_string()),
                bool_value: None,
                array_values: Vec::default(),
            },
            activation_intent::NumberType::NumberF64(_f) => todo!("scalarvalue::f64 for rpc layer"),
        }
    }
}

impl From<&worker_rpc::ScalarValue> for activation_intent::ScalarValue {
    fn from(value: &worker_rpc::ScalarValue) -> Self {
        match ScalarValueKind::try_from(value.kind) {
            Ok(ScalarValueKind::I32) => {
                let value = value
                    .i32_value
                    .expect("no value for i32-kinded scalar value");
                activation_intent::ScalarValue::from(value)
            }
            Ok(ScalarValueKind::U64) => {
                let value = value
                    .u64_value
                    .expect("no value for u64-kinded scalar value");
                activation_intent::ScalarValue::from(value)
            }
            Ok(ScalarValueKind::Usize) => {
                let value = value
                    .string_value
                    .clone()
                    .expect("no value for usize-kinded scalar value");
                let value = value
                    .parse::<usize>()
                    .expect(&format!("failed to parse string as usize: {}", value));
                activation_intent::ScalarValue::from(value)
            }
            Ok(ScalarValueKind::U128) => {
                let value = value
                    .string_value
                    .clone()
                    .expect("no value for u128-kinded scalar value");
                let value = value
                    .parse::<u128>()
                    .expect(&format!("failed to parse string as u128: {}", value));
                activation_intent::ScalarValue::from(value)
            }
            Ok(ScalarValueKind::String) => {
                let value = value
                    .string_value
                    .clone()
                    .expect("no value for string-kinded scalar value");
                activation_intent::ScalarValue::from(value)
            }
            Ok(ScalarValueKind::Bool) => {
                let value = value
                    .bool_value
                    .clone()
                    .expect("no value for bool-kinded scalar value");
                activation_intent::ScalarValue::from(value)
            }
            Ok(ScalarValueKind::Array) => activation_intent::ScalarValue::Array(
                value
                    .array_values
                    .iter()
                    .map(|v| activation_intent::ScalarValue::from(v))
                    .collect(),
            ),
            _ => panic!("unsupported scalar argument: {:?}", value),
        }
    }
}

impl From<&activation_intent::ScalarValue> for worker_rpc::ScalarValue {
    fn from(value: &activation_intent::ScalarValue) -> Self {
        match value {
            activation_intent::ScalarValue::Number(ref nt) => nt.into(),
            activation_intent::ScalarValue::Bool(b) => b.into(),
            activation_intent::ScalarValue::Str(st) => Self {
                kind: ScalarValueKind::String.into(),
                i32_value: None,
                u64_value: None,
                string_value: Some(st.clone()),
                bool_value: None,
                array_values: Vec::default(),
            },
            activation_intent::ScalarValue::Array(vs) => Self {
                kind: ScalarValueKind::Array.into(),
                i32_value: None,
                u64_value: None,
                string_value: None,
                bool_value: None,
                array_values: vs.iter().map(|v| v.into()).collect(),
            },
            _ => panic!("unhandled scalar value type {:?}", value),
        }
    }
}

impl From<&worker_rpc::ActivationIntent> for activation_intent::NandoActivationIntent {
    fn from(value: &worker_rpc::ActivationIntent) -> Self {
        Self {
            host_idx: Some(value.host_idx),
            name: value.name.clone(),
            args: value.args.iter().map(|a| a.into()).collect(),
        }
    }
}

impl From<&activation_intent::NandoActivationIntent> for worker_rpc::ActivationIntent {
    fn from(value: &activation_intent::NandoActivationIntent) -> Self {
        Self {
            host_idx: value
                .host_idx
                .expect("missing host idx in intra-worker intent"),
            name: value.name.clone(),
            args: value.args.iter().map(|a| a.into()).collect(),
        }
    }
}

impl From<&worker_rpc::NandoArgument> for activation_intent::NandoArgument {
    fn from(value: &worker_rpc::NandoArgument) -> Self {
        match NandoArgumentKind::try_from(value.kind) {
            Ok(NandoArgumentKind::Ref) => {
                let serialized_iptr = value.iptr.as_ref().expect("no iptr in ref argument");
                activation_intent::NandoArgument::Ref(IPtr::from(serialized_iptr))
            }
            Ok(NandoArgumentKind::MRef) => {
                let serialized_iptrs = &value.iptrs;
                activation_intent::NandoArgument::MRef(serialized_iptrs.into())
            }
            Ok(NandoArgumentKind::Value) => {
                let serialized_value = value
                    .value
                    .as_ref()
                    .expect("no concrete value for value-kinded argument");
                activation_intent::NandoArgument::Value(activation_intent::ScalarValue::from(
                    serialized_value,
                ))
            }
            Ok(NandoArgumentKind::Unresolved) => {
                let arg_idx = value.unresolved_arg_idx.unwrap();
                match value.unresolved_arg_task_id {
                    Some(ref task_id) => {
                        activation_intent::NandoArgument::TaskedUnresolvedArgument((
                            task_id.parse().expect("failed to parse task id"),
                            arg_idx.try_into().unwrap(),
                        ))
                    }
                    None => activation_intent::NandoArgument::UnresolvedArgument(
                        arg_idx.try_into().unwrap(),
                    ),
                }
            }
            Ok(NandoArgumentKind::Nil) => {
                activation_intent::NandoArgument::Value(activation_intent::ScalarValue::Nil)
            }
            _ => panic!("failed to parse argument kind"),
        }
    }
}

impl From<&activation_intent::NandoArgument> for worker_rpc::NandoResult {
    fn from(value: &activation_intent::NandoArgument) -> Self {
        match value {
            activation_intent::NandoArgument::Ref(iptr) => Self {
                kind: NandoArgumentKind::Ref.into(),
                iptr: Some(iptr.into()),
                value: None,
                spawned_epic_id: None,
                iptrs: Vec::default(),
                unresolved_arg_idx: None,
                unresolved_arg_task_id: None,
            },
            activation_intent::NandoArgument::MRef(ref iptrs) => Self {
                kind: NandoArgumentKind::MRef.into(),
                iptr: None,
                value: None,
                spawned_epic_id: None,
                iptrs: iptrs.into(),
                unresolved_arg_idx: None,
                unresolved_arg_task_id: None,
            },
            activation_intent::NandoArgument::Value(v) => match v {
                activation_intent::ScalarValue::Nil => Self {
                    kind: NandoArgumentKind::Nil.into(),
                    iptr: None,
                    value: None,
                    spawned_epic_id: None,
                    iptrs: Vec::default(),
                    unresolved_arg_idx: None,
                    unresolved_arg_task_id: None,
                },
                _ => Self {
                    kind: NandoArgumentKind::Value.into(),
                    iptr: None,
                    value: Some(v.into()),
                    spawned_epic_id: None,
                    iptrs: Vec::default(),
                    unresolved_arg_idx: None,
                    unresolved_arg_task_id: None,
                },
            },
            activation_intent::NandoArgument::UnresolvedArgument(idx) => Self {
                kind: NandoArgumentKind::Unresolved.into(),
                iptr: None,
                value: None,
                iptrs: Vec::default(),
                spawned_epic_id: None,
                unresolved_arg_idx: Some(*idx as u32),
                unresolved_arg_task_id: None,
            },
            activation_intent::NandoArgument::TaskedUnresolvedArgument((ecb_id, idx)) => Self {
                kind: NandoArgumentKind::Unresolved.into(),
                iptr: None,
                value: None,
                iptrs: Vec::default(),
                spawned_epic_id: None,
                unresolved_arg_idx: Some(*idx as u32),
                unresolved_arg_task_id: Some(ecb_id.to_string()),
            },
        }
    }
}

impl From<&activation_intent::NandoArgument> for worker_rpc::NandoArgument {
    fn from(value: &activation_intent::NandoArgument) -> Self {
        match value {
            activation_intent::NandoArgument::Ref(iptr) => Self {
                kind: NandoArgumentKind::Ref.into(),
                iptr: Some(iptr.into()),
                value: None,
                iptrs: Vec::default(),
                unresolved_arg_idx: None,
                unresolved_arg_task_id: None,
            },
            activation_intent::NandoArgument::MRef(ref iptrs) => Self {
                kind: NandoArgumentKind::MRef.into(),
                iptr: None,
                value: None,
                iptrs: iptrs.into(),
                unresolved_arg_idx: None,
                unresolved_arg_task_id: None,
            },
            activation_intent::NandoArgument::Value(v) => match v {
                activation_intent::ScalarValue::Nil => Self {
                    kind: NandoArgumentKind::Nil.into(),
                    iptr: None,
                    value: None,
                    iptrs: Vec::default(),
                    unresolved_arg_idx: None,
                    unresolved_arg_task_id: None,
                },
                _ => Self {
                    kind: NandoArgumentKind::Value.into(),
                    iptr: None,
                    value: Some(v.into()),
                    iptrs: Vec::default(),
                    unresolved_arg_idx: None,
                    unresolved_arg_task_id: None,
                },
            },
            activation_intent::NandoArgument::UnresolvedArgument(idx) => Self {
                kind: NandoArgumentKind::Unresolved.into(),
                iptr: None,
                value: None,
                iptrs: Vec::default(),
                unresolved_arg_idx: Some(*idx as u32),
                unresolved_arg_task_id: None,
            },
            activation_intent::NandoArgument::TaskedUnresolvedArgument((ecb_id, idx)) => Self {
                kind: NandoArgumentKind::Unresolved.into(),
                iptr: None,
                value: None,
                iptrs: Vec::default(),
                unresolved_arg_idx: Some(*idx as u32),
                unresolved_arg_task_id: Some(ecb_id.to_string()),
            },
        }
    }
}

impl From<&worker_rpc::NandoResult> for activation_intent::NandoArgument {
    fn from(value: &worker_rpc::NandoResult) -> Self {
        match NandoArgumentKind::try_from(value.kind) {
            Ok(NandoArgumentKind::Ref) => {
                let serialized_iptr = value.iptr.as_ref().expect("no iptr in ref argument");
                activation_intent::NandoArgument::Ref(IPtr::from(serialized_iptr))
            }
            Ok(NandoArgumentKind::Value) => {
                let serialized_value = value
                    .value
                    .as_ref()
                    .expect("no concrete value for value-kinded argument");
                activation_intent::NandoArgument::Value(activation_intent::ScalarValue::from(
                    serialized_value,
                ))
            }
            Ok(NandoArgumentKind::Unresolved) => {
                let arg_idx = value.unresolved_arg_idx.unwrap();
                match value.unresolved_arg_task_id {
                    Some(ref task_id) => {
                        activation_intent::NandoArgument::TaskedUnresolvedArgument((
                            task_id.parse().expect("failed to parse task id"),
                            arg_idx.try_into().unwrap(),
                        ))
                    }
                    None => activation_intent::NandoArgument::UnresolvedArgument(
                        arg_idx.try_into().unwrap(),
                    ),
                }
            }
            Ok(NandoArgumentKind::Nil) => {
                activation_intent::NandoArgument::Value(activation_intent::ScalarValue::Nil)
            }
            _ => panic!("failed to parse argument kind"),
        }
    }
}

impl From<&ActivationOutput> for worker_rpc::NandoResult {
    fn from(value: &ActivationOutput) -> Self {
        match value {
            ActivationOutput::Result(ref r) => r.into(),
            ActivationOutput::Epic(ecb_id) => worker_rpc::NandoResult {
                kind: NandoArgumentKind::Epic.into(),
                spawned_epic_id: Some(ecb_id.to_string()),
                iptr: None,
                value: None,
                iptrs: Vec::default(),
                unresolved_arg_idx: None,
                unresolved_arg_task_id: None,
            },
        }
    }
}

impl From<&worker_rpc::NandoResult> for ActivationOutput {
    fn from(value: &worker_rpc::NandoResult) -> Self {
        match NandoArgumentKind::try_from(value.kind) {
            Ok(NandoArgumentKind::Epic) => ActivationOutput::Epic(
                value
                    .spawned_epic_id
                    .as_ref()
                    .expect("missing ecb id in epic entity")
                    .parse()
                    .unwrap(),
            ),
            Ok(_) => ActivationOutput::Result(value.into()),
            _ => panic!("failed to parse argument kind"),
        }
    }
}

impl From<&worker_rpc::PlanningContext> for epic_control::PlanningContext {
    fn from(value: &worker_rpc::PlanningContext) -> Self {
        Self {
            idx: value.idx as usize,
            plan_key: value.plan_key.clone(),
        }
    }
}

impl From<&worker_rpc::SpawnedTask> for epic_control::SpawnedTask {
    fn from(value: &worker_rpc::SpawnedTask) -> Self {
        Self {
            id: value.id.parse().expect("failed to parse spawned task id"),
            intent: value
                .intent
                .as_ref()
                .expect("missing intent in spawned task")
                .into(),
            parent_task: match value.parent_task.as_ref() {
                None => epic_control::DownstreamTaskDependency::None,
                Some(p) => epic_control::DownstreamTaskDependency::ParentDependency(
                    epic_control::DependencyRef::EcbRef(
                        p.parse().expect("failed to parse parent task id"),
                    ),
                ),
            },
            downstream_dependents: value
                .downstream_dependents
                .iter()
                .map(|dd| dd.into())
                .collect(),
            upstream_control_dependencies: value
                .upstream_control_dependencies
                .keys()
                .map(|k| k.parse().expect("failed to parse upstream dependency id"))
                .collect(),
            #[cfg(feature = "object-caching")]
            mask_invalidations: match value.mask_invalidations {
                None => false,
                Some(mi) => mi,
            },

            planning_context: match value.planning_context {
                None => epic_control::PlanningContext::default(),
                Some(ref p) => p.into(),
            },

            combinator_context: match value.is_fold {
                false => epic_control::CombinatorContext::NoCombinator,
                true => epic_control::CombinatorContext::Fold {
                    can_subdivide: false,
                },
            },
        }
    }
}

impl From<&epic_control::DownstreamTaskDependency> for worker_rpc::DownstreamDependent {
    fn from(value: &epic_control::DownstreamTaskDependency) -> Self {
        match value {
            epic_control::DownstreamTaskDependency::ParentDependency(dep_ref) => {
                worker_rpc::DownstreamDependent {
                    kind: worker_rpc::DownstreamDependentKind::Parent.into(),
                    dependency_id: dep_ref.get_inner_ecb_id().unwrap().to_string(),
                    argument_idx: None,
                }
            }
            epic_control::DownstreamTaskDependency::ControlDependency(dep_ref) => {
                worker_rpc::DownstreamDependent {
                    kind: worker_rpc::DownstreamDependentKind::Control.into(),
                    dependency_id: dep_ref.get_inner_ecb_id().unwrap().to_string(),
                    argument_idx: None,
                }
            }
            epic_control::DownstreamTaskDependency::DataDependency(dep_ref, arg_idx) => {
                worker_rpc::DownstreamDependent {
                    kind: worker_rpc::DownstreamDependentKind::Data.into(),
                    dependency_id: dep_ref.get_inner_ecb_id().unwrap().to_string(),
                    argument_idx: Some(
                        (*arg_idx)
                            .try_into()
                            .expect("arg_idx exceeds max serializable"),
                    ),
                }
            }
            epic_control::DownstreamTaskDependency::None => {
                panic!("cannot handle none-type for dependency")
            }
        }
    }
}

impl From<TaskCompletionNotification> for worker_rpc::TaskCompletion {
    fn from(value: TaskCompletionNotification) -> Self {
        let (tasks_to_notify, statuses_results): (Vec<_>, Vec<(&epic_control::TaskStatus, _)>) =
            value
                .tasks_to_notify
                .iter()
                .map(|(t, s, r)| (t.into(), (s, r)))
                .unzip();
        let (statuses, results): (Vec<_>, Vec<_>) = statuses_results
            .into_iter()
            .map(|(s, r)| {
                (
                    <&epic_control::TaskStatus as Into<worker_rpc::NandoStatusKind>>::into(s),
                    r,
                )
            })
            .unzip();
        Self {
            id: value.completed_task_id.to_string(),
            tasks_to_notify,
            results: results
                .iter()
                .map(|r| match r {
                    None => worker_rpc::NandoResult {
                        kind: worker_rpc::NandoArgumentKind::Nil.into(),
                        iptr: None,
                        value: None,
                        spawned_epic_id: None,
                        iptrs: Vec::default(),
                        unresolved_arg_idx: None,
                        unresolved_arg_task_id: None,
                    },
                    Some(ref r) => r.into(),
                })
                .collect(),
            // FIXME
            subgraph_allocations: Vec::default(),
            notifying_host_idx: u64::default(),
            statuses: statuses.into_iter().map(|s| s.into()).collect(),
        }
    }
}

impl From<&worker_rpc::DownstreamDependent> for epic_control::DownstreamTaskDependency {
    fn from(value: &worker_rpc::DownstreamDependent) -> Self {
        match worker_rpc::DownstreamDependentKind::try_from(value.kind) {
            Ok(worker_rpc::DownstreamDependentKind::Parent) => {
                Self::ParentDependency(epic_control::DependencyRef::EcbRef(
                    value
                        .dependency_id
                        .parse()
                        .expect("failed to parse parent dependency id"),
                ))
            }
            Ok(worker_rpc::DownstreamDependentKind::Control) => {
                Self::ControlDependency(epic_control::DependencyRef::EcbRef(
                    value
                        .dependency_id
                        .parse()
                        .expect("failed to parse control dependency id"),
                ))
            }
            Ok(worker_rpc::DownstreamDependentKind::Data) => Self::DataDependency(
                epic_control::DependencyRef::EcbRef(
                    value
                        .dependency_id
                        .parse()
                        .expect("failed to parse data dependency id"),
                ),
                value.argument_idx.expect("missing arg idx") as usize,
            ),
            _ => panic!("cannot convert downstream dependency"),
        }
    }
}

// Client-side adapters.

impl From<&worker_rpc_client::IPtr> for IPtr {
    fn from(value: &worker_rpc_client::IPtr) -> Self {
        Self {
            object_id: value.object_id.parse().expect("failed to parse object id"),
            offset: value.offset,
            size: value.size,
        }
    }
}

impl From<&IPtr> for worker_rpc_client::IPtr {
    fn from(value: &IPtr) -> Self {
        Self {
            object_id: value.object_id.to_string(),
            offset: value.offset,
            size: value.size,
        }
    }
}

impl From<&epic_control::TaskStatus> for worker_rpc_client::NandoStatusKind {
    fn from(value: &epic_control::TaskStatus) -> Self {
        match value {
            epic_control::TaskStatus::Success => Self::Success.into(),
            epic_control::TaskStatus::Failure => Self::Error.into(),
            _ => Self::Other.into(),
        }
    }
}

impl From<&worker_rpc_client::NandoStatusKind> for epic_control::TaskStatus {
    fn from(value: &worker_rpc_client::NandoStatusKind) -> Self {
        match value {
            worker_rpc_client::NandoStatusKind::Success => Self::Success,
            worker_rpc_client::NandoStatusKind::Error => Self::Failure,
            worker_rpc_client::NandoStatusKind::Other
            | worker_rpc_client::NandoStatusKind::RecomputedSite => panic!(),
        }
    }
}

impl From<&bool> for worker_rpc_client::ScalarValue {
    fn from(value: &bool) -> Self {
        worker_rpc_client::ScalarValue {
            kind: ScalarValueKind::Bool.into(),
            i32_value: None,
            u64_value: None,
            string_value: None,
            bool_value: Some(*value),
            array_values: Vec::default(),
        }
    }
}

impl From<&activation_intent::NumberType> for worker_rpc_client::ScalarValue {
    fn from(value: &activation_intent::NumberType) -> Self {
        match value {
            activation_intent::NumberType::NumberI32(n) => worker_rpc_client::ScalarValue {
                kind: ScalarValueKind::I32.into(),
                i32_value: Some(*n),
                u64_value: None,
                string_value: None,
                bool_value: None,
                array_values: Vec::default(),
            },
            activation_intent::NumberType::NumberU8(n) => worker_rpc_client::ScalarValue {
                kind: ScalarValueKind::U8.into(),
                i32_value: None,
                u64_value: Some((*n).into()),
                string_value: None,
                bool_value: None,
                array_values: Vec::default(),
            },
            activation_intent::NumberType::NumberU64(n) => worker_rpc_client::ScalarValue {
                kind: ScalarValueKind::U64.into(),
                i32_value: None,
                u64_value: Some(*n),
                string_value: None,
                bool_value: None,
                array_values: Vec::default(),
            },
            activation_intent::NumberType::NumberUS(n) => worker_rpc_client::ScalarValue {
                kind: ScalarValueKind::Usize.into(),
                i32_value: None,
                u64_value: None,
                string_value: Some(n.to_string()),
                bool_value: None,
                array_values: Vec::default(),
            },
            activation_intent::NumberType::NumberU128(n) => worker_rpc_client::ScalarValue {
                kind: ScalarValueKind::U128.into(),
                i32_value: None,
                u64_value: None,
                string_value: Some(n.to_string()),
                bool_value: None,
                array_values: Vec::default(),
            },
            activation_intent::NumberType::NumberF64(_f) => todo!("scalarvalue::f64 for rpc layer"),
        }
    }
}

impl From<&worker_rpc_client::ScalarValue> for activation_intent::ScalarValue {
    fn from(value: &worker_rpc_client::ScalarValue) -> Self {
        match ScalarValueKind::try_from(value.kind) {
            Ok(ScalarValueKind::I32) => {
                let value = value
                    .i32_value
                    .expect("no value for i32-kinded scalar value");
                activation_intent::ScalarValue::from(value)
            }
            Ok(ScalarValueKind::U64) => {
                let value = value
                    .u64_value
                    .expect("no value for u64-kinded scalar value");
                activation_intent::ScalarValue::from(value)
            }
            Ok(ScalarValueKind::Usize) => {
                let value = value
                    .string_value
                    .clone()
                    .expect("no value for usize-kinded scalar value");
                let value = value
                    .parse::<usize>()
                    .expect(&format!("failed to parse string as usize: {}", value));
                activation_intent::ScalarValue::from(value)
            }
            Ok(ScalarValueKind::U128) => {
                let value = value
                    .string_value
                    .clone()
                    .expect("no value for u128-kinded scalar value");
                let value = value
                    .parse::<u128>()
                    .expect(&format!("failed to parse string as u128: {}", value));
                activation_intent::ScalarValue::from(value)
            }
            Ok(ScalarValueKind::String) => {
                let value = value
                    .string_value
                    .clone()
                    .expect("no value for string-kinded scalar value");
                activation_intent::ScalarValue::from(value)
            }
            Ok(ScalarValueKind::Bool) => {
                let value = value
                    .bool_value
                    .clone()
                    .expect("no value for bool-kinded scalar value");
                activation_intent::ScalarValue::from(value)
            }
            Ok(ScalarValueKind::Array) => activation_intent::ScalarValue::Array(
                value
                    .array_values
                    .iter()
                    .map(|v| activation_intent::ScalarValue::from(v))
                    .collect(),
            ),
            _ => panic!("unsupported scalar argument"),
        }
    }
}

impl From<&activation_intent::ScalarValue> for worker_rpc_client::ScalarValue {
    fn from(value: &activation_intent::ScalarValue) -> Self {
        match value {
            activation_intent::ScalarValue::Number(ref nt) => nt.into(),
            activation_intent::ScalarValue::Bool(b) => b.into(),
            activation_intent::ScalarValue::Str(st) => Self {
                kind: ScalarValueKind::String.into(),
                i32_value: None,
                u64_value: None,
                string_value: Some(st.clone()),
                bool_value: None,
                array_values: Vec::default(),
            },
            activation_intent::ScalarValue::Array(vs) => Self {
                kind: ScalarValueKind::Array.into(),
                i32_value: None,
                u64_value: None,
                string_value: None,
                bool_value: None,
                array_values: vs.iter().map(|v| v.into()).collect(),
            },
            _ => panic!("unhandled scalar value type {:?}", value),
        }
    }
}

impl From<&worker_rpc_client::ActivationIntent> for activation_intent::NandoActivationIntent {
    fn from(value: &worker_rpc_client::ActivationIntent) -> Self {
        Self {
            host_idx: Some(value.host_idx),
            name: value.name.clone(),
            args: value.args.iter().map(|a| a.into()).collect(),
        }
    }
}

impl From<&activation_intent::NandoActivationIntent> for worker_rpc_client::ActivationIntent {
    fn from(value: &activation_intent::NandoActivationIntent) -> Self {
        Self {
            host_idx: value
                .host_idx
                .expect("missing host idx in intra-worker intent"),
            name: value.name.clone(),
            args: value.args.iter().map(|a| a.into()).collect(),
        }
    }
}

impl From<&worker_rpc_client::NandoArgument> for activation_intent::NandoArgument {
    fn from(value: &worker_rpc_client::NandoArgument) -> Self {
        match NandoArgumentKind::try_from(value.kind) {
            Ok(NandoArgumentKind::Ref) => {
                let serialized_iptr = value.iptr.as_ref().expect("no iptr in ref argument");
                activation_intent::NandoArgument::Ref(IPtr::from(serialized_iptr))
            }
            Ok(NandoArgumentKind::Value) => {
                let serialized_value = value
                    .value
                    .as_ref()
                    .expect("no concrete value for value-kinded argument");
                activation_intent::NandoArgument::Value(activation_intent::ScalarValue::from(
                    serialized_value,
                ))
            }
            Ok(NandoArgumentKind::Unresolved) => {
                let arg_idx = value.unresolved_arg_idx.unwrap();
                match value.unresolved_arg_task_id {
                    Some(ref task_id) => {
                        activation_intent::NandoArgument::TaskedUnresolvedArgument((
                            task_id.parse().expect("failed to parse task id"),
                            arg_idx.try_into().unwrap(),
                        ))
                    }
                    None => activation_intent::NandoArgument::UnresolvedArgument(
                        arg_idx.try_into().unwrap(),
                    ),
                }
            }
            Ok(NandoArgumentKind::Nil) => {
                activation_intent::NandoArgument::Value(activation_intent::ScalarValue::Nil)
            }
            _ => panic!("failed to parse argument kind"),
        }
    }
}

impl From<&worker_rpc_client::NandoResult> for activation_intent::NandoArgument {
    fn from(value: &worker_rpc_client::NandoResult) -> Self {
        match NandoArgumentKind::try_from(value.kind) {
            Ok(NandoArgumentKind::Ref) => {
                let serialized_iptr = value.iptr.as_ref().expect("no iptr in ref argument");
                activation_intent::NandoArgument::Ref(IPtr::from(serialized_iptr))
            }
            Ok(NandoArgumentKind::Value) => {
                let serialized_value = value
                    .value
                    .as_ref()
                    .expect("no concrete value for value-kinded argument");
                activation_intent::NandoArgument::Value(activation_intent::ScalarValue::from(
                    serialized_value,
                ))
            }
            Ok(NandoArgumentKind::Unresolved) => {
                let arg_idx = value.unresolved_arg_idx.unwrap();
                match value.unresolved_arg_task_id {
                    Some(ref task_id) => {
                        activation_intent::NandoArgument::TaskedUnresolvedArgument((
                            task_id.parse().expect("failed to parse task id"),
                            arg_idx.try_into().unwrap(),
                        ))
                    }
                    None => activation_intent::NandoArgument::UnresolvedArgument(
                        arg_idx.try_into().unwrap(),
                    ),
                }
            }
            Ok(NandoArgumentKind::Epic) => {
                let serialized_value = value
                    .value
                    .as_ref()
                    .expect("no concrete value for value-kinded argument");
                activation_intent::NandoArgument::Value(activation_intent::ScalarValue::from(
                    serialized_value,
                ))
            }
            Ok(NandoArgumentKind::Nil) => {
                activation_intent::NandoArgument::Value(activation_intent::ScalarValue::Nil)
            }
            _ => panic!("failed to parse argument kind"),
        }
    }
}

impl From<&activation_intent::NandoArgument> for worker_rpc_client::NandoArgument {
    fn from(value: &activation_intent::NandoArgument) -> Self {
        match value {
            activation_intent::NandoArgument::Ref(iptr) => Self {
                kind: NandoArgumentKind::Ref.into(),
                iptr: Some(iptr.into()),
                value: None,
                iptrs: Vec::default(),
                unresolved_arg_idx: None,
                unresolved_arg_task_id: None,
            },
            activation_intent::NandoArgument::MRef(ref iptrs) => Self {
                kind: NandoArgumentKind::MRef.into(),
                iptr: None,
                value: None,
                iptrs: iptrs.into(),
                unresolved_arg_idx: None,
                unresolved_arg_task_id: None,
            },
            activation_intent::NandoArgument::Value(v) => match v {
                activation_intent::ScalarValue::Nil => Self {
                    kind: NandoArgumentKind::Nil.into(),
                    iptr: None,
                    value: None,
                    iptrs: Vec::default(),
                    unresolved_arg_idx: None,
                    unresolved_arg_task_id: None,
                },
                _ => Self {
                    kind: NandoArgumentKind::Value.into(),
                    iptr: None,
                    value: Some(v.into()),
                    iptrs: Vec::default(),
                    unresolved_arg_idx: None,
                    unresolved_arg_task_id: None,
                },
            },
            activation_intent::NandoArgument::UnresolvedArgument(idx) => Self {
                kind: NandoArgumentKind::Unresolved.into(),
                iptr: None,
                value: None,
                iptrs: Vec::default(),
                unresolved_arg_idx: Some(*idx as u32),
                unresolved_arg_task_id: None,
            },
            activation_intent::NandoArgument::TaskedUnresolvedArgument((ecb_id, idx)) => Self {
                kind: NandoArgumentKind::Unresolved.into(),
                iptr: None,
                value: None,
                iptrs: Vec::default(),
                unresolved_arg_idx: Some(*idx as u32),
                unresolved_arg_task_id: Some(ecb_id.to_string()),
            },
        }
    }
}

impl From<&activation_intent::NandoArgument> for worker_rpc_client::NandoResult {
    fn from(value: &activation_intent::NandoArgument) -> Self {
        match value {
            activation_intent::NandoArgument::Ref(iptr) => Self {
                kind: NandoArgumentKind::Ref.into(),
                iptr: Some(iptr.into()),
                value: None,
                spawned_epic_id: None,
                iptrs: Vec::default(),
                unresolved_arg_idx: None,
                unresolved_arg_task_id: None,
            },
            activation_intent::NandoArgument::MRef(ref iptrs) => Self {
                kind: NandoArgumentKind::MRef.into(),
                iptr: None,
                value: None,
                spawned_epic_id: None,
                iptrs: iptrs.into(),
                unresolved_arg_idx: None,
                unresolved_arg_task_id: None,
            },
            activation_intent::NandoArgument::Value(v) => match v {
                activation_intent::ScalarValue::Nil => Self {
                    kind: NandoArgumentKind::Nil.into(),
                    iptr: None,
                    value: None,
                    spawned_epic_id: None,
                    iptrs: Vec::default(),
                    unresolved_arg_idx: None,
                    unresolved_arg_task_id: None,
                },
                _ => Self {
                    kind: NandoArgumentKind::Value.into(),
                    iptr: None,
                    value: Some(v.into()),
                    spawned_epic_id: None,
                    iptrs: Vec::default(),
                    unresolved_arg_idx: None,
                    unresolved_arg_task_id: None,
                },
            },
            activation_intent::NandoArgument::UnresolvedArgument(idx) => Self {
                kind: NandoArgumentKind::Unresolved.into(),
                iptr: None,
                value: None,
                iptrs: Vec::default(),
                spawned_epic_id: None,
                unresolved_arg_idx: Some(*idx as u32),
                unresolved_arg_task_id: None,
            },
            activation_intent::NandoArgument::TaskedUnresolvedArgument((ecb_id, idx)) => Self {
                kind: NandoArgumentKind::Unresolved.into(),
                iptr: None,
                value: None,
                iptrs: Vec::default(),
                spawned_epic_id: None,
                unresolved_arg_idx: Some(*idx as u32),
                unresolved_arg_task_id: Some(ecb_id.to_string()),
            },
        }
    }
}

impl From<&ActivationOutput> for worker_rpc_client::NandoResult {
    fn from(value: &ActivationOutput) -> Self {
        match value {
            ActivationOutput::Result(ref r) => r.into(),
            ActivationOutput::Epic(ecb_id) => worker_rpc_client::NandoResult {
                kind: NandoArgumentKind::Epic.into(),
                spawned_epic_id: Some(ecb_id.to_string()),
                iptr: None,
                value: None,
                iptrs: Vec::default(),
                unresolved_arg_idx: None,
                unresolved_arg_task_id: None,
            },
        }
    }
}

impl From<&worker_rpc_client::NandoResult> for ActivationOutput {
    fn from(value: &worker_rpc_client::NandoResult) -> Self {
        match NandoArgumentKind::try_from(value.kind) {
            Ok(NandoArgumentKind::Epic) => ActivationOutput::Epic(
                value
                    .spawned_epic_id
                    .as_ref()
                    .expect("missing ecb id in epic entity")
                    .parse()
                    .unwrap(),
            ),
            Ok(_) => ActivationOutput::Result(value.into()),
            _ => panic!("failed to parse argument kind"),
        }
    }
}

impl From<&epic_control::PlanningContext> for worker_rpc_client::PlanningContext {
    fn from(value: &epic_control::PlanningContext) -> Self {
        Self {
            idx: value.idx as u64,
            plan_key: value.plan_key.clone(),
        }
    }
}

impl From<&epic_control::SpawnedTask> for worker_rpc_client::SpawnedTask {
    fn from(value: &epic_control::SpawnedTask) -> Self {
        Self {
            id: value.id.to_string(),
            intent: Some((&value.intent).into()),
            parent_task: match value.parent_task.get_inner_ecb_id() {
                None => None,
                Some(ref p) => Some(p.to_string()),
            },
            downstream_dependents: value
                .downstream_dependents
                .iter()
                .map(|dd| dd.into())
                .collect(),
            upstream_control_dependencies: value
                .upstream_control_dependencies
                .iter()
                .map(|k| (k.to_string(), true))
                .collect(),
            #[cfg(not(feature = "object-caching"))]
            mask_invalidations: None,
            #[cfg(feature = "object-caching")]
            mask_invalidations: Some(value.mask_invalidations),

            planning_context: match value.planning_context.is_default() {
                true => None,
                false => Some((&value.planning_context).into()),
            },

            is_fold: value.is_fold(),
        }
    }
}

impl From<&epic_control::DownstreamTaskDependency> for worker_rpc_client::DownstreamDependent {
    fn from(value: &epic_control::DownstreamTaskDependency) -> Self {
        match value {
            epic_control::DownstreamTaskDependency::ParentDependency(dep_ref) => {
                worker_rpc_client::DownstreamDependent {
                    kind: worker_rpc_client::DownstreamDependentKind::Parent.into(),
                    dependency_id: dep_ref.get_inner_ecb_id().unwrap().to_string(),
                    argument_idx: None,
                }
            }
            epic_control::DownstreamTaskDependency::ControlDependency(dep_ref) => {
                worker_rpc_client::DownstreamDependent {
                    kind: worker_rpc_client::DownstreamDependentKind::Control.into(),
                    dependency_id: dep_ref.get_inner_ecb_id().unwrap().to_string(),
                    argument_idx: None,
                }
            }
            epic_control::DownstreamTaskDependency::DataDependency(dep_ref, arg_idx) => {
                worker_rpc_client::DownstreamDependent {
                    kind: worker_rpc_client::DownstreamDependentKind::Data.into(),
                    dependency_id: dep_ref.get_inner_ecb_id().unwrap().to_string(),
                    argument_idx: Some(
                        (*arg_idx)
                            .try_into()
                            .expect("arg_idx exceeds max serializable"),
                    ),
                }
            }
            epic_control::DownstreamTaskDependency::None => {
                panic!("cannot handle none-type for dependency")
            }
        }
    }
}

impl From<&ownership::AssumeOwnershipRequest> for worker_rpc_client::AssumeOwnershipRequest {
    fn from(value: &ownership::AssumeOwnershipRequest) -> Self {
        Self {
            object_id: value.object_id.to_string(),
            first_version: value.first_version,
            get_signature: value.get_signature,
        }
    }
}

impl From<&ownership::MoveOwnershipRequest> for worker_rpc_client::MoveOwnershipRequest {
    fn from(value: &ownership::MoveOwnershipRequest) -> Self {
        Self {
            object_refs: value.object_refs.iter().map(|r| r.to_string()).collect(),
            new_host: value.new_host.clone(),
        }
    }
}

impl From<&worker_rpc_client::NandoStatus> for activation_intent::NandoActivationStatus {
    fn from(value: &worker_rpc_client::NandoStatus) -> Self {
        match worker_rpc_client::NandoStatusKind::try_from(value.kind) {
            Ok(worker_rpc_client::NandoStatusKind::Error) => {
                Self::Executed(activation_intent::NandoActivationExecutionStatus::Error(
                    value.error_string.as_ref().unwrap().clone(),
                ))
            }
            Ok(worker_rpc_client::NandoStatusKind::Success) => {
                Self::Executed(activation_intent::NandoActivationExecutionStatus::Success)
            }
            Ok(worker_rpc_client::NandoStatusKind::RecomputedSite) => {
                Self::RecomputedActivationSite(value.new_site.as_ref().unwrap().clone())
            }
            Ok(worker_rpc_client::NandoStatusKind::Other) => {
                panic!("unexpected nando status kind other")
            }
            Err(e) => panic!("error while parsing value kind: {}", e),
        }
    }
}

impl From<&worker_rpc_client::ActivationResolution>
    for activation_intent::NandoActivationResolution
{
    fn from(value: &worker_rpc_client::ActivationResolution) -> Self {
        let output = match value.result.is_empty() {
            true => vec![],
            false => value
                .result
                .iter()
                .map(|r| match NandoArgumentKind::try_from(r.kind) {
                    Ok(NandoArgumentKind::Epic) => {
                        let ecb_id = r
                            .spawned_epic_id
                            .as_ref()
                            .expect("missing spawned ecb id")
                            .parse()
                            .expect("failed to parse spawned epic id");
                        activation_intent::NandoResultSerializable::Epic(ecb_id)
                    }
                    _ => {
                        let result: activation_intent::NandoResult = r.into();
                        (&result).into()
                    }
                })
                .collect(),
        };

        Self {
            status: value
                .status
                .as_ref()
                .expect("missing status in resolution")
                .into(),
            output,
            cacheable_objects: Some(
                value
                    .cacheable_objects
                    .iter()
                    .map(|pair| {
                        (
                            pair.object_id
                                .parse()
                                .expect(&format!("failed to parse object id {}", pair.object_id)),
                            pair.version,
                        )
                    })
                    .collect(),
            ),
        }
    }
}

impl From<&TaskCompletionNotification> for worker_rpc_client::TaskCompletion {
    fn from(value: &TaskCompletionNotification) -> Self {
        let (tasks_to_notify, statuses_results): (Vec<_>, Vec<(&epic_control::TaskStatus, _)>) =
            value
                .tasks_to_notify
                .iter()
                .map(|(t, s, r)| (t.into(), (s, r)))
                .unzip();
        let (statuses, results): (Vec<_>, Vec<_>) = statuses_results
            .into_iter()
            .map(|(s, r)| {
                (
                    <&epic_control::TaskStatus as Into<worker_rpc::NandoStatusKind>>::into(s),
                    r,
                )
            })
            .unzip();

        Self {
            id: value.completed_task_id.to_string(),
            tasks_to_notify,
            results: results
                .iter()
                .map(|r| match r {
                    None => worker_rpc_client::NandoResult {
                        kind: worker_rpc_client::NandoArgumentKind::Nil.into(),
                        iptr: None,
                        value: None,
                        spawned_epic_id: None,
                        iptrs: Vec::default(),
                        unresolved_arg_idx: None,
                        unresolved_arg_task_id: None,
                    },
                    Some(ref r) => r.into(),
                })
                .collect(),
            // FIXME
            subgraph_allocations: Vec::default(),
            notifying_host_idx: u64::default(),
            statuses: statuses.into_iter().map(|s| s.into()).collect(),
        }
    }
}
