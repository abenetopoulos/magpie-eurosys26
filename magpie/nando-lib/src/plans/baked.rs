use crate::plans::definitions::*;
use crate::plans::host_target_built_ins::*;

macro_rules! sort_input_no_move {
    ($task_idx:expr) => {{
        PhysicalPlanNode {
            idx: PlanNodeIdx::Idx($task_idx),
            intent_name: "sorting::sorting::sort_input_block".to_string(),
            pre_actions: ActionBlock::default(),
            schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                node_idx: TaskIndex::SelfIdx,
                domain: ObjectArgumentDomain::Arguments,
                arg_idx: 1,
            }),
            post_actions: PostActionBlock::default(),
        }
    }};
}

macro_rules! sort_input_next_host_move {
    ($task_idx:expr) => {{
        PhysicalPlanNode {
            idx: PlanNodeIdx::Idx($task_idx),
            intent_name: "sorting::sorting::sort_input_block".to_string(),
            pre_actions: ActionBlock {
                dependencies: Vec::default(),
                ownership_transfers: vec![LocaticsSpecifier {
                    target_object: ObjectArgumentRef {
                        node_idx: TaskIndex::SelfIdx,
                        domain: ObjectArgumentDomain::Arguments,
                        arg_idx: 1,
                    },
                    target_host: HostTarget::NextHost,
                }],
                push_copies: vec![
                    RangedLocaticsSpecifier {
                        range: RangeOver::NoRange,
                        specifier: LocaticsSpecifier {
                            target_object: ObjectArgumentRef {
                                node_idx: TaskIndex::SelfIdx,
                                domain: ObjectArgumentDomain::Arguments,
                                arg_idx: 0,
                            },
                            target_host: HostTarget::NextHost,
                        },
                    },
                    RangedLocaticsSpecifier {
                        range: RangeOver::NoRange,
                        specifier: LocaticsSpecifier {
                            target_object: ObjectArgumentRef {
                                node_idx: TaskIndex::SelfIdx,
                                domain: ObjectArgumentDomain::Arguments,
                                arg_idx: 2,
                            },
                            target_host: HostTarget::NextHost,
                        },
                    },
                ],
                moves: vec![LocaticsSpecifier {
                    target_object: ObjectArgumentRef {
                        node_idx: TaskIndex::SelfIdx,
                        domain: ObjectArgumentDomain::Arguments,
                        arg_idx: 1,
                    },
                    target_host: HostTarget::Owner(ObjectArgumentRef {
                        node_idx: TaskIndex::SelfIdx,
                        domain: ObjectArgumentDomain::Arguments,
                        arg_idx: 1,
                    }),
                }],
            },
            schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                node_idx: TaskIndex::SelfIdx,
                domain: ObjectArgumentDomain::Arguments,
                arg_idx: 1,
            }),
            post_actions: PostActionBlock::default(),
        }
    }};
}

