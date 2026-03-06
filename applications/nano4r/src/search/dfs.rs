use std::mem::size_of as mem_size_of;
use std::sync::Arc;

use nando_support::{
    allocate_and_init, bump_if_changed, iptr::IPtr, nando_spawn, nando_spawn_sink, nando_yield,
    nando_yield_sink,
};
use nandoize::{nandoize_lib, PersistableDeriveLib};
use object_lib::{
    allocators::{
        bump_allocator::BumpAllocator, persistently_allocatable::PersistentlyAllocatable,
    },
    collections::pvec::PVec,
    tls as object_lib_tls,
};
use object_lib::{MaterializedObjectVersion, Persistable};
use object_tracker::{object_tracker_tls, unit_ptr_of};
use parking_lot::RwLock;

use crate::definitions::*;

#[repr(C)]
#[derive(PersistableDeriveLib)]
pub struct DFSSearchState {
    graph_is_zero_indexed: bool,
    discovered: PVec<u8>,
    stack: PVec<VertexId>,
    n_vertices: usize,
}

impl DFSSearchState {
    const VERTICES_PER_ENTRY: f64 = 8.0;

    pub fn resize_discovered(&mut self, graph: &Graph, max_vertex_idx: Option<usize>) {
        let n_vertices_ptr = unit_ptr_of!(&self.n_vertices);
        object_lib_tls::add_new_pre_image(n_vertices_ptr, self.n_vertices.as_bytes());
        self.n_vertices = graph.n_verts;
        object_lib_tls::add_new_post_image_if_changed(n_vertices_ptr, self.n_vertices.as_bytes());

        let upper_limit = match max_vertex_idx {
            None => graph.n_verts,
            Some(max) => max,
        } as f64;
        let new_capacity = (upper_limit as f64 / Self::VERTICES_PER_ENTRY).ceil() as usize;

        self.discovered.resize_to_capacity(new_capacity);
        for _ in 0..new_capacity {
            // init
            self.discovered.push(0);
        }
    }

    pub fn is_discovered(&self, vertex: VertexId) -> bool {
        let vertex = vertex as usize;
        let vertices_per_entry = Self::VERTICES_PER_ENTRY as usize;

        let entry = self.discovered[vertex / vertices_per_entry];
        entry >> (vertex % vertices_per_entry) & 1 == 1
    }

    pub fn mark_discovered(&mut self, vertex: VertexId) {
        let vertex = vertex as usize;
        let vertices_per_entry = Self::VERTICES_PER_ENTRY as usize;

        let mut entry = self.discovered[vertex / vertices_per_entry];

        let discovered_ptr = unit_ptr_of!(&self.discovered[vertex / vertices_per_entry]);
        object_lib_tls::add_new_pre_image(discovered_ptr, entry.as_bytes());

        entry |= 1 << (vertex % vertices_per_entry);
        self.discovered[vertex / vertices_per_entry] = entry;

        object_lib_tls::add_new_post_image_if_changed(discovered_ptr, entry.as_bytes());
    }

    pub fn get_unvisited_vertex(&self) -> Option<VertexId> {
        let num_vertices_per_entry = Self::VERTICES_PER_ENTRY as usize;
        let max_vertex = match self.graph_is_zero_indexed {
            true => self.n_vertices,
            false => self.n_vertices + 1,
        };

        for (idx, e) in self.discovered.into_iter().enumerate() {
            for packed_idx in 0..num_vertices_per_entry {
                if !self.graph_is_zero_indexed && idx == 0 && packed_idx == 0 {
                    continue;
                }

                if idx * num_vertices_per_entry + packed_idx == max_vertex {
                    return None;
                }

                if (e >> packed_idx) & 1 == 1 {
                    continue;
                }

                return Some(idx * num_vertices_per_entry + packed_idx);
            }
        }

        None
    }
}

impl PersistentlyAllocatable for DFSSearchState {
    fn set_allocator(&mut self, allocator: Arc<RwLock<BumpAllocator>>) {
        self.discovered.set_allocator(Arc::clone(&allocator));
        self.stack.set_allocator(allocator);
    }

    fn get_allocator(&self) -> Option<Arc<RwLock<BumpAllocator>>> {
        self.discovered.get_allocator()
    }
}

#[nandoize_lib]
pub fn dfs(graph: &Graph, is_zero_indexed: bool) -> IPtr {
    let object_tracker = object_tracker_tls::get_local_object_tracker_instance();
    let search_state = allocate_and_init!(object_tracker, DFSSearchState);
    let search_state_data = search_state.read_into_mut::<DFSSearchState>(None).unwrap();
    search_state_data.stack.resize_to_capacity(16);
    search_state_data.resize_discovered(graph, None);
    search_state_data.graph_is_zero_indexed = is_zero_indexed;
    let _ = search_state.bump_version();
    object_tracker.push_initial_version(search_state.id, (&*search_state).into());

    let graph_iptr = object_lib_tls::iptr_of(unit_ptr_of!(graph)).unwrap();
    let search_state_iptr = search_state.iptr_of();
    nando_spawn!("nano4r::dfs_root", graph_iptr, search_state_iptr);

    search_state_iptr
}

#[nandoize_lib]
pub fn dfs_bulk(graph: &Graph, is_zero_indexed: bool, max_idx: usize) -> IPtr {
    let object_tracker = object_tracker_tls::get_local_object_tracker_instance();
    let search_state = allocate_and_init!(object_tracker, DFSSearchState);
    let search_state_data = search_state.read_into_mut::<DFSSearchState>(None).unwrap();
    search_state_data
        .stack
        .resize_to_capacity(graph.n_verts / 4);
    search_state_data.resize_discovered(graph, Some(max_idx));
    search_state_data.graph_is_zero_indexed = is_zero_indexed;
    let _ = search_state.bump_version();
    object_tracker.push_initial_version(search_state.id, (&*search_state).into());

    let graph_iptr = object_lib_tls::iptr_of(unit_ptr_of!(graph)).unwrap();
    let search_state_iptr = search_state.iptr_of();
    nando_spawn!("nano4r::dfs_bulk_root", graph_iptr, search_state_iptr);

    search_state_iptr
}

#[nandoize_lib]
pub fn dfs_root(graph: &Graph, search_state: &DFSSearchState) {
    let root_vertex = search_state.get_unvisited_vertex();
    if let None = root_vertex {
        // Done exploring the whole graph.
        return;
    }

    let root_vertex = root_vertex.unwrap();
    let graph_iptr = object_lib_tls::iptr_of(unit_ptr_of!(graph)).unwrap();
    let search_state_iptr = object_lib_tls::iptr_of(unit_ptr_of!(search_state)).unwrap();

    // Get the new root vertex's partition. Since we're using probabilistic filters, we might spawn
    // more subcomputations than we need (exactly 1). The goal of `tas_partition_local_dfs` is to check for
    // membership on the _actual_ object.
    for idx in 0..graph.partitions.len() {
        if !graph.filters[idx].contains(root_vertex) {
            continue;
        }

        // Spawn partition-local search.
        let partition_ptr = graph.partitions[idx].get_inner();
        nando_spawn!(
            "nano4r::tas_partition_local_dfs",
            graph_iptr,
            partition_ptr,
            root_vertex,
            search_state_iptr
        );

        break;
    }

    // After search from the current root is done, call this function to pick a new root.
    nando_spawn_sink!("nano4r::dfs_root", graph_iptr, search_state_iptr);
}

#[nandoize_lib]
pub fn dfs_bulk_root(graph: &Graph, search_state: &DFSSearchState) {
    let root_vertex = search_state.get_unvisited_vertex();
    if let None = root_vertex {
        // Done exploring the whole graph.
        return;
    }

    let root_vertex = root_vertex.unwrap();
    let graph_iptr = object_lib_tls::iptr_of(unit_ptr_of!(graph)).unwrap();
    let search_state_iptr = object_lib_tls::iptr_of(unit_ptr_of!(search_state)).unwrap();

    let partition_iptrs: Vec<IPtr> = graph.partitions.iter().map(|p| p.get_inner()).collect();
    nando_spawn!(
        "nano4r::dfs_bulk_inner",
        graph_iptr,
        partition_iptrs,
        root_vertex,
        search_state_iptr
    );

    // After search from the current root is done, call this function to pick a new root.
    // nando_yield_sink!("nano4r::dfs_root", &dfs_task, graph_iptr, search_state_iptr);
}

#[nandoize_lib]
pub fn tas_partition_local_dfs(
    graph: &Graph,
    partition: &GraphPartition,
    partition_root_vertex: VertexId,
    state: &mut DFSSearchState,
) {
    if !partition.adjacencies.contains(&partition_root_vertex) {
        return;
    }

    // NOTE since we already are in the context of a nanotransaction that has ownership of its
    // arguments, we can just directly invoke the target function instead of going through
    // nando_{spawn,yield}
    partition_local_dfs(graph, partition, partition_root_vertex, state);
}

#[nandoize_lib]
pub fn partition_local_dfs(
    graph: &Graph,
    partition: &GraphPartition,
    partition_root_vertex: VertexId,
    state: &mut DFSSearchState,
) {
    state.stack.push(partition_root_vertex);

    while !state.stack.is_empty() {
        let v = state
            .stack
            .pop()
            .expect("failed to get vertex from non-empty stack");
        if state.is_discovered(v) {
            continue;
        }

        // Cannot continue exploring further, need to spawn
        // on another object
        if !partition.adjacencies.contains(&v) {
            let graph_iptr = object_lib_tls::iptr_of(unit_ptr_of!(graph)).unwrap();
            let search_state_iptr = object_lib_tls::iptr_of(unit_ptr_of!(state)).unwrap();

            for idx in 0..graph.partitions.len() {
                if !graph.filters[idx].contains(v) {
                    continue;
                }

                let partition_ptr = graph.partitions[idx].get_inner();
                nando_spawn!(
                    "nano4r::tas_partition_local_dfs",
                    graph_iptr,
                    partition_ptr,
                    v,
                    search_state_iptr
                );
            }
        }

        state.mark_discovered(v);
        for w in partition.adjacencies.get(&v).unwrap() {
            state.stack.push(*w);
        }
    }
}

#[nandoize_lib]
pub fn dfs_bulk_inner(
    graph: &Graph,
    partitions: Vec<&GraphPartition>,
    root_vertex: VertexId,
    state: &mut DFSSearchState,
) {
    state.stack.push(root_vertex);
    state.mark_discovered(root_vertex);

    while !state.stack.is_empty() {
        let v = state
            .stack
            .pop()
            .expect("failed to get vertex from non-empty stack");

        for partition in &partitions {
            if !partition.adjacencies.contains(&v) {
                continue;
            }
            for w in partition.adjacencies.get(&v).unwrap() {
                if state.is_discovered(*w) {
                    continue;
                }

                state.mark_discovered(*w);
                state.stack.push(*w);
            }

            break;
        }
    }
}

#[nandoize_lib]
pub fn print_discovered_dfs(search_state: &DFSSearchState) {
    for discovered_map in &search_state.discovered {
        println!("discovered: {:b}", discovered_map);
    }
}