pub(crate) fn get_pre_baked() -> Vec<(&'static str, ParsedPhysicalPlan)> {
    vec![
        (
            "kvs-put",
            ParsedPhysicalPlan {
                nodes: vec![
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Idx(1),
                        intent_name: "kvs_consumer::put_i32".to_string(),
                        // schedule on index object owner
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::Idx(1),
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 0,
                        }),
                        pre_actions: ActionBlock::default(),
                        post_actions: PostActionBlock::default(),
                    },
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Idx(2),
                        // unfortunately, because of how namespacing for generic functions works,
                        // we need the kvs_consumer namespace
                        intent_name: "kvs_consumer::kvs::put_internal".to_string(),
                        // schedule on bucket object owner
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::Idx(2),
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 0,
                        }),
                        pre_actions: ActionBlock::default(),
                        post_actions: PostActionBlock::default(),
                    },
                ],
            },
        ),
        (
            "kvs-put-collect",
            ParsedPhysicalPlan {
                nodes: vec![
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Idx(1),
                        intent_name: "kvs_consumer::put_i32".to_string(),
                        // schedule on index object owner
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::Idx(1),
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 0,
                        }),
                        pre_actions: ActionBlock::default(),
                        post_actions: PostActionBlock {
                            append_to_current_task: false,
                            action_block: ActionBlock {
                                dependencies: Vec::default(),
                                push_copies: Vec::default(),
                                ownership_transfers: vec![LocaticsSpecifier {
                                    target_object: ObjectArgumentRef {
                                        node_idx: TaskIndex::Spawns(0),
                                        domain: ObjectArgumentDomain::Arguments,
                                        arg_idx: 0,
                                    },
                                    target_host: HostTarget::Owner(ObjectArgumentRef {
                                        node_idx: TaskIndex::Idx(1),
                                        domain: ObjectArgumentDomain::Arguments,
                                        arg_idx: 0,
                                    }),
                                }],
                                moves: vec![LocaticsSpecifier {
                                    target_object: ObjectArgumentRef {
                                        node_idx: TaskIndex::Spawns(0),
                                        domain: ObjectArgumentDomain::Arguments,
                                        arg_idx: 0,
                                    },
                                    target_host: HostTarget::Owner(ObjectArgumentRef {
                                        node_idx: TaskIndex::Spawns(0),
                                        domain: ObjectArgumentDomain::Arguments,
                                        arg_idx: 0,
                                    }),
                                }],
                            },
                        },
                    },
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Idx(2),
                        // unfortunately, because of how namespacing for generic functions works,
                        // we need the kvs_consumer namespace
                        intent_name: "kvs_consumer::kvs::put_internal".to_string(),
                        // schedule on bucket object owner
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::Idx(2),
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 0,
                        }),
                        pre_actions: ActionBlock::default(),
                        post_actions: PostActionBlock::default(),
                    },
                ],
            },
        ),
        (
            "kvs-multi-get-batch-collect",
            ParsedPhysicalPlan {
                nodes: vec![
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Idx(1),
                        intent_name: "kvs_consumer::multi_get_batch_i32".to_string(),
                        // schedule on index object owner
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::Idx(1),
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 0,
                        }),
                        pre_actions: ActionBlock::default(),
                        post_actions: PostActionBlock {
                            append_to_current_task: false,
                            action_block: ActionBlock {
                                dependencies: Vec::default(),
                                push_copies: Vec::default(),
                                ownership_transfers: vec![LocaticsSpecifier {
                                    target_object: ObjectArgumentRef {
                                        node_idx: TaskIndex::Spawns(0),
                                        domain: ObjectArgumentDomain::Objects,
                                        arg_idx: 1,
                                    },
                                    target_host: HostTarget::Owner(ObjectArgumentRef {
                                        node_idx: TaskIndex::SelfIdx,
                                        domain: ObjectArgumentDomain::Arguments,
                                        arg_idx: 0,
                                    }),
                                }],
                                moves: vec![LocaticsSpecifier {
                                    target_object: ObjectArgumentRef {
                                        node_idx: TaskIndex::Spawns(0),
                                        domain: ObjectArgumentDomain::Objects,
                                        arg_idx: 1,
                                    },
                                    target_host: HostTarget::Owner(ObjectArgumentRef {
                                        node_idx: TaskIndex::Spawns(0),
                                        domain: ObjectArgumentDomain::Arguments,
                                        arg_idx: 0,
                                    }),
                                }],
                            },
                        },
                    },
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Idx(2),
                        // unfortunately, because of how namespacing for generic functions works,
                        // we need the kvs_consumer namespace
                        intent_name: "kvs_consumer::kvs::multi_get_batch_internal".to_string(),
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::Idx(2),
                            domain: ObjectArgumentDomain::Objects,
                            arg_idx: 1,
                        }),
                        pre_actions: ActionBlock::default(),
                        post_actions: PostActionBlock::default(),
                    },
                ],
            },
        ),
        (
            "kvs-multi-get",
            ParsedPhysicalPlan {
                nodes: vec![
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Idx(1),
                        intent_name: "kvs_consumer::multi_get_i32".to_string(),
                        // schedule on index object owner
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::Idx(1),
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 0,
                        }),
                        pre_actions: ActionBlock::default(),
                        post_actions: PostActionBlock::default(),
                    },
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Any,
                        // unfortunately, because of how namespacing for generic functions works,
                        // we need the kvs_consumer namespace
                        intent_name: "kvs_consumer::kvs::multi_get_internal".to_string(),
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::SelfIdx,
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 0,
                        }),
                        pre_actions: ActionBlock::default(),
                        post_actions: PostActionBlock::default(),
                    },
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Any,
                        // unfortunately, because of how namespacing for generic functions works,
                        // we need the kvs_consumer namespace
                        intent_name: "kvs_consumer::kvs::append_partial_ref".to_string(),
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::SelfIdx,
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 0,
                        }),
                        pre_actions: ActionBlock::default(),
                        post_actions: PostActionBlock::default(),
                    },
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Any,
                        intent_name: "kvs_consumer::kvs::spawn_multi_get_merge_partial".to_string(),
                        // schedule on index object owner
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::SelfIdx,
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 0,
                        }),
                        pre_actions: ActionBlock::default(),
                        post_actions: PostActionBlock {
                            append_to_current_task: false,
                            action_block: ActionBlock {
                                dependencies: Vec::default(),
                                push_copies: Vec::default(),
                                ownership_transfers: vec![LocaticsSpecifier {
                                    target_object: ObjectArgumentRef {
                                        node_idx: TaskIndex::Spawns(0),
                                        domain: ObjectArgumentDomain::Objects,
                                        arg_idx: 0,
                                    },
                                    target_host: HostTarget::Owner(ObjectArgumentRef {
                                        node_idx: TaskIndex::SelfIdx,
                                        domain: ObjectArgumentDomain::Arguments,
                                        arg_idx: 0,
                                    }),
                                }],
                                moves: vec![LocaticsSpecifier {
                                    target_object: ObjectArgumentRef {
                                        node_idx: TaskIndex::Spawns(0),
                                        domain: ObjectArgumentDomain::Objects,
                                        arg_idx: 0,
                                    },
                                    target_host: HostTarget::Owner(ObjectArgumentRef {
                                        node_idx: TaskIndex::Spawns(0),
                                        domain: ObjectArgumentDomain::Objects,
                                        arg_idx: 0,
                                    }),
                                }],
                            },
                        },
                    },
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Any,
                        // unfortunately, because of how namespacing for generic functions works,
                        // we need the kvs_consumer namespace
                        intent_name: "kvs_consumer::kvs::multi_get_merge_partial".to_string(),
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::SelfIdx,
                            domain: ObjectArgumentDomain::Objects,
                            arg_idx: 0,
                        }),
                        pre_actions: ActionBlock::default(),
                        post_actions: PostActionBlock::default(),
                    },
                ],
            },
        ),
        (
            "kvs-repartition",
            ParsedPhysicalPlan {
                nodes: vec![
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Idx(1),
                        intent_name: "kvs_consumer::visit_chunks_i32".to_string(),
                        // schedule on index object owner
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::Idx(1),
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 0,
                        }),
                        pre_actions: ActionBlock::default(),
                        post_actions: PostActionBlock::default(),
                    },
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Any,
                        // unfortunately, because of how namespacing for generic functions works,
                        // we need the kvs_consumer namespace
                        intent_name: "kvs_consumer::kvs::visit_chunk".to_string(),
                        // schedule on bucket object owner
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::SelfIdx,
                            // FIXME we should not have to choose between Objects and other domains
                            domain: ObjectArgumentDomain::Objects,
                            arg_idx: 0,
                        }),
                        pre_actions: ActionBlock {
                            dependencies: Vec::default(),
                            ownership_transfers: vec![LocaticsSpecifier {
                                target_object: ObjectArgumentRef {
                                    node_idx: TaskIndex::SelfIdx,
                                    domain: ObjectArgumentDomain::Objects,
                                    arg_idx: 0,
                                },
                                target_host: HostTarget::EvalResult(
                                    PredefinedTargetFunction::SelfIdxOffset,
                                ),
                            }],
                            push_copies: Vec::default(),
                            moves: vec![LocaticsSpecifier {
                                target_object: ObjectArgumentRef {
                                    node_idx: TaskIndex::SelfIdx,
                                    domain: ObjectArgumentDomain::Objects,
                                    arg_idx: 0,
                                },
                                target_host: HostTarget::Owner(ObjectArgumentRef {
                                    node_idx: TaskIndex::SelfIdx,
                                    domain: ObjectArgumentDomain::Objects,
                                    arg_idx: 0,
                                }),
                            }],
                        },
                        post_actions: PostActionBlock::default(),
                    },
                ],
            },
        ),
        (
            // two sorting hosts, one merging host, 4 input / 2 output partitions.
            "sort-collection-u64-bulk-s2m1-i4o2",
            ParsedPhysicalPlan {
                nodes: vec![
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Idx(1),
                        intent_name: "sorting::sort_collection_u64".to_string(),
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::Idx(1),
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 0,
                        }),
                        pre_actions: ActionBlock::default(),
                        post_actions: PostActionBlock::default(),
                    },
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Idx(2),
                        intent_name: "sorting::sorting::sort_collection_bulk".to_string(),
                        pre_actions: ActionBlock::default(),
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::Idx(2),
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 1,
                        }),
                        post_actions: PostActionBlock::default(),
                    },
                    sort_input_no_move!(3),
                    sort_input_no_move!(4),
                    sort_input_next_host_move!(5),
                    sort_input_next_host_move!(6),
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Idx(7),
                        intent_name: "sorting::sorting::spawn_merge_tasks".to_string(),
                        pre_actions: ActionBlock::default(),
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::SelfIdx,
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 0,
                        }),
                        post_actions: PostActionBlock::default(),
                    },
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Any,
                        intent_name: "sorting::sorting::add_sorted_block_to_collection".to_string(),
                        pre_actions: ActionBlock::default(),
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::SelfIdx,
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 0,
                        }),
                        post_actions: PostActionBlock::default(),
                    },
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Any,
                        intent_name: "sorting::sorting::spawn_merge_blocks".to_string(),
                        pre_actions: ActionBlock::default(),
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::SelfIdx,
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 0,
                        }),
                        post_actions: PostActionBlock::default(),
                    },
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Any,
                        intent_name: "sorting::sorting::merge_blocks".to_string(),
                        pre_actions: ActionBlock {
                            ownership_transfers: vec![LocaticsSpecifier {
                                target_object: ObjectArgumentRef {
                                    node_idx: TaskIndex::SelfIdx,
                                    domain: ObjectArgumentDomain::Objects,
                                    arg_idx: 1,
                                },
                                target_host: HostTarget::Owner(ObjectArgumentRef {
                                    node_idx: TaskIndex::SelfIdx,
                                    domain: ObjectArgumentDomain::Arguments,
                                    arg_idx: 0,
                                }),
                            }],
                            moves: vec![LocaticsSpecifier {
                                target_object: ObjectArgumentRef {
                                    node_idx: TaskIndex::SelfIdx,
                                    domain: ObjectArgumentDomain::Objects,
                                    arg_idx: 1,
                                },
                                target_host: HostTarget::Owner(ObjectArgumentRef {
                                    node_idx: TaskIndex::SelfIdx,
                                    domain: ObjectArgumentDomain::Objects,
                                    arg_idx: 1,
                                }),
                            }],
                            push_copies: Vec::default(),
                            dependencies: Vec::default(),
                        },
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::SelfIdx,
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 0,
                        }),
                        post_actions: PostActionBlock::default(),
                    },
                ],
            },
        ),
        (
            // two sorting hosts, one merging host, 16 input/output partitions.
            "sort-collection-u64-bulk-s2m1-i16o16",
            ParsedPhysicalPlan {
                nodes: vec![
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Idx(1),
                        intent_name: "sorting::sort_collection_u64".to_string(),
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::Idx(1),
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 0,
                        }),
                        pre_actions: ActionBlock::default(),
                        post_actions: PostActionBlock::default(),
                    },
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Idx(2),
                        intent_name: "sorting::sorting::sort_collection_bulk".to_string(),
                        pre_actions: ActionBlock::default(),
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::SelfIdx,
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 1,
                        }),
                        post_actions: PostActionBlock::default(),
                    },
                    sort_input_no_move!(3),
                    sort_input_no_move!(4),
                    sort_input_no_move!(5),
                    sort_input_no_move!(6),
                    sort_input_no_move!(7),
                    sort_input_no_move!(8),
                    sort_input_no_move!(9),
                    sort_input_no_move!(10),
                    sort_input_next_host_move!(11),
                    sort_input_next_host_move!(12),
                    sort_input_next_host_move!(13),
                    sort_input_next_host_move!(14),
                    sort_input_next_host_move!(15),
                    sort_input_next_host_move!(16),
                    sort_input_next_host_move!(17),
                    sort_input_next_host_move!(18),
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Idx(19),
                        intent_name: "sorting::sorting::spawn_merge_tasks".to_string(),
                        pre_actions: ActionBlock::default(),
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::SelfIdx,
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 0,
                        }),
                        post_actions: PostActionBlock::default(),
                    },
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Any,
                        intent_name: "sorting::sorting::spawn_merge_blocks".to_string(),
                        pre_actions: ActionBlock::default(),
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::SelfIdx,
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 0,
                        }),
                        post_actions: PostActionBlock::default(),
                    },
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Any,
                        intent_name: "sorting::sorting::merge_blocks".to_string(),
                        pre_actions: ActionBlock {
                            ownership_transfers: vec![LocaticsSpecifier {
                                target_object: ObjectArgumentRef {
                                    node_idx: TaskIndex::SelfIdx,
                                    domain: ObjectArgumentDomain::Objects,
                                    arg_idx: 1,
                                },
                                target_host: HostTarget::Owner(ObjectArgumentRef {
                                    node_idx: TaskIndex::SelfIdx,
                                    domain: ObjectArgumentDomain::Arguments,
                                    arg_idx: 0,
                                }),
                            }],
                            moves: vec![LocaticsSpecifier {
                                target_object: ObjectArgumentRef {
                                    node_idx: TaskIndex::SelfIdx,
                                    domain: ObjectArgumentDomain::Objects,
                                    arg_idx: 1,
                                },
                                target_host: HostTarget::Owner(ObjectArgumentRef {
                                    node_idx: TaskIndex::SelfIdx,
                                    domain: ObjectArgumentDomain::Objects,
                                    arg_idx: 1,
                                }),
                            }],
                            push_copies: Vec::default(),
                            dependencies: Vec::default(),
                        },
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::SelfIdx,
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 0,
                        }),
                        post_actions: PostActionBlock::default(),
                    },
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Any,
                        intent_name: "sorting::sorting::add_sorted_block_to_collection".to_string(),
                        pre_actions: ActionBlock::default(),
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::SelfIdx,
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 0,
                        }),
                        post_actions: PostActionBlock::default(),
                    },
                ],
            },
        ),
        (
            // two sorting/merging host, 16 input/output partitions.
            "sort-collection-u64-bulk-s2m2-i16o16",
            ParsedPhysicalPlan {
                nodes: vec![
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Idx(1),
                        intent_name: "sorting::sort_collection_u64".to_string(),
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::Idx(1),
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 0,
                        }),
                        pre_actions: ActionBlock::default(),
                        post_actions: PostActionBlock::default(),
                    },
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Idx(2),
                        intent_name: "sorting::sorting::sort_collection_bulk".to_string(),
                        pre_actions: ActionBlock::default(),
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::SelfIdx,
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 1,
                        }),
                        post_actions: PostActionBlock::default(),
                    },
                    sort_input_no_move!(3),
                    sort_input_no_move!(4),
                    sort_input_no_move!(5),
                    sort_input_no_move!(6),
                    sort_input_no_move!(7),
                    sort_input_no_move!(8),
                    sort_input_no_move!(9),
                    sort_input_no_move!(10),
                    sort_input_next_host_move!(11),
                    sort_input_next_host_move!(12),
                    sort_input_next_host_move!(13),
                    sort_input_next_host_move!(14),
                    sort_input_next_host_move!(15),
                    sort_input_next_host_move!(16),
                    sort_input_next_host_move!(17),
                    sort_input_next_host_move!(18),
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Idx(19),
                        intent_name: "sorting::sorting::spawn_merge_tasks".to_string(),
                        pre_actions: ActionBlock::default(),
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::SelfIdx,
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 0,
                        }),
                        post_actions: PostActionBlock::default(),
                    },
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Any,
                        intent_name: "sorting::sorting::spawn_merge_blocks".to_string(),
                        pre_actions: ActionBlock::default(),
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::SelfIdx,
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 0,
                        }),
                        post_actions: PostActionBlock::default(),
                    },
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Any,
                        intent_name: "sorting::sorting::foldable_merge_blocks".to_string(),
                        pre_actions: ActionBlock {
                            ownership_transfers: vec![LocaticsSpecifier {
                                target_object: ObjectArgumentRef {
                                    node_idx: TaskIndex::SelfIdx,
                                    domain: ObjectArgumentDomain::Arguments,
                                    arg_idx: 0,
                                },
                                target_host: HostTarget::Owner(ObjectArgumentRef {
                                    node_idx: TaskIndex::SelfIdx,
                                    domain: ObjectArgumentDomain::Objects,
                                    arg_idx: 1,
                                }),
                            }],
                            moves: vec![LocaticsSpecifier {
                                target_object: ObjectArgumentRef {
                                    node_idx: TaskIndex::SelfIdx,
                                    domain: ObjectArgumentDomain::Arguments,
                                    arg_idx: 0,
                                },
                                target_host: HostTarget::Owner(ObjectArgumentRef {
                                    node_idx: TaskIndex::SelfIdx,
                                    domain: ObjectArgumentDomain::Arguments,
                                    arg_idx: 0,
                                }),
                            }],
                            push_copies: Vec::default(),
                            dependencies: Vec::default(),
                        },
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::SelfIdx,
                            domain: ObjectArgumentDomain::Objects,
                            arg_idx: 1,
                        }),
                        post_actions: PostActionBlock::default(),
                    },
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Any,
                        intent_name: "sorting::sorting::add_sorted_block_to_collection".to_string(),
                        pre_actions: ActionBlock::default(),
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::SelfIdx,
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 0,
                        }),
                        post_actions: PostActionBlock::default(),
                    },
                ],
            },
        ),
        (
            "sort-collection-repartition",
            ParsedPhysicalPlan {
                nodes: vec![
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Idx(1),
                        intent_name: "sorting_consumer::visit_chunks_u64".to_string(),
                        // schedule on index object owner
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::Idx(1),
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 0,
                        }),
                        pre_actions: ActionBlock::default(),
                        post_actions: PostActionBlock::default(),
                    },
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Any,
                        // unfortunately, because of how namespacing for generic functions works,
                        // we need the sorting_consumer namespace
                        intent_name: "sorting_consumer::sorting::visit_chunk".to_string(),
                        // schedule on bucket object owner
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::SelfIdx,
                            // FIXME we should not have to choose between Objects and other domains
                            domain: ObjectArgumentDomain::Objects,
                            arg_idx: 0,
                        }),
                        pre_actions: ActionBlock {
                            dependencies: Vec::default(),
                            ownership_transfers: vec![LocaticsSpecifier {
                                target_object: ObjectArgumentRef {
                                    node_idx: TaskIndex::SelfIdx,
                                    domain: ObjectArgumentDomain::Objects,
                                    arg_idx: 0,
                                },
                                target_host: HostTarget::EvalResult(
                                    PredefinedTargetFunction::SelfIdxOffset,
                                ),
                            }],
                            push_copies: Vec::default(),
                            moves: vec![LocaticsSpecifier {
                                target_object: ObjectArgumentRef {
                                    node_idx: TaskIndex::SelfIdx,
                                    domain: ObjectArgumentDomain::Objects,
                                    arg_idx: 0,
                                },
                                target_host: HostTarget::Owner(ObjectArgumentRef {
                                    node_idx: TaskIndex::SelfIdx,
                                    domain: ObjectArgumentDomain::Objects,
                                    arg_idx: 0,
                                }),
                            }],
                        },
                        post_actions: PostActionBlock::default(),
                    },
                ],
            },
        ),
        (
            "sort-collection-bulk-any",
            ParsedPhysicalPlan {
                nodes: vec![
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Idx(1),
                        intent_name: "sorting::sort_collection_u64".to_string(),
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::Idx(1),
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 0,
                        }),
                        pre_actions: ActionBlock::default(),
                        post_actions: PostActionBlock::default(),
                    },
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Idx(2),
                        intent_name: "sorting::sorting::sort_collection_bulk".to_string(),
                        pre_actions: ActionBlock::default(),
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::SelfIdx,
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 1,
                        }),
                        post_actions: PostActionBlock::default(),
                    },
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Any,
                        intent_name: "sorting::sorting::sort_input_block".to_string(),
                        pre_actions: ActionBlock {
                            dependencies: Vec::default(),
                            ownership_transfers: Vec::default(),
                            push_copies: vec![
                                RangedLocaticsSpecifier {
                                    range: RangeOver::NoRange,
                                    specifier: LocaticsSpecifier {
                                        target_object: ObjectArgumentRef {
                                            node_idx: TaskIndex::SelfIdx,
                                            domain: ObjectArgumentDomain::Arguments,
                                            arg_idx: 0,
                                        },
                                        target_host: HostTarget::Owner(ObjectArgumentRef {
                                            node_idx: TaskIndex::SelfIdx,
                                            domain: ObjectArgumentDomain::Arguments,
                                            arg_idx: 1,
                                        }),
                                    },
                                },
                                RangedLocaticsSpecifier {
                                    range: RangeOver::NoRange,
                                    specifier: LocaticsSpecifier {
                                        target_object: ObjectArgumentRef {
                                            node_idx: TaskIndex::SelfIdx,
                                            domain: ObjectArgumentDomain::Arguments,
                                            arg_idx: 2,
                                        },
                                        target_host: HostTarget::Owner(ObjectArgumentRef {
                                            node_idx: TaskIndex::SelfIdx,
                                            domain: ObjectArgumentDomain::Arguments,
                                            arg_idx: 1,
                                        }),
                                    },
                                },
                            ],
                            moves: Vec::default(),
                        },
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::SelfIdx,
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 1,
                        }),
                        post_actions: PostActionBlock::default(),
                    },
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Any,
                        intent_name: "sorting::sorting::spawn_merge_tasks".to_string(),
                        pre_actions: ActionBlock::default(),
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::SelfIdx,
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 0,
                        }),
                        post_actions: PostActionBlock::default(),
                    },
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Any,
                        intent_name: "sorting::sorting::spawn_merge_blocks".to_string(),
                        pre_actions: ActionBlock::default(),
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::SelfIdx,
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 0,
                        }),
                        post_actions: PostActionBlock::default(),
                    },
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Any,
                        intent_name: "sorting::sorting::merge_blocks".to_string(),
                        pre_actions: ActionBlock {
                            ownership_transfers: vec![LocaticsSpecifier {
                                target_object: ObjectArgumentRef {
                                    node_idx: TaskIndex::SelfIdx,
                                    domain: ObjectArgumentDomain::Objects,
                                    arg_idx: 1,
                                },
                                target_host: HostTarget::Owner(ObjectArgumentRef {
                                    node_idx: TaskIndex::SelfIdx,
                                    domain: ObjectArgumentDomain::Arguments,
                                    arg_idx: 0,
                                }),
                            }],
                            moves: vec![LocaticsSpecifier {
                                target_object: ObjectArgumentRef {
                                    node_idx: TaskIndex::SelfIdx,
                                    domain: ObjectArgumentDomain::Objects,
                                    arg_idx: 1,
                                },
                                target_host: HostTarget::Owner(ObjectArgumentRef {
                                    node_idx: TaskIndex::SelfIdx,
                                    domain: ObjectArgumentDomain::Objects,
                                    arg_idx: 1,
                                }),
                            }],
                            push_copies: Vec::default(),
                            dependencies: Vec::default(),
                        },
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::SelfIdx,
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 0,
                        }),
                        post_actions: PostActionBlock::default(),
                    },
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Any,
                        intent_name: "sorting::sorting::add_sorted_block_to_collection".to_string(),
                        pre_actions: ActionBlock::default(),
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::SelfIdx,
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 0,
                        }),
                        post_actions: PostActionBlock::default(),
                    },
                ],
            },
        ),
        (
            "sort-collection-fold-any",
            ParsedPhysicalPlan {
                nodes: vec![
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Idx(1),
                        intent_name: "sorting::sort_collection_u64".to_string(),
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::Idx(1),
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 0,
                        }),
                        pre_actions: ActionBlock::default(),
                        post_actions: PostActionBlock::default(),
                    },
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Idx(2),
                        intent_name: "sorting::sorting::sort_collection_bulk".to_string(),
                        pre_actions: ActionBlock::default(),
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::SelfIdx,
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 1,
                        }),
                        post_actions: PostActionBlock::default(),
                    },
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Any,
                        intent_name: "sorting::sorting::sort_input_block".to_string(),
                        pre_actions: ActionBlock {
                            dependencies: Vec::default(),
                            ownership_transfers: Vec::default(),
                            push_copies: vec![
                                RangedLocaticsSpecifier {
                                    range: RangeOver::NoRange,
                                    specifier: LocaticsSpecifier {
                                        target_object: ObjectArgumentRef {
                                            node_idx: TaskIndex::SelfIdx,
                                            domain: ObjectArgumentDomain::Arguments,
                                            arg_idx: 0,
                                        },
                                        target_host: HostTarget::Owner(ObjectArgumentRef {
                                            node_idx: TaskIndex::SelfIdx,
                                            domain: ObjectArgumentDomain::Arguments,
                                            arg_idx: 1,
                                        }),
                                    },
                                },
                                RangedLocaticsSpecifier {
                                    range: RangeOver::NoRange,
                                    specifier: LocaticsSpecifier {
                                        target_object: ObjectArgumentRef {
                                            node_idx: TaskIndex::SelfIdx,
                                            domain: ObjectArgumentDomain::Arguments,
                                            arg_idx: 2,
                                        },
                                        target_host: HostTarget::Owner(ObjectArgumentRef {
                                            node_idx: TaskIndex::SelfIdx,
                                            domain: ObjectArgumentDomain::Arguments,
                                            arg_idx: 1,
                                        }),
                                    },
                                },
                            ],
                            moves: Vec::default(),
                        },
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::SelfIdx,
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 1,
                        }),
                        post_actions: PostActionBlock::default(),
                    },
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Any,
                        intent_name: "sorting::sorting::spawn_merge_tasks".to_string(),
                        pre_actions: ActionBlock::default(),
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::SelfIdx,
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 0,
                        }),
                        post_actions: PostActionBlock::default(),
                    },
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Any,
                        intent_name: "sorting::sorting::spawn_merge_blocks".to_string(),
                        pre_actions: ActionBlock::default(),
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::SelfIdx,
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 0,
                        }),
                        post_actions: PostActionBlock::default(),
                    },
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Any,
                        intent_name: "sorting::sorting::foldable_merge_blocks".to_string(),
                        pre_actions: ActionBlock {
                            ownership_transfers: vec![LocaticsSpecifier {
                                target_object: ObjectArgumentRef {
                                    node_idx: TaskIndex::SelfIdx,
                                    domain: ObjectArgumentDomain::Arguments,
                                    arg_idx: 0,
                                },
                                target_host: HostTarget::HostIdx(0),
                            }],
                            moves: vec![LocaticsSpecifier {
                                target_object: ObjectArgumentRef {
                                    node_idx: TaskIndex::SelfIdx,
                                    domain: ObjectArgumentDomain::Arguments,
                                    arg_idx: 0,
                                },
                                target_host: HostTarget::Owner(ObjectArgumentRef {
                                    node_idx: TaskIndex::SelfIdx,
                                    domain: ObjectArgumentDomain::Arguments,
                                    arg_idx: 0,
                                }),
                            }],
                            push_copies: Vec::default(),
                            dependencies: Vec::default(),
                        },
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::SelfIdx,
                            domain: ObjectArgumentDomain::Objects,
                            arg_idx: 0,
                        }),
                        post_actions: PostActionBlock::default(),
                    },
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Any,
                        intent_name: "sorting::sorting::add_sorted_block_to_collection".to_string(),
                        pre_actions: ActionBlock::default(),
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::SelfIdx,
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 0,
                        }),
                        post_actions: PostActionBlock::default(),
                    },
                ],
            },
        ),
        (
            "sort-collection-bulk-hash",
            ParsedPhysicalPlan {
                nodes: vec![
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Idx(1),
                        intent_name: "sorting::sort_collection_u64".to_string(),
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::Idx(1),
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 0,
                        }),
                        pre_actions: ActionBlock::default(),
                        post_actions: PostActionBlock::default(),
                    },
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Idx(2),
                        intent_name: "sorting::sorting::sort_collection_bulk".to_string(),
                        pre_actions: ActionBlock::default(),
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::SelfIdx,
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 1,
                        }),
                        post_actions: PostActionBlock::default(),
                    },
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Any,
                        intent_name: "sorting::sorting::sort_input_block".to_string(),
                        pre_actions: ActionBlock {
                            dependencies: Vec::default(),
                            ownership_transfers: Vec::default(),
                            push_copies: vec![
                                RangedLocaticsSpecifier {
                                    range: RangeOver::NoRange,
                                    specifier: LocaticsSpecifier {
                                        target_object: ObjectArgumentRef {
                                            node_idx: TaskIndex::SelfIdx,
                                            domain: ObjectArgumentDomain::Arguments,
                                            arg_idx: 0,
                                        },
                                        target_host: HostTarget::Owner(ObjectArgumentRef {
                                            node_idx: TaskIndex::SelfIdx,
                                            domain: ObjectArgumentDomain::Arguments,
                                            arg_idx: 1,
                                        }),
                                    },
                                },
                                RangedLocaticsSpecifier {
                                    range: RangeOver::NoRange,
                                    specifier: LocaticsSpecifier {
                                        target_object: ObjectArgumentRef {
                                            node_idx: TaskIndex::SelfIdx,
                                            domain: ObjectArgumentDomain::Arguments,
                                            arg_idx: 2,
                                        },
                                        target_host: HostTarget::Owner(ObjectArgumentRef {
                                            node_idx: TaskIndex::SelfIdx,
                                            domain: ObjectArgumentDomain::Arguments,
                                            arg_idx: 1,
                                        }),
                                    },
                                },
                            ],
                            moves: Vec::default(),
                        },
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::SelfIdx,
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 1,
                        }),
                        post_actions: PostActionBlock::default(),
                    },
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Any,
                        intent_name: "sorting::sorting::spawn_merge_tasks".to_string(),
                        pre_actions: ActionBlock::default(),
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::SelfIdx,
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 0,
                        }),
                        post_actions: PostActionBlock::default(),
                    },
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Any,
                        intent_name: "sorting::sorting::spawn_merge_blocks".to_string(),
                        pre_actions: ActionBlock::default(),
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::SelfIdx,
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 0,
                        }),
                        post_actions: PostActionBlock::default(),
                    },
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Any,
                        intent_name: "sorting::sorting::merge_blocks".to_string(),
                        pre_actions: ActionBlock {
                            ownership_transfers: vec![
                                LocaticsSpecifier {
                                    target_object: ObjectArgumentRef {
                                        node_idx: TaskIndex::SelfIdx,
                                        domain: ObjectArgumentDomain::Arguments,
                                        arg_idx: 0,
                                    },
                                    target_host: HostTarget::EvalResult(
                                        PredefinedTargetFunction::HashTaskIdx,
                                    ),
                                },
                                LocaticsSpecifier {
                                    target_object: ObjectArgumentRef {
                                        node_idx: TaskIndex::SelfIdx,
                                        domain: ObjectArgumentDomain::Objects,
                                        arg_idx: 1,
                                    },
                                    target_host: HostTarget::EvalResult(
                                        PredefinedTargetFunction::HashTaskIdx,
                                    ),
                                },
                            ],
                            moves: vec![
                                LocaticsSpecifier {
                                    target_object: ObjectArgumentRef {
                                        node_idx: TaskIndex::SelfIdx,
                                        domain: ObjectArgumentDomain::Arguments,
                                        arg_idx: 0,
                                    },
                                    target_host: HostTarget::Owner(ObjectArgumentRef {
                                        node_idx: TaskIndex::SelfIdx,
                                        domain: ObjectArgumentDomain::Arguments,
                                        arg_idx: 0,
                                    }),
                                },
                                LocaticsSpecifier {
                                    target_object: ObjectArgumentRef {
                                        node_idx: TaskIndex::SelfIdx,
                                        domain: ObjectArgumentDomain::Objects,
                                        arg_idx: 1,
                                    },
                                    target_host: HostTarget::Owner(ObjectArgumentRef {
                                        node_idx: TaskIndex::SelfIdx,
                                        domain: ObjectArgumentDomain::Objects,
                                        arg_idx: 1,
                                    }),
                                },
                            ],
                            push_copies: Vec::default(),
                            dependencies: Vec::default(),
                        },
                        schedule_on: ActivationTarget::EvalResult(
                            PredefinedTargetFunction::HashTaskIdx,
                        ),
                        post_actions: PostActionBlock::default(),
                    },
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Any,
                        intent_name: "sorting::sorting::add_sorted_block_to_collection".to_string(),
                        pre_actions: ActionBlock::default(),
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::SelfIdx,
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 0,
                        }),
                        post_actions: PostActionBlock::default(),
                    },
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Any,
                        intent_name: "sorting::sorting::add_sorted_blocks_to_collections"
                            .to_string(),
                        pre_actions: ActionBlock::default(),
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::SelfIdx,
                            domain: ObjectArgumentDomain::Objects,
                            arg_idx: 0,
                        }),
                        post_actions: PostActionBlock::default(),
                    },
                ],
            },
        ),
        (
            "sort-collection-fold-hash",
            ParsedPhysicalPlan {
                nodes: vec![
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Idx(1),
                        intent_name: "sorting::sort_collection_u64".to_string(),
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::Idx(1),
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 0,
                        }),
                        pre_actions: ActionBlock::default(),
                        post_actions: PostActionBlock::default(),
                    },
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Idx(2),
                        intent_name: "sorting::sorting::sort_collection_bulk".to_string(),
                        pre_actions: ActionBlock::default(),
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::SelfIdx,
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 1,
                        }),
                        post_actions: PostActionBlock::default(),
                    },
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Any,
                        intent_name: "sorting::sorting::sort_input_block".to_string(),
                        pre_actions: ActionBlock {
                            dependencies: Vec::default(),
                            ownership_transfers: Vec::default(),
                            push_copies: vec![
                                RangedLocaticsSpecifier {
                                    range: RangeOver::NoRange,
                                    specifier: LocaticsSpecifier {
                                        target_object: ObjectArgumentRef {
                                            node_idx: TaskIndex::SelfIdx,
                                            domain: ObjectArgumentDomain::Arguments,
                                            arg_idx: 0,
                                        },
                                        target_host: HostTarget::Owner(ObjectArgumentRef {
                                            node_idx: TaskIndex::SelfIdx,
                                            domain: ObjectArgumentDomain::Arguments,
                                            arg_idx: 1,
                                        }),
                                    },
                                },
                                RangedLocaticsSpecifier {
                                    range: RangeOver::NoRange,
                                    specifier: LocaticsSpecifier {
                                        target_object: ObjectArgumentRef {
                                            node_idx: TaskIndex::SelfIdx,
                                            domain: ObjectArgumentDomain::Arguments,
                                            arg_idx: 2,
                                        },
                                        target_host: HostTarget::Owner(ObjectArgumentRef {
                                            node_idx: TaskIndex::SelfIdx,
                                            domain: ObjectArgumentDomain::Arguments,
                                            arg_idx: 1,
                                        }),
                                    },
                                },
                            ],
                            moves: Vec::default(),
                        },
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::SelfIdx,
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 1,
                        }),
                        post_actions: PostActionBlock::default(),
                    },
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Any,
                        intent_name: "sorting::sorting::spawn_merge_tasks".to_string(),
                        pre_actions: ActionBlock::default(),
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::SelfIdx,
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 0,
                        }),
                        post_actions: PostActionBlock::default(),
                    },
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Any,
                        intent_name: "sorting::sorting::spawn_merge_blocks".to_string(),
                        pre_actions: ActionBlock::default(),
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::SelfIdx,
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 0,
                        }),
                        post_actions: PostActionBlock::default(),
                    },
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Any,
                        intent_name: "sorting::sorting::foldable_merge_blocks".to_string(),
                        pre_actions: ActionBlock {
                            ownership_transfers: vec![LocaticsSpecifier {
                                target_object: ObjectArgumentRef {
                                    node_idx: TaskIndex::SelfIdx,
                                    domain: ObjectArgumentDomain::Arguments,
                                    arg_idx: 0,
                                },
                                target_host: HostTarget::EvalResult(
                                    PredefinedTargetFunction::HashTaskIdx,
                                ),
                            }],
                            moves: vec![LocaticsSpecifier {
                                target_object: ObjectArgumentRef {
                                    node_idx: TaskIndex::SelfIdx,
                                    domain: ObjectArgumentDomain::Arguments,
                                    arg_idx: 0,
                                },
                                target_host: HostTarget::Owner(ObjectArgumentRef {
                                    node_idx: TaskIndex::SelfIdx,
                                    domain: ObjectArgumentDomain::Arguments,
                                    arg_idx: 0,
                                }),
                            }],
                            push_copies: Vec::default(),
                            dependencies: Vec::default(),
                        },
                        schedule_on: ActivationTarget::EvalResult(
                            PredefinedTargetFunction::HashTaskIdx,
                        ),
                        post_actions: PostActionBlock::default(),
                    },
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Any,
                        intent_name: "sorting::sorting::add_sorted_block_to_collection".to_string(),
                        pre_actions: ActionBlock::default(),
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::SelfIdx,
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 0,
                        }),
                        post_actions: PostActionBlock::default(),
                    },
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Any,
                        intent_name: "sorting::sorting::add_sorted_blocks_to_collections"
                            .to_string(),
                        pre_actions: ActionBlock::default(),
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::SelfIdx,
                            domain: ObjectArgumentDomain::Objects,
                            arg_idx: 0,
                        }),
                        post_actions: PostActionBlock::default(),
                    },
                ],
            },
        ),
        (
            "graph-repartition",
            ParsedPhysicalPlan {
                nodes: vec![
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Idx(1),
                        intent_name: "nano4r::chunk_partitions".to_string(),
                        // schedule on index object owner
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::SelfIdx,
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 0,
                        }),
                        pre_actions: ActionBlock::default(),
                        post_actions: PostActionBlock::default(),
                    },
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Any,
                        intent_name: "nano4r::visit_partition_chunk".to_string(),
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::SelfIdx,
                            domain: ObjectArgumentDomain::Objects,
                            arg_idx: 1,
                        }),
                        pre_actions: ActionBlock {
                            dependencies: Vec::default(),
                            ownership_transfers: vec![
                                // partitions
                                LocaticsSpecifier {
                                    target_object: ObjectArgumentRef {
                                        node_idx: TaskIndex::SelfIdx,
                                        domain: ObjectArgumentDomain::Objects,
                                        arg_idx: 1,
                                    },
                                    target_host: HostTarget::EvalResult(
                                        PredefinedTargetFunction::SelfIdxOffset,
                                    ),
                                },
                                // optionally, foreign vertex degrees
                                LocaticsSpecifier {
                                    target_object: ObjectArgumentRef {
                                        node_idx: TaskIndex::SelfIdx,
                                        domain: ObjectArgumentDomain::Objects,
                                        arg_idx: 2,
                                    },
                                    target_host: HostTarget::EvalResult(
                                        PredefinedTargetFunction::SelfIdxOffset,
                                    ),
                                },
                                // optionally, incoming edges
                                LocaticsSpecifier {
                                    target_object: ObjectArgumentRef {
                                        node_idx: TaskIndex::SelfIdx,
                                        domain: ObjectArgumentDomain::Objects,
                                        arg_idx: 3,
                                    },
                                    target_host: HostTarget::EvalResult(
                                        PredefinedTargetFunction::SelfIdxOffset,
                                    ),
                                },
                            ],
                            push_copies: vec![RangedLocaticsSpecifier {
                                range: RangeOver::NoRange,
                                specifier: LocaticsSpecifier {
                                    target_object: ObjectArgumentRef {
                                        node_idx: TaskIndex::SelfIdx,
                                        domain: ObjectArgumentDomain::Arguments,
                                        arg_idx: 0,
                                    },
                                    target_host: HostTarget::EvalResult(
                                        PredefinedTargetFunction::SelfIdxOffset,
                                    ),
                                },
                            }],
                            moves: vec![
                                LocaticsSpecifier {
                                    target_object: ObjectArgumentRef {
                                        node_idx: TaskIndex::SelfIdx,
                                        domain: ObjectArgumentDomain::Objects,
                                        arg_idx: 1,
                                    },
                                    target_host: HostTarget::EvalResult(
                                        PredefinedTargetFunction::SelfIdxOffset,
                                    ),
                                },
                                LocaticsSpecifier {
                                    target_object: ObjectArgumentRef {
                                        node_idx: TaskIndex::SelfIdx,
                                        domain: ObjectArgumentDomain::Objects,
                                        arg_idx: 2,
                                    },
                                    target_host: HostTarget::EvalResult(
                                        PredefinedTargetFunction::SelfIdxOffset,
                                    ),
                                },
                                LocaticsSpecifier {
                                    target_object: ObjectArgumentRef {
                                        node_idx: TaskIndex::SelfIdx,
                                        domain: ObjectArgumentDomain::Objects,
                                        arg_idx: 3,
                                    },
                                    target_host: HostTarget::EvalResult(
                                        PredefinedTargetFunction::SelfIdxOffset,
                                    ),
                                },
                            ],
                        },
                        post_actions: PostActionBlock {
                            append_to_current_task: true,
                            action_block: ActionBlock {
                                push_copies: vec![
                                    RangedLocaticsSpecifier {
                                        range: RangeOver::NoRange,
                                        specifier: LocaticsSpecifier {
                                            target_object: ObjectArgumentRef {
                                                node_idx: TaskIndex::SelfIdx,
                                                domain: ObjectArgumentDomain::Objects,
                                                arg_idx: 1,
                                            },
                                            target_host: HostTarget::NonOwners(ObjectArgumentRef {
                                                node_idx: TaskIndex::SelfIdx,
                                                domain: ObjectArgumentDomain::Objects,
                                                arg_idx: 1,
                                            }),
                                        },
                                    },
                                    RangedLocaticsSpecifier {
                                        range: RangeOver::NoRange,
                                        specifier: LocaticsSpecifier {
                                            target_object: ObjectArgumentRef {
                                                node_idx: TaskIndex::SelfIdx,
                                                domain: ObjectArgumentDomain::Objects,
                                                arg_idx: 2,
                                            },
                                            target_host: HostTarget::NonOwners(ObjectArgumentRef {
                                                node_idx: TaskIndex::SelfIdx,
                                                domain: ObjectArgumentDomain::Objects,
                                                arg_idx: 2,
                                            }),
                                        },
                                    },
                                    RangedLocaticsSpecifier {
                                        range: RangeOver::NoRange,
                                        specifier: LocaticsSpecifier {
                                            target_object: ObjectArgumentRef {
                                                node_idx: TaskIndex::SelfIdx,
                                                domain: ObjectArgumentDomain::Objects,
                                                arg_idx: 3,
                                            },
                                            target_host: HostTarget::NonOwners(ObjectArgumentRef {
                                                node_idx: TaskIndex::SelfIdx,
                                                domain: ObjectArgumentDomain::Objects,
                                                arg_idx: 3,
                                            }),
                                        },
                                    },
                                ],
                                ownership_transfers: Vec::default(),
                                moves: Vec::default(),
                                dependencies: Vec::default(),
                            },
                        },
                    },
                ],
            },
        ),
        (
            "pagerank-even-split",
            ParsedPhysicalPlan {
                nodes: vec![
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Idx(1),
                        intent_name: "nano4r::launch_pagerank_to_convergence".to_string(),
                        // schedule on graph manifest owner
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::Idx(1),
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 0,
                        }),
                        pre_actions: ActionBlock::default(),
                        post_actions: PostActionBlock::default(),
                    },
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Any,
                        intent_name: "nano4r::init_partition_state".to_string(),
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::SelfIdx,
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 3,
                        }),
                        pre_actions: ActionBlock {
                            dependencies: Vec::default(),
                            ownership_transfers: vec![LocaticsSpecifier {
                                target_object: ObjectArgumentRef {
                                    node_idx: TaskIndex::SelfIdx,
                                    domain: ObjectArgumentDomain::Arguments,
                                    arg_idx: 3,
                                },
                                target_host: HostTarget::Owner(ObjectArgumentRef {
                                    node_idx: TaskIndex::SelfIdx,
                                    domain: ObjectArgumentDomain::Arguments,
                                    arg_idx: 1,
                                }),
                            }],
                            push_copies: Vec::default(),
                            moves: vec![LocaticsSpecifier {
                                target_object: ObjectArgumentRef {
                                    node_idx: TaskIndex::SelfIdx,
                                    domain: ObjectArgumentDomain::Arguments,
                                    arg_idx: 3,
                                },
                                target_host: HostTarget::Owner(ObjectArgumentRef {
                                    node_idx: TaskIndex::SelfIdx,
                                    domain: ObjectArgumentDomain::Arguments,
                                    arg_idx: 3,
                                }),
                            }],
                        },
                        post_actions: PostActionBlock::default(),
                    },
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Any,
                        intent_name: "nano4r::pagerank_iter_or_stop".to_string(),
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::SelfIdx,
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 1,
                        }),
                        pre_actions: ActionBlock::default(),
                        post_actions: PostActionBlock::default(),
                    },
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Any,
                        intent_name: "nano4r::partition_round".to_string(),
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::SelfIdx,
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 1,
                        }),
                        pre_actions: ActionBlock::default(),
                        post_actions: PostActionBlock::default(),
                    },
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Any,
                        intent_name: "nano4r::partition_round_internal".to_string(),
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::SelfIdx,
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 1,
                        }),
                        pre_actions: ActionBlock::default(),
                        post_actions: PostActionBlock::default(),
                    },
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Any,
                        intent_name: "nano4r::mark_partition_converged".to_string(),
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::SelfIdx,
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 0,
                        }),
                        pre_actions: ActionBlock::default(),
                        post_actions: PostActionBlock::default(),
                    },
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Any,
                        intent_name: "nano4r::spawn_inter_partition_tasks".to_string(),
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::SelfIdx,
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 0,
                        }),
                        pre_actions: ActionBlock::default(),
                        post_actions: PostActionBlock::default(),
                    },
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Any,
                        intent_name: "nano4r::spawn_inter_partition_tasks_for_partition"
                            .to_string(),
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::SelfIdx,
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 1,
                        }),
                        pre_actions: ActionBlock {
                            dependencies: Vec::default(),
                            ownership_transfers: Vec::default(),
                            moves: Vec::default(),
                            push_copies: vec![RangedLocaticsSpecifier {
                                range: RangeOver::NoRange,
                                specifier: LocaticsSpecifier {
                                    target_object: ObjectArgumentRef {
                                        node_idx: TaskIndex::SelfIdx,
                                        domain: ObjectArgumentDomain::Arguments,
                                        arg_idx: 2,
                                    },
                                    target_host: HostTarget::Owner(ObjectArgumentRef {
                                        node_idx: TaskIndex::SelfIdx,
                                        domain: ObjectArgumentDomain::Arguments,
                                        arg_idx: 1,
                                    }),
                                },
                            }],
                        },
                        post_actions: PostActionBlock::default(),
                    },
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Any,
                        intent_name: "nano4r::inter_partition_rank_update".to_string(),
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::SelfIdx,
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 1,
                        }),
                        pre_actions: ActionBlock {
                            dependencies: Vec::default(),
                            ownership_transfers: Vec::default(),
                            moves: Vec::default(),
                            push_copies: vec![RangedLocaticsSpecifier {
                                range: RangeOver::NoRange,
                                specifier: LocaticsSpecifier {
                                    target_object: ObjectArgumentRef {
                                        node_idx: TaskIndex::SelfIdx,
                                        domain: ObjectArgumentDomain::Arguments,
                                        arg_idx: 2,
                                    },
                                    target_host: HostTarget::Owner(ObjectArgumentRef {
                                        node_idx: TaskIndex::SelfIdx,
                                        domain: ObjectArgumentDomain::Arguments,
                                        arg_idx: 1,
                                    }),
                                },
                            }],
                        },
                        post_actions: PostActionBlock::default(),
                    },
                ],
            },
        ),
        (
            "sw-4-non-step",
            ParsedPhysicalPlan {
                nodes: vec![
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Idx(1),
                        intent_name: "smith_waterman::init_smith_waterman".to_string(),
                        // ignored by manager
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::Idx(0),
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 0,
                        }),
                        pre_actions: ActionBlock::default(),
                        post_actions: PostActionBlock::default(),
                    },
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Any,
                        intent_name: "smith_waterman::process_block".to_string(),
                        // schedule on bucket object owner
                        schedule_on: ActivationTarget::EvalResult(
                            PredefinedTargetFunction::SplitAlongAntiDiagonal(4),
                        ),
                        pre_actions: ActionBlock {
                            dependencies: Vec::default(),
                            ownership_transfers: Vec::default(),
                            push_copies: vec![
                                RangedLocaticsSpecifier {
                                    range: RangeOver::NoRange,
                                    specifier: LocaticsSpecifier {
                                        target_object: ObjectArgumentRef {
                                            node_idx: TaskIndex::SelfIdx,
                                            domain: ObjectArgumentDomain::Arguments,
                                            arg_idx: 0,
                                        },
                                        target_host: HostTarget::EvalResult(
                                            PredefinedTargetFunction::SplitAlongAntiDiagonal(4),
                                        ),
                                    },
                                },
                                RangedLocaticsSpecifier {
                                    range: RangeOver::NoRange,
                                    specifier: LocaticsSpecifier {
                                        target_object: ObjectArgumentRef {
                                            node_idx: TaskIndex::SelfIdx,
                                            domain: ObjectArgumentDomain::Arguments,
                                            arg_idx: 6,
                                        },
                                        target_host: HostTarget::EvalResult(
                                            PredefinedTargetFunction::SplitAlongAntiDiagonal(4),
                                        ),
                                    },
                                },
                                RangedLocaticsSpecifier {
                                    range: RangeOver::NoRange,
                                    specifier: LocaticsSpecifier {
                                        target_object: ObjectArgumentRef {
                                            node_idx: TaskIndex::SelfIdx,
                                            domain: ObjectArgumentDomain::Arguments,
                                            arg_idx: 7,
                                        },
                                        target_host: HostTarget::EvalResult(
                                            PredefinedTargetFunction::SplitAlongAntiDiagonal(4),
                                        ),
                                    },
                                },
                                RangedLocaticsSpecifier {
                                    range: RangeOver::NoRange,
                                    specifier: LocaticsSpecifier {
                                        target_object: ObjectArgumentRef {
                                            node_idx: TaskIndex::SelfIdx,
                                            domain: ObjectArgumentDomain::Arguments,
                                            arg_idx: 8,
                                        },
                                        target_host: HostTarget::EvalResult(
                                            PredefinedTargetFunction::SplitAlongAntiDiagonal(4),
                                        ),
                                    },
                                },
                            ],
                            moves: Vec::default(),
                        },
                        post_actions: PostActionBlock::default(),
                    },
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Any,
                        intent_name: "smith_waterman::set_final_value".to_string(),
                        // schedule on state object owner
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::SelfIdx,
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 0,
                        }),
                        pre_actions: ActionBlock::default(),
                        post_actions: PostActionBlock::default(),
                    },
                ],
            },
        ),
        (
            "sw-8-non-step",
            ParsedPhysicalPlan {
                nodes: vec![
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Idx(1),
                        intent_name: "smith_waterman::init_smith_waterman".to_string(),
                        // ignored by manager
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::Idx(0),
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 0,
                        }),
                        pre_actions: ActionBlock::default(),
                        post_actions: PostActionBlock::default(),
                    },
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Any,
                        intent_name: "smith_waterman::process_block".to_string(),
                        // schedule on bucket object owner
                        schedule_on: ActivationTarget::EvalResult(
                            PredefinedTargetFunction::SplitAlongAntiDiagonal(8),
                        ),
                        pre_actions: ActionBlock {
                            dependencies: Vec::default(),
                            ownership_transfers: Vec::default(),
                            push_copies: vec![
                                RangedLocaticsSpecifier {
                                    range: RangeOver::NoRange,
                                    specifier: LocaticsSpecifier {
                                        target_object: ObjectArgumentRef {
                                            node_idx: TaskIndex::SelfIdx,
                                            domain: ObjectArgumentDomain::Arguments,
                                            arg_idx: 0,
                                        },
                                        target_host: HostTarget::EvalResult(
                                            PredefinedTargetFunction::SplitAlongAntiDiagonal(8),
                                        ),
                                    },
                                },
                                RangedLocaticsSpecifier {
                                    range: RangeOver::NoRange,
                                    specifier: LocaticsSpecifier {
                                        target_object: ObjectArgumentRef {
                                            node_idx: TaskIndex::SelfIdx,
                                            domain: ObjectArgumentDomain::Arguments,
                                            arg_idx: 6,
                                        },
                                        target_host: HostTarget::EvalResult(
                                            PredefinedTargetFunction::SplitAlongAntiDiagonal(8),
                                        ),
                                    },
                                },
                                RangedLocaticsSpecifier {
                                    range: RangeOver::NoRange,
                                    specifier: LocaticsSpecifier {
                                        target_object: ObjectArgumentRef {
                                            node_idx: TaskIndex::SelfIdx,
                                            domain: ObjectArgumentDomain::Arguments,
                                            arg_idx: 7,
                                        },
                                        target_host: HostTarget::EvalResult(
                                            PredefinedTargetFunction::SplitAlongAntiDiagonal(8),
                                        ),
                                    },
                                },
                                RangedLocaticsSpecifier {
                                    range: RangeOver::NoRange,
                                    specifier: LocaticsSpecifier {
                                        target_object: ObjectArgumentRef {
                                            node_idx: TaskIndex::SelfIdx,
                                            domain: ObjectArgumentDomain::Arguments,
                                            arg_idx: 8,
                                        },
                                        target_host: HostTarget::EvalResult(
                                            PredefinedTargetFunction::SplitAlongAntiDiagonal(8),
                                        ),
                                    },
                                },
                            ],
                            moves: Vec::default(),
                        },
                        post_actions: PostActionBlock::default(),
                    },
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Any,
                        intent_name: "smith_waterman::set_final_value".to_string(),
                        // schedule on state object owner
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::SelfIdx,
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 0,
                        }),
                        pre_actions: ActionBlock::default(),
                        post_actions: PostActionBlock::default(),
                    },
                ],
            },
        ),
        (
            "sw-16-non-step",
            ParsedPhysicalPlan {
                nodes: vec![
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Idx(1),
                        intent_name: "smith_waterman::init_smith_waterman".to_string(),
                        // ignored by manager
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::Idx(0),
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 0,
                        }),
                        pre_actions: ActionBlock::default(),
                        post_actions: PostActionBlock::default(),
                    },
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Any,
                        intent_name: "smith_waterman::process_block".to_string(),
                        // schedule on bucket object owner
                        schedule_on: ActivationTarget::EvalResult(
                            PredefinedTargetFunction::SplitAlongAntiDiagonal(16),
                        ),
                        pre_actions: ActionBlock {
                            dependencies: Vec::default(),
                            ownership_transfers: Vec::default(),
                            push_copies: vec![
                                RangedLocaticsSpecifier {
                                    range: RangeOver::NoRange,
                                    specifier: LocaticsSpecifier {
                                        target_object: ObjectArgumentRef {
                                            node_idx: TaskIndex::SelfIdx,
                                            domain: ObjectArgumentDomain::Arguments,
                                            arg_idx: 0,
                                        },
                                        target_host: HostTarget::EvalResult(
                                            PredefinedTargetFunction::SplitAlongAntiDiagonal(16),
                                        ),
                                    },
                                },
                                RangedLocaticsSpecifier {
                                    range: RangeOver::NoRange,
                                    specifier: LocaticsSpecifier {
                                        target_object: ObjectArgumentRef {
                                            node_idx: TaskIndex::SelfIdx,
                                            domain: ObjectArgumentDomain::Arguments,
                                            arg_idx: 6,
                                        },
                                        target_host: HostTarget::EvalResult(
                                            PredefinedTargetFunction::SplitAlongAntiDiagonal(16),
                                        ),
                                    },
                                },
                                RangedLocaticsSpecifier {
                                    range: RangeOver::NoRange,
                                    specifier: LocaticsSpecifier {
                                        target_object: ObjectArgumentRef {
                                            node_idx: TaskIndex::SelfIdx,
                                            domain: ObjectArgumentDomain::Arguments,
                                            arg_idx: 7,
                                        },
                                        target_host: HostTarget::EvalResult(
                                            PredefinedTargetFunction::SplitAlongAntiDiagonal(16),
                                        ),
                                    },
                                },
                                RangedLocaticsSpecifier {
                                    range: RangeOver::NoRange,
                                    specifier: LocaticsSpecifier {
                                        target_object: ObjectArgumentRef {
                                            node_idx: TaskIndex::SelfIdx,
                                            domain: ObjectArgumentDomain::Arguments,
                                            arg_idx: 8,
                                        },
                                        target_host: HostTarget::EvalResult(
                                            PredefinedTargetFunction::SplitAlongAntiDiagonal(16),
                                        ),
                                    },
                                },
                            ],
                            moves: Vec::default(),
                        },
                        post_actions: PostActionBlock::default(),
                    },
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Any,
                        intent_name: "smith_waterman::set_final_value".to_string(),
                        // schedule on state object owner
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::SelfIdx,
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 0,
                        }),
                        pre_actions: ActionBlock::default(),
                        post_actions: PostActionBlock::default(),
                    },
                ],
            },
        ),
        (
            "sw-32-non-step",
            ParsedPhysicalPlan {
                nodes: vec![
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Idx(1),
                        intent_name: "smith_waterman::init_smith_waterman".to_string(),
                        // ignored by manager
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::Idx(0),
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 0,
                        }),
                        pre_actions: ActionBlock::default(),
                        post_actions: PostActionBlock::default(),
                    },
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Any,
                        intent_name: "smith_waterman::process_block".to_string(),
                        // schedule on bucket object owner
                        schedule_on: ActivationTarget::EvalResult(
                            PredefinedTargetFunction::SplitAlongAntiDiagonal(32),
                        ),
                        pre_actions: ActionBlock {
                            dependencies: Vec::default(),
                            ownership_transfers: Vec::default(),
                            push_copies: vec![
                                RangedLocaticsSpecifier {
                                    range: RangeOver::NoRange,
                                    specifier: LocaticsSpecifier {
                                        target_object: ObjectArgumentRef {
                                            node_idx: TaskIndex::SelfIdx,
                                            domain: ObjectArgumentDomain::Arguments,
                                            arg_idx: 0,
                                        },
                                        target_host: HostTarget::EvalResult(
                                            PredefinedTargetFunction::SplitAlongAntiDiagonal(32),
                                        ),
                                    },
                                },
                                RangedLocaticsSpecifier {
                                    range: RangeOver::NoRange,
                                    specifier: LocaticsSpecifier {
                                        target_object: ObjectArgumentRef {
                                            node_idx: TaskIndex::SelfIdx,
                                            domain: ObjectArgumentDomain::Arguments,
                                            arg_idx: 6,
                                        },
                                        target_host: HostTarget::EvalResult(
                                            PredefinedTargetFunction::SplitAlongAntiDiagonal(32),
                                        ),
                                    },
                                },
                                RangedLocaticsSpecifier {
                                    range: RangeOver::NoRange,
                                    specifier: LocaticsSpecifier {
                                        target_object: ObjectArgumentRef {
                                            node_idx: TaskIndex::SelfIdx,
                                            domain: ObjectArgumentDomain::Arguments,
                                            arg_idx: 7,
                                        },
                                        target_host: HostTarget::EvalResult(
                                            PredefinedTargetFunction::SplitAlongAntiDiagonal(32),
                                        ),
                                    },
                                },
                                RangedLocaticsSpecifier {
                                    range: RangeOver::NoRange,
                                    specifier: LocaticsSpecifier {
                                        target_object: ObjectArgumentRef {
                                            node_idx: TaskIndex::SelfIdx,
                                            domain: ObjectArgumentDomain::Arguments,
                                            arg_idx: 8,
                                        },
                                        target_host: HostTarget::EvalResult(
                                            PredefinedTargetFunction::SplitAlongAntiDiagonal(32),
                                        ),
                                    },
                                },
                            ],
                            moves: Vec::default(),
                        },
                        post_actions: PostActionBlock::default(),
                    },
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Any,
                        intent_name: "smith_waterman::set_final_value".to_string(),
                        // schedule on state object owner
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::SelfIdx,
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 0,
                        }),
                        pre_actions: ActionBlock::default(),
                        post_actions: PostActionBlock::default(),
                    },
                ],
            },
        ),
        (
            "sw-64-non-step",
            ParsedPhysicalPlan {
                nodes: vec![
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Idx(1),
                        intent_name: "smith_waterman::init_smith_waterman".to_string(),
                        // ignored by manager
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::Idx(0),
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 0,
                        }),
                        pre_actions: ActionBlock::default(),
                        post_actions: PostActionBlock::default(),
                    },
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Any,
                        intent_name: "smith_waterman::process_block".to_string(),
                        // schedule on bucket object owner
                        schedule_on: ActivationTarget::EvalResult(
                            PredefinedTargetFunction::SplitAlongAntiDiagonal(64),
                        ),
                        pre_actions: ActionBlock {
                            dependencies: Vec::default(),
                            ownership_transfers: Vec::default(),
                            push_copies: vec![
                                RangedLocaticsSpecifier {
                                    range: RangeOver::NoRange,
                                    specifier: LocaticsSpecifier {
                                        target_object: ObjectArgumentRef {
                                            node_idx: TaskIndex::SelfIdx,
                                            domain: ObjectArgumentDomain::Arguments,
                                            arg_idx: 0,
                                        },
                                        target_host: HostTarget::EvalResult(
                                            PredefinedTargetFunction::SplitAlongAntiDiagonal(64),
                                        ),
                                    },
                                },
                                RangedLocaticsSpecifier {
                                    range: RangeOver::NoRange,
                                    specifier: LocaticsSpecifier {
                                        target_object: ObjectArgumentRef {
                                            node_idx: TaskIndex::SelfIdx,
                                            domain: ObjectArgumentDomain::Arguments,
                                            arg_idx: 6,
                                        },
                                        target_host: HostTarget::EvalResult(
                                            PredefinedTargetFunction::SplitAlongAntiDiagonal(64),
                                        ),
                                    },
                                },
                                RangedLocaticsSpecifier {
                                    range: RangeOver::NoRange,
                                    specifier: LocaticsSpecifier {
                                        target_object: ObjectArgumentRef {
                                            node_idx: TaskIndex::SelfIdx,
                                            domain: ObjectArgumentDomain::Arguments,
                                            arg_idx: 7,
                                        },
                                        target_host: HostTarget::EvalResult(
                                            PredefinedTargetFunction::SplitAlongAntiDiagonal(64),
                                        ),
                                    },
                                },
                                RangedLocaticsSpecifier {
                                    range: RangeOver::NoRange,
                                    specifier: LocaticsSpecifier {
                                        target_object: ObjectArgumentRef {
                                            node_idx: TaskIndex::SelfIdx,
                                            domain: ObjectArgumentDomain::Arguments,
                                            arg_idx: 8,
                                        },
                                        target_host: HostTarget::EvalResult(
                                            PredefinedTargetFunction::SplitAlongAntiDiagonal(64),
                                        ),
                                    },
                                },
                            ],
                            moves: Vec::default(),
                        },
                        post_actions: PostActionBlock::default(),
                    },
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Any,
                        intent_name: "smith_waterman::set_final_value".to_string(),
                        // schedule on state object owner
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::SelfIdx,
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 0,
                        }),
                        pre_actions: ActionBlock::default(),
                        post_actions: PostActionBlock::default(),
                    },
                ],
            },
        ),
        (
            "sw-128-non-step",
            ParsedPhysicalPlan {
                nodes: vec![
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Idx(1),
                        intent_name: "smith_waterman::init_smith_waterman".to_string(),
                        // ignored by manager
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::Idx(0),
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 0,
                        }),
                        pre_actions: ActionBlock::default(),
                        post_actions: PostActionBlock::default(),
                    },
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Any,
                        intent_name: "smith_waterman::process_block".to_string(),
                        // schedule on bucket object owner
                        schedule_on: ActivationTarget::EvalResult(
                            PredefinedTargetFunction::SplitAlongAntiDiagonal(128),
                        ),
                        pre_actions: ActionBlock {
                            dependencies: Vec::default(),
                            ownership_transfers: Vec::default(),
                            push_copies: vec![
                                RangedLocaticsSpecifier {
                                    range: RangeOver::NoRange,
                                    specifier: LocaticsSpecifier {
                                        target_object: ObjectArgumentRef {
                                            node_idx: TaskIndex::SelfIdx,
                                            domain: ObjectArgumentDomain::Arguments,
                                            arg_idx: 0,
                                        },
                                        target_host: HostTarget::EvalResult(
                                            PredefinedTargetFunction::SplitAlongAntiDiagonal(128),
                                        ),
                                    },
                                },
                                RangedLocaticsSpecifier {
                                    range: RangeOver::NoRange,
                                    specifier: LocaticsSpecifier {
                                        target_object: ObjectArgumentRef {
                                            node_idx: TaskIndex::SelfIdx,
                                            domain: ObjectArgumentDomain::Arguments,
                                            arg_idx: 6,
                                        },
                                        target_host: HostTarget::EvalResult(
                                            PredefinedTargetFunction::SplitAlongAntiDiagonal(128),
                                        ),
                                    },
                                },
                                RangedLocaticsSpecifier {
                                    range: RangeOver::NoRange,
                                    specifier: LocaticsSpecifier {
                                        target_object: ObjectArgumentRef {
                                            node_idx: TaskIndex::SelfIdx,
                                            domain: ObjectArgumentDomain::Arguments,
                                            arg_idx: 7,
                                        },
                                        target_host: HostTarget::EvalResult(
                                            PredefinedTargetFunction::SplitAlongAntiDiagonal(128),
                                        ),
                                    },
                                },
                                RangedLocaticsSpecifier {
                                    range: RangeOver::NoRange,
                                    specifier: LocaticsSpecifier {
                                        target_object: ObjectArgumentRef {
                                            node_idx: TaskIndex::SelfIdx,
                                            domain: ObjectArgumentDomain::Arguments,
                                            arg_idx: 8,
                                        },
                                        target_host: HostTarget::EvalResult(
                                            PredefinedTargetFunction::SplitAlongAntiDiagonal(128),
                                        ),
                                    },
                                },
                            ],
                            moves: Vec::default(),
                        },
                        post_actions: PostActionBlock::default(),
                    },
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Any,
                        intent_name: "smith_waterman::set_final_value".to_string(),
                        // schedule on state object owner
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::SelfIdx,
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 0,
                        }),
                        pre_actions: ActionBlock::default(),
                        post_actions: PostActionBlock::default(),
                    },
                ],
            },
        ),
        (
            "sw-256-non-step",
            ParsedPhysicalPlan {
                nodes: vec![
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Idx(1),
                        intent_name: "smith_waterman::init_smith_waterman".to_string(),
                        // ignored by manager
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::Idx(0),
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 0,
                        }),
                        pre_actions: ActionBlock::default(),
                        post_actions: PostActionBlock::default(),
                    },
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Any,
                        intent_name: "smith_waterman::process_block".to_string(),
                        // schedule on bucket object owner
                        schedule_on: ActivationTarget::EvalResult(
                            PredefinedTargetFunction::SplitAlongAntiDiagonal(256),
                        ),
                        pre_actions: ActionBlock {
                            dependencies: Vec::default(),
                            ownership_transfers: Vec::default(),
                            push_copies: vec![
                                RangedLocaticsSpecifier {
                                    range: RangeOver::NoRange,
                                    specifier: LocaticsSpecifier {
                                        target_object: ObjectArgumentRef {
                                            node_idx: TaskIndex::SelfIdx,
                                            domain: ObjectArgumentDomain::Arguments,
                                            arg_idx: 0,
                                        },
                                        target_host: HostTarget::EvalResult(
                                            PredefinedTargetFunction::SplitAlongAntiDiagonal(256),
                                        ),
                                    },
                                },
                                RangedLocaticsSpecifier {
                                    range: RangeOver::NoRange,
                                    specifier: LocaticsSpecifier {
                                        target_object: ObjectArgumentRef {
                                            node_idx: TaskIndex::SelfIdx,
                                            domain: ObjectArgumentDomain::Arguments,
                                            arg_idx: 6,
                                        },
                                        target_host: HostTarget::EvalResult(
                                            PredefinedTargetFunction::SplitAlongAntiDiagonal(256),
                                        ),
                                    },
                                },
                                RangedLocaticsSpecifier {
                                    range: RangeOver::NoRange,
                                    specifier: LocaticsSpecifier {
                                        target_object: ObjectArgumentRef {
                                            node_idx: TaskIndex::SelfIdx,
                                            domain: ObjectArgumentDomain::Arguments,
                                            arg_idx: 7,
                                        },
                                        target_host: HostTarget::EvalResult(
                                            PredefinedTargetFunction::SplitAlongAntiDiagonal(256),
                                        ),
                                    },
                                },
                                RangedLocaticsSpecifier {
                                    range: RangeOver::NoRange,
                                    specifier: LocaticsSpecifier {
                                        target_object: ObjectArgumentRef {
                                            node_idx: TaskIndex::SelfIdx,
                                            domain: ObjectArgumentDomain::Arguments,
                                            arg_idx: 8,
                                        },
                                        target_host: HostTarget::EvalResult(
                                            PredefinedTargetFunction::SplitAlongAntiDiagonal(256),
                                        ),
                                    },
                                },
                            ],
                            moves: Vec::default(),
                        },
                        post_actions: PostActionBlock::default(),
                    },
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Any,
                        intent_name: "smith_waterman::set_final_value".to_string(),
                        // schedule on state object owner
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::SelfIdx,
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 0,
                        }),
                        pre_actions: ActionBlock::default(),
                        post_actions: PostActionBlock::default(),
                    },
                ],
            },
        ),
        (
            "sw-non-step-hash",
            ParsedPhysicalPlan {
                nodes: vec![
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Idx(1),
                        intent_name: "smith_waterman::init_smith_waterman".to_string(),
                        // ignored by manager
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::Idx(0),
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 0,
                        }),
                        pre_actions: ActionBlock::default(),
                        post_actions: PostActionBlock::default(),
                    },
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Any,
                        intent_name: "smith_waterman::process_block".to_string(),
                        // schedule on bucket object owner
                        schedule_on: ActivationTarget::EvalResult(
                            PredefinedTargetFunction::HashTaskIdx,
                        ),
                        pre_actions: ActionBlock {
                            dependencies: Vec::default(),
                            ownership_transfers: Vec::default(),
                            push_copies: vec![
                                RangedLocaticsSpecifier {
                                    range: RangeOver::NoRange,
                                    specifier: LocaticsSpecifier {
                                        target_object: ObjectArgumentRef {
                                            node_idx: TaskIndex::SelfIdx,
                                            domain: ObjectArgumentDomain::Arguments,
                                            arg_idx: 0,
                                        },
                                        target_host: HostTarget::EvalResult(
                                            PredefinedTargetFunction::HashTaskIdx,
                                        ),
                                    },
                                },
                                RangedLocaticsSpecifier {
                                    range: RangeOver::NoRange,
                                    specifier: LocaticsSpecifier {
                                        target_object: ObjectArgumentRef {
                                            node_idx: TaskIndex::SelfIdx,
                                            domain: ObjectArgumentDomain::Arguments,
                                            arg_idx: 6,
                                        },
                                        target_host: HostTarget::EvalResult(
                                            PredefinedTargetFunction::HashTaskIdx,
                                        ),
                                    },
                                },
                                RangedLocaticsSpecifier {
                                    range: RangeOver::NoRange,
                                    specifier: LocaticsSpecifier {
                                        target_object: ObjectArgumentRef {
                                            node_idx: TaskIndex::SelfIdx,
                                            domain: ObjectArgumentDomain::Arguments,
                                            arg_idx: 7,
                                        },
                                        target_host: HostTarget::EvalResult(
                                            PredefinedTargetFunction::HashTaskIdx,
                                        ),
                                    },
                                },
                                RangedLocaticsSpecifier {
                                    range: RangeOver::NoRange,
                                    specifier: LocaticsSpecifier {
                                        target_object: ObjectArgumentRef {
                                            node_idx: TaskIndex::SelfIdx,
                                            domain: ObjectArgumentDomain::Arguments,
                                            arg_idx: 8,
                                        },
                                        target_host: HostTarget::EvalResult(
                                            PredefinedTargetFunction::HashTaskIdx,
                                        ),
                                    },
                                },
                            ],
                            moves: Vec::default(),
                        },
                        post_actions: PostActionBlock::default(),
                    },
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Any,
                        intent_name: "smith_waterman::set_final_value".to_string(),
                        // schedule on state object owner
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::SelfIdx,
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 0,
                        }),
                        pre_actions: ActionBlock {
                            dependencies: Vec::default(),
                            ownership_transfers: vec![LocaticsSpecifier {
                                target_object: ObjectArgumentRef {
                                    node_idx: TaskIndex::SelfIdx,
                                    domain: ObjectArgumentDomain::Arguments,
                                    arg_idx: 1,
                                },
                                target_host: HostTarget::Owner(ObjectArgumentRef {
                                    node_idx: TaskIndex::SelfIdx,
                                    domain: ObjectArgumentDomain::Arguments,
                                    arg_idx: 0,
                                }),
                            }],
                            push_copies: Vec::default(),
                            moves: vec![LocaticsSpecifier {
                                target_object: ObjectArgumentRef {
                                    node_idx: TaskIndex::SelfIdx,
                                    domain: ObjectArgumentDomain::Arguments,
                                    arg_idx: 1,
                                },
                                target_host: HostTarget::Owner(ObjectArgumentRef {
                                    node_idx: TaskIndex::SelfIdx,
                                    domain: ObjectArgumentDomain::Arguments,
                                    arg_idx: 1,
                                }),
                            }],
                        },
                        post_actions: PostActionBlock::default(),
                    },
                ],
            },
        ),
        (
            "sw-512-non-step",
            ParsedPhysicalPlan {
                nodes: vec![
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Idx(1),
                        intent_name: "smith_waterman::init_smith_waterman".to_string(),
                        // ignored by manager
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::Idx(0),
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 0,
                        }),
                        pre_actions: ActionBlock::default(),
                        post_actions: PostActionBlock::default(),
                    },
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Any,
                        intent_name: "smith_waterman::process_block".to_string(),
                        // schedule on bucket object owner
                        schedule_on: ActivationTarget::EvalResult(
                            PredefinedTargetFunction::SplitAlongAntiDiagonal(512),
                        ),
                        pre_actions: ActionBlock {
                            dependencies: Vec::default(),
                            ownership_transfers: Vec::default(),
                            push_copies: vec![
                                RangedLocaticsSpecifier {
                                    range: RangeOver::NoRange,
                                    specifier: LocaticsSpecifier {
                                        target_object: ObjectArgumentRef {
                                            node_idx: TaskIndex::SelfIdx,
                                            domain: ObjectArgumentDomain::Arguments,
                                            arg_idx: 0,
                                        },
                                        target_host: HostTarget::EvalResult(
                                            PredefinedTargetFunction::SplitAlongAntiDiagonal(512),
                                        ),
                                    },
                                },
                                RangedLocaticsSpecifier {
                                    range: RangeOver::NoRange,
                                    specifier: LocaticsSpecifier {
                                        target_object: ObjectArgumentRef {
                                            node_idx: TaskIndex::SelfIdx,
                                            domain: ObjectArgumentDomain::Arguments,
                                            arg_idx: 6,
                                        },
                                        target_host: HostTarget::EvalResult(
                                            PredefinedTargetFunction::SplitAlongAntiDiagonal(512),
                                        ),
                                    },
                                },
                                RangedLocaticsSpecifier {
                                    range: RangeOver::NoRange,
                                    specifier: LocaticsSpecifier {
                                        target_object: ObjectArgumentRef {
                                            node_idx: TaskIndex::SelfIdx,
                                            domain: ObjectArgumentDomain::Arguments,
                                            arg_idx: 7,
                                        },
                                        target_host: HostTarget::EvalResult(
                                            PredefinedTargetFunction::SplitAlongAntiDiagonal(512),
                                        ),
                                    },
                                },
                                RangedLocaticsSpecifier {
                                    range: RangeOver::NoRange,
                                    specifier: LocaticsSpecifier {
                                        target_object: ObjectArgumentRef {
                                            node_idx: TaskIndex::SelfIdx,
                                            domain: ObjectArgumentDomain::Arguments,
                                            arg_idx: 8,
                                        },
                                        target_host: HostTarget::EvalResult(
                                            PredefinedTargetFunction::SplitAlongAntiDiagonal(512),
                                        ),
                                    },
                                },
                            ],
                            moves: Vec::default(),
                        },
                        post_actions: PostActionBlock::default(),
                    },
                    PhysicalPlanNode {
                        idx: PlanNodeIdx::Any,
                        intent_name: "smith_waterman::set_final_value".to_string(),
                        // schedule on state object owner
                        schedule_on: ActivationTarget::Owner(ObjectArgumentRef {
                            node_idx: TaskIndex::SelfIdx,
                            domain: ObjectArgumentDomain::Arguments,
                            arg_idx: 0,
                        }),
                        pre_actions: ActionBlock::default(),
                        post_actions: PostActionBlock::default(),
                    },
                ],
            },
        ),
    ]
}
