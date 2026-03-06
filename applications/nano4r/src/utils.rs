use std::io::{BufRead, BufReader};
use std::mem::size_of as mem_size_of;
use std::path::Path;
use std::sync::Arc;
use std::{fs, num};

use nando_support::{
    allocate_and_init,
    iptr::{IPtr, TypedIPtr},
    iptr_of_ref, nando_spawn, nando_yield, nando_yield_sink,
};
use nandoize::{nandoize_lib, PersistableDeriveLib};
use object_lib::{
    allocators::{
        bump_allocator::BumpAllocator, persistently_allocatable::PersistentlyAllocatable,
    },
    collections::{
        pcuckoo::{self, PCuckooFilter},
        pmap::PHashMap,
        pvec::PVec,
    },
    tls as object_lib_tls, Object,
};
use object_lib::{MaterializedObjectVersion, ObjectId, Persistable};
use object_tracker::{object_tracker_tls, unit_ptr_of, ObjectTracker};
use ownership_tracker::ownership_tracker_tls;
use parking_lot::RwLock;
use rand::{rngs::SmallRng, seq::SliceRandom, Rng, SeedableRng};

use crate::definitions::*;

/* Graph Utils */

#[nandoize_lib]
pub fn parse_graph(
    /// Absolute path to graph file.
    graph_path_str: String,
    /// Initial graph size. Setting it too low might lead to slower parsing due to the need for
    /// multiple resizes of the partition vector in the graph root.
    num_initial_partitions: usize,
    undirected: bool,
) -> Option<ObjectId> {
    // NOTE we expect to be passed an absolute path to the target file. Alternatively, if the path
    // is relative, it is expected to be relative to the magpie working directory, *not* the
    // library's working directory.
    let path = Path::new(&graph_path_str);
    let mut file_options = fs::OpenOptions::new();
    file_options
        .read(true)
        .write(false)
        .create(false)
        .truncate(false);

    match file_options.open(path) {
        Ok(f) => {
            let object_tracker = object_tracker_tls::get_local_object_tracker_instance();

            // manifest object
            let graph_object = allocate_and_init!(object_tracker, Graph);
            let graph_object_data = graph_object.read_into_mut::<Graph>(None).unwrap();
            graph_object_data
                .filters
                .resize_to_capacity(num_initial_partitions);
            graph_object_data
                .partitions
                .resize_to_capacity(num_initial_partitions);

            graph_object_data.is_directed = !undirected;
            graph_object_data.min_vertex_id = VertexId::MAX;
            graph_object_data.max_vertex_id = VertexId::default();

            if graph_object_data.is_directed {
                graph_object_data
                    .foreign_vertex_degrees
                    .resize_to_capacity(0);
                graph_object_data
                    .incoming_edges
                    .resize_to_capacity(num_initial_partitions);
            } else {
                graph_object_data
                    .foreign_vertex_degrees
                    .resize_to_capacity(num_initial_partitions);
                graph_object_data.incoming_edges.resize_to_capacity(0);
            }

            let mut partition_refs = Vec::with_capacity(num_initial_partitions);
            let mut fvd_refs = Vec::with_capacity(num_initial_partitions);
            let mut incoming_edges_refs = match graph_object_data.is_directed {
                true => Vec::with_capacity(num_initial_partitions),
                false => Vec::default(),
            };

            let kvs_iptr: IPtr = kvs::init_kvs::<ObjectId>(1, num_initial_partitions as u64);
            for idx in 0..num_initial_partitions {
                partition_refs.push(allocate_partition(
                    Arc::clone(&object_tracker),
                    graph_object_data,
                    false,
                ));
                if !graph_object_data.is_directed {
                    fvd_refs.push(allocate_fvd_object(
                        Arc::clone(&object_tracker),
                        graph_object_data,
                        kvs_iptr.clone(),
                    ));
                }

                if graph_object_data.is_directed {
                    incoming_edges_refs.push(allocate_incoming_edge_object(
                        Arc::clone(&object_tracker),
                        graph_object_data,
                    ));
                }
            }

            let line_reader = BufReader::new(f);

            for line in line_reader.lines() {
                let Ok(line) = line else {
                    break;
                };

                if line.starts_with('#') {
                    continue;
                }

                let vertices = line.split_once(|c| c == ' ' || c == '\t').unwrap();
                let source: VertexId = match vertices.0.parse() {
                    Err(_) => continue,
                    Ok(s) => s,
                };
                let dest: VertexId = vertices.1.parse().unwrap();

                let source_idx = insert_edge(
                    graph_object_data,
                    source,
                    dest,
                    None,
                    &mut partition_refs,
                    &mut fvd_refs,
                    &mut incoming_edges_refs,
                    kvs_iptr.clone(),
                    false,
                );
                if undirected {
                    // FIXME insert_edge should also try to update the incoming edges list of the
                    // target vertex if undirected
                    let _ = insert_edge(
                        graph_object_data,
                        dest,
                        source,
                        Some(source_idx),
                        &mut partition_refs,
                        &mut fvd_refs,
                        &mut incoming_edges_refs,
                        kvs_iptr.clone(),
                        false,
                    );
                } else {
                    insert_vertex(
                        graph_object_data,
                        dest,
                        source,
                        Some(source_idx),
                        &mut partition_refs,
                        &mut fvd_refs,
                        &mut incoming_edges_refs,
                        kvs_iptr.clone(),
                        false,
                    );
                }

                graph_object_data.n_edges += 1;
                graph_object_data.min_vertex_id =
                    std::cmp::min(graph_object_data.min_vertex_id, std::cmp::min(source, dest));
                graph_object_data.max_vertex_id =
                    std::cmp::max(graph_object_data.max_vertex_id, std::cmp::max(source, dest));
            }

            for partition in partition_refs {
                let _ = partition.bump_version();
                partition.flush();
                object_tracker.push_initial_version(partition.id, (&*partition).into());
                ownership_tracker_tls::mark_object_owned(partition.id);
            }

            for fvd_object in fvd_refs {
                let _ = fvd_object.bump_version();
                fvd_object.flush();
                object_tracker.push_initial_version(fvd_object.id, (&*fvd_object).into());
                ownership_tracker_tls::mark_object_owned(fvd_object.id);
            }

            for incoming_edges_object in incoming_edges_refs {
                let _ = incoming_edges_object.bump_version();
                incoming_edges_object.flush();
                object_tracker.push_initial_version(
                    incoming_edges_object.id,
                    (&*incoming_edges_object).into(),
                );
                ownership_tracker_tls::mark_object_owned(incoming_edges_object.id);
            }

            let graph_object_iptr = graph_object.iptr_of();
            nando_spawn!("nano4r::graph_compute_foreign_degrees", graph_object_iptr);

            let graph_object_id = graph_object.id;
            let _ = graph_object.bump_version();
            graph_object.flush();
            object_tracker.push_initial_version(graph_object_id, (&*graph_object).into());
            ownership_tracker_tls::mark_object_owned(graph_object_id);

            println!("Partitions: {:#?}", graph_object_data.partitions);
            println!("Incoming Edges: {:#?}", graph_object_data.incoming_edges);
            println!(
                "foreign vertex degrees: {:#?}",
                graph_object_data.foreign_vertex_degrees
            );

            Some(graph_object_id)
        }
        Err(e) => {
            eprintln!(
                "could not open graph file {:?} from wd {:?}: {}",
                path,
                std::env::current_dir(),
                e
            );
            None
        }
    }
}

#[nandoize_lib]
pub fn parse_huge_graph(
    /// Absolute path to graph file.
    graph_path_str: String,
    /// Initial graph size. Setting it too low might lead to slower parsing due to the need for
    /// multiple resizes of the partition vector in the graph root.
    num_initial_partitions: usize,
    undirected: bool,
) -> Option<ObjectId> {
    let path = Path::new(&graph_path_str);
    let mut file_options = fs::OpenOptions::new();
    file_options
        .read(true)
        .write(false)
        .create(false)
        .truncate(false);

    match file_options.open(path) {
        Ok(f) => {
            let object_tracker = object_tracker_tls::get_local_object_tracker_instance();

            // manifest object
            let graph_object = allocate_and_init!(object_tracker, Graph);
            let graph_object_data = graph_object.read_into_mut::<Graph>(None).unwrap();
            graph_object_data
                .filters
                .resize_to_capacity(num_initial_partitions);
            graph_object_data
                .partitions
                .resize_to_capacity(num_initial_partitions);

            graph_object_data.is_directed = !undirected;
            graph_object_data.min_vertex_id = VertexId::MAX;
            graph_object_data.max_vertex_id = VertexId::default();

            if graph_object_data.is_directed {
                graph_object_data
                    .foreign_vertex_degrees
                    .resize_to_capacity(0);
                graph_object_data
                    .incoming_edges
                    .resize_to_capacity(num_initial_partitions);
            } else {
                graph_object_data
                    .foreign_vertex_degrees
                    .resize_to_capacity(num_initial_partitions);
                graph_object_data.incoming_edges.resize_to_capacity(0);
            }

            let mut partition_refs = Vec::with_capacity(num_initial_partitions);
            let mut fvd_refs = match graph_object_data.is_directed {
                false => Vec::with_capacity(num_initial_partitions),
                true => Vec::default(),
            };
            let mut incoming_edges_refs = match graph_object_data.is_directed {
                true => Vec::with_capacity(num_initial_partitions),
                false => Vec::default(),
            };

            let kvs_iptr: IPtr = kvs::init_kvs::<ObjectId>(1, num_initial_partitions as u64);
            for idx in 0..num_initial_partitions {
                partition_refs.push(allocate_partition(
                    Arc::clone(&object_tracker),
                    graph_object_data,
                    true,
                ));
                if !graph_object_data.is_directed {
                    fvd_refs.push(allocate_fvd_object(
                        Arc::clone(&object_tracker),
                        graph_object_data,
                        kvs_iptr.clone(),
                    ));
                }

                if graph_object_data.is_directed {
                    incoming_edges_refs.push(allocate_incoming_edge_object(
                        Arc::clone(&object_tracker),
                        graph_object_data,
                    ));
                }
            }

            let line_reader = BufReader::new(f);

            for line in line_reader.lines() {
                let Ok(line) = line else {
                    break;
                };

                if line.starts_with('#') {
                    continue;
                }

                let vertices = line.split_once(|c| c == ' ' || c == '\t').unwrap();
                let source: VertexId = vertices.0.parse().unwrap();
                let source: VertexId = match vertices.0.parse() {
                    Err(_) => continue,
                    Ok(s) => s,
                };
                let dest: VertexId = vertices.1.parse().unwrap();

                let source_idx = insert_edge(
                    graph_object_data,
                    source,
                    dest,
                    None,
                    &mut partition_refs,
                    &mut fvd_refs,
                    &mut incoming_edges_refs,
                    kvs_iptr.clone(),
                    true,
                );
                if undirected {
                    let _ = insert_edge(
                        graph_object_data,
                        dest,
                        source,
                        Some(source_idx),
                        &mut partition_refs,
                        &mut fvd_refs,
                        &mut incoming_edges_refs,
                        kvs_iptr.clone(),
                        true,
                    );
                } else {
                    insert_vertex(
                        graph_object_data,
                        dest,
                        source,
                        Some(source_idx),
                        &mut partition_refs,
                        &mut fvd_refs,
                        &mut incoming_edges_refs,
                        kvs_iptr.clone(),
                        true,
                    );
                }

                graph_object_data.n_edges += 1;
                graph_object_data.min_vertex_id =
                    std::cmp::min(graph_object_data.min_vertex_id, std::cmp::min(source, dest));
                graph_object_data.max_vertex_id =
                    std::cmp::max(graph_object_data.max_vertex_id, std::cmp::max(source, dest));
            }

            for partition in partition_refs {
                let _ = partition.bump_version();
                partition.flush();
                object_tracker.push_initial_version(partition.id, (&*partition).into());
                ownership_tracker_tls::mark_object_owned(partition.id);
            }

            for fvd_object in fvd_refs {
                let _ = fvd_object.bump_version();
                fvd_object.flush();
                object_tracker.push_initial_version(fvd_object.id, (&*fvd_object).into());
                ownership_tracker_tls::mark_object_owned(fvd_object.id);
            }

            for incoming_edges_object in incoming_edges_refs {
                let _ = incoming_edges_object.bump_version();
                incoming_edges_object.flush();
                object_tracker.push_initial_version(
                    incoming_edges_object.id,
                    (&*incoming_edges_object).into(),
                );
                ownership_tracker_tls::mark_object_owned(incoming_edges_object.id);
            }

            if !graph_object_data.is_directed {
                let graph_object_iptr = graph_object.iptr_of();
                nando_spawn!("nano4r::graph_compute_foreign_degrees", graph_object_iptr);
            }

            let graph_object_id = graph_object.id;
            let _ = graph_object.bump_version();
            graph_object.flush();
            object_tracker.push_initial_version(graph_object_id, (&*graph_object).into());
            ownership_tracker_tls::mark_object_owned(graph_object_id);

            println!("Partitions: {:#?}", graph_object_data.partitions);
            println!("Incoming Edges: {:#?}", graph_object_data.incoming_edges);
            println!(
                "foreign vertex degrees: {:#?}",
                graph_object_data.foreign_vertex_degrees
            );

            Some(graph_object_id)
        }
        Err(e) => {
            eprintln!(
                "could not open graph file {:?} from wd {:?}: {}",
                path,
                std::env::current_dir(),
                e
            );
            None
        }
    }
}

#[nandoize_lib]
pub fn push_remote_partition_incoming_edges(
    partitioned_graph: &Graph,
    partition: &GraphPartition,
    own_incoming_edges: &mut PartitionIncomingEdges,
) {
    let object_tracker = object_tracker_tls::get_local_object_tracker_instance();
    let mut per_vertex_possible_partitions =
        std::collections::HashMap::with_capacity(partition.adjacencies.len());
    let mut per_partition_incoming_edges: std::collections::HashMap<
        ObjectId,
        std::collections::HashMap<VertexId, Vec<VertexId>>,
    > = std::collections::HashMap::with_capacity(partitioned_graph.partitions.len());
    let own_partition_iptr = iptr_of_ref!(partition);

    for (vertex, neighbors) in &partition.adjacencies {
        for neighbor in neighbors {
            if partition.adjacencies.contains(neighbor) {
                match own_incoming_edges.per_vertex_incoming.get_mut(&neighbor) {
                    None => {
                        own_incoming_edges
                            .per_vertex_incoming
                            .insert(*neighbor, PVec::new());
                        let allocator = own_incoming_edges
                            .per_vertex_incoming
                            .get_allocator()
                            .unwrap();
                        let adj = own_incoming_edges
                            .per_vertex_incoming
                            .get_mut(neighbor)
                            .unwrap();
                        adj.set_allocator(Arc::clone(&allocator));
                        adj.resize_to_capacity(8);

                        adj.push(*vertex);
                    }
                    Some(incoming_adj) => {
                        incoming_adj.push(*vertex);
                    }
                }
                continue;
            }

            let partition_ids = match per_vertex_possible_partitions.get(neighbor) {
                Some(ref p) => p,
                None => {
                    let partitions: Vec<ObjectId> = partitioned_graph
                        .partitions
                        .iter()
                        .enumerate()
                        .filter(|(idx, partition_iptr)| {
                            !(partition_iptr.get_inner().get_object_id()
                                == own_partition_iptr.get_object_id()
                                || !partitioned_graph.filters[*idx].contains(*neighbor))
                        })
                        .map(|(_, partition_iptr)| partition_iptr.get_inner().get_object_id())
                        .collect();

                    per_vertex_possible_partitions.insert(*neighbor, partitions);

                    per_vertex_possible_partitions.get(neighbor).unwrap()
                }
            };

            for partition in partition_ids {
                per_partition_incoming_edges
                    .entry(*partition)
                    .and_modify(|e| {
                        e.entry(*neighbor)
                            .and_modify(|e| e.push(*vertex))
                            .or_insert(vec![*vertex]);
                    })
                    .or_insert({
                        let mut map = std::collections::HashMap::new();
                        map.insert(*neighbor, vec![*vertex]);
                        map
                    });
            }
        }
    }

    for (partition_id, incoming_edges) in per_partition_incoming_edges {
        let partition_incoming_edges = allocate_and_init!(object_tracker, IncomingEdgesChunk);
        let partition_incoming_edges_data = partition_incoming_edges
            .read_into_mut::<IncomingEdgesChunk>(None)
            .unwrap();
        partition_incoming_edges_data
            .incoming_edges
            .with_capacity(incoming_edges.len());

        // FIXME don't do this element-wise
        let allocator = partition_incoming_edges_data.get_allocator().unwrap();
        for (target_vertex, vertex_incoming_edges) in incoming_edges {
            partition_incoming_edges_data
                .incoming_edges
                .insert(target_vertex, PVec::new());
            let mut incoming_edges = partition_incoming_edges_data
                .incoming_edges
                .get_mut(&target_vertex)
                .unwrap();
            incoming_edges.set_allocator(Arc::clone(&allocator));
            incoming_edges.resize_to_capacity(vertex_incoming_edges.len());

            for incoming_edge in vertex_incoming_edges {
                incoming_edges.push(incoming_edge);
            }
        }

        let partition_incoming_edges_chunk_iptr = partition_incoming_edges.iptr_of();
        let mut partition_tasks = vec![];
        for (idx, partition) in partitioned_graph.partitions.iter().enumerate() {
            let partition_iptr = partition.get_inner();
            let partition_object_id = partition_iptr.get_object_id();
            if partition_object_id != partition_id {
                continue;
            }

            let incoming_edge_iptr = partitioned_graph.incoming_edges[idx].get_inner();

            partition_tasks.push(nando_spawn!(
                "nano4r::update_partition_incoming_edges",
                partition_iptr,
                incoming_edge_iptr,
                partition_incoming_edges_chunk_iptr
            ));
        }

        nando_yield_sink!(
            "nano4r::delete_incoming_edges_chunk",
            &partition_tasks,
            partition_incoming_edges_chunk_iptr
        );

        let _ = partition_incoming_edges.bump_version();
        partition_incoming_edges.flush();
        object_tracker.push_initial_version(
            partition_incoming_edges.id,
            (&*partition_incoming_edges).into(),
        );
    }
}

#[nandoize_lib]
pub fn update_partition_incoming_edges(
    partition: &GraphPartition,
    partition_incoming_edges: &mut PartitionIncomingEdges,
    incoming_edges_chunk: &IncomingEdgesChunk,
) {
    let allocator = partition_incoming_edges.get_allocator().unwrap();
    for (vertex, incoming_edges_chunk) in &incoming_edges_chunk.incoming_edges {
        if !partition.adjacencies.contains(vertex) {
            continue;
        }

        let incoming_edges = match partition_incoming_edges.per_vertex_incoming.get_mut(vertex) {
            None => {
                partition_incoming_edges
                    .per_vertex_incoming
                    .insert(*vertex, PVec::new());
                let mut incoming_edges = partition_incoming_edges
                    .per_vertex_incoming
                    .get_mut(vertex)
                    .unwrap();
                incoming_edges.set_allocator(Arc::clone(&allocator));
                incoming_edges.resize_to_capacity(incoming_edges_chunk.len());

                incoming_edges
            }
            Some(e) => e,
        };

        for incoming_edge in incoming_edges_chunk {
            incoming_edges.push(*incoming_edge);
        }
    }
}

#[repr(C)]
#[derive(PersistableDeriveLib)]
pub struct RepartitionChunks {
    to_move: PVec<VertexId>,
}

impl PersistentlyAllocatable for RepartitionChunks {
    fn set_allocator(&mut self, allocator: Arc<RwLock<BumpAllocator>>) {
        self.to_move.set_allocator(Arc::clone(&allocator));
    }

    fn get_allocator(&self) -> Option<Arc<RwLock<BumpAllocator>>> {
        self.to_move.get_allocator()
    }
}

#[repr(C)]
#[derive(PersistableDeriveLib)]
pub struct IncomingEdgesChunk {
    incoming_edges: PHashMap<VertexId, PVec<VertexId>>,
}

impl PersistentlyAllocatable for IncomingEdgesChunk {
    fn set_allocator(&mut self, allocator: Arc<RwLock<BumpAllocator>>) {
        self.incoming_edges.set_allocator(Arc::clone(&allocator));
    }

    fn get_allocator(&self) -> Option<Arc<RwLock<BumpAllocator>>> {
        self.incoming_edges.get_allocator()
    }
}

#[nandoize_lib]
pub fn generate_repartition_chunks(partition: &GraphPartition, verts_per_partition: usize) -> IPtr {
    let object_tracker = object_tracker_tls::get_local_object_tracker_instance();
    let repartition_chunks = allocate_and_init!(object_tracker, RepartitionChunks);
    let repartition_chunk_data = repartition_chunks
        .read_into_mut::<RepartitionChunks>(None)
        .unwrap();

    repartition_chunk_data
        .to_move
        .resize_to_capacity(partition.adjacencies.len());

    for (vertex, _) in &partition.adjacencies {
        repartition_chunk_data.to_move.push(*vertex);
    }

    let _ = repartition_chunks.bump_version();
    repartition_chunks.flush();
    object_tracker.push_initial_version(repartition_chunks.id, (&*repartition_chunks).into());

    repartition_chunks.iptr_of()
}

#[nandoize_lib]
pub fn spread_repartitioned_chunks(
    partition: &GraphPartition,
    original_graph: &Graph,
    partitioned_graph: &Graph,
    chunks: &mut RepartitionChunks,
    verts_per_partition: usize,
) {
    let first_partition = partitioned_graph.partitions[0].get_inner();
    let partition_iptr = iptr_of_ref!(partition);
    let partitioned_graph_iptr = iptr_of_ref!(partitioned_graph);
    let original_graph_iptr = iptr_of_ref!(original_graph);
    let chunks_iptr = iptr_of_ref!(chunks);
    nando_spawn!(
        "nano4r::try_partition_or_next",
        first_partition,
        0,
        partition_iptr,
        partitioned_graph_iptr,
        original_graph_iptr,
        chunks_iptr,
        verts_per_partition
    );
}

#[nandoize_lib]
pub fn try_partition_or_next(
    target_partition: &mut GraphPartition,
    target_partition_idx: usize,
    source_partition: &GraphPartition,
    partitioned_graph: &mut Graph,
    original_graph: &Graph,
    chunks: &mut RepartitionChunks,
    verts_per_partition: usize,
) {
    let allocator = target_partition.get_allocator().unwrap();
    while target_partition.adjacencies.len() < verts_per_partition {
        let vertex_to_move = match chunks.to_move.pop() {
            None => break,
            Some(v) => v,
        };

        target_partition
            .adjacencies
            .insert(vertex_to_move, PVec::new());
        partitioned_graph.n_verts += 1;
        let mut adjacencies = &mut target_partition.adjacencies;

        let source_adj_list = adjacencies
            .get_mut(&vertex_to_move)
            .expect(&format!("source vertex {} not found", vertex_to_move));
        let neighbor_list = source_partition.adjacencies.get(&vertex_to_move).unwrap();

        source_adj_list.set_allocator(Arc::clone(&allocator));
        source_adj_list.resize_to_capacity(neighbor_list.len());

        for neighbor in neighbor_list {
            source_adj_list.push(*neighbor);
        }
        partitioned_graph.n_edges += neighbor_list.len();

        assert!(
            partitioned_graph.filters[target_partition_idx].add(vertex_to_move)
                == pcuckoo::Status::Ok
        );
        assert!(partitioned_graph.filters[target_partition_idx].contains(vertex_to_move));
    }

    if chunks.to_move.is_empty() {
        let repartition_chunk_iptr = iptr_of_ref!(chunks);
        nando_spawn!("nano4r::delete_repartition_chunk", repartition_chunk_iptr);
        return;
    }

    if target_partition_idx + 1 == partitioned_graph.partitions.len() {
        let object_tracker = object_tracker_tls::get_local_object_tracker_instance();
        // NOTE this should never get exercised currently
        let p = allocate_partition(Arc::clone(&object_tracker), partitioned_graph, true);

        let _ = p.bump_version();
        object_tracker.push_initial_version(p.id, (&*p).into());
    }

    let next_partition_idx = target_partition_idx + 1;
    let next_partition_iptr = partitioned_graph.partitions[next_partition_idx].get_inner();
    let source_partition_iptr = iptr_of_ref!(source_partition);
    let partitioned_graph_iptr = iptr_of_ref!(partitioned_graph);
    let original_graph_iptr = iptr_of_ref!(original_graph);
    let chunks_iptr = iptr_of_ref!(chunks);
    nando_spawn!(
        "nano4r::try_partition_or_next",
        next_partition_iptr,
        next_partition_idx,
        source_partition_iptr,
        partitioned_graph_iptr,
        original_graph_iptr,
        chunks_iptr,
        verts_per_partition
    );
}

#[nandoize_lib]
pub fn print_graph(graph: &Graph) {
    for partition in &graph.partitions {
        nando_spawn!("nano4r::print_adjacency_lists", partition);
    }
}

#[nandoize_lib]
pub fn print_adjacency_lists(partition: &GraphPartition) {
    println!("number of entries: {}", partition.adjacencies.len());
    for (source, dests) in &partition.adjacencies {
        print!("Sinks for source '{}': {}\n", source, dests.len());
    }
}

#[nandoize_lib]
pub fn print_per_partition_stats(graph: &Graph) {
    println!(
        "Total num vertices: {}, num edges: {}",
        graph.n_verts, graph.n_edges
    );
    for partition in &graph.partitions {
        nando_spawn!("nano4r::print_partition_stats", partition);
    }
}

#[nandoize_lib]
pub fn print_partition_stats(partition: &GraphPartition) {
    println!(
        "Number of vertices in partition: {}",
        partition.adjacencies.len()
    );
}

fn allocate_incoming_edge_object(
    object_tracker: Arc<ObjectTracker>,
    graph_object_data: &mut Graph,
) -> Arc<Object> {
    let incoming_edges = allocate_and_init!(object_tracker, PartitionIncomingEdges);
    let incoming_edges_data = incoming_edges
        .read_into_mut::<PartitionIncomingEdges>(None)
        .unwrap();
    incoming_edges_data
        .per_vertex_incoming
        .with_capacity(1024usize.pow(2) * 4);

    graph_object_data
        .incoming_edges
        .push(TypedIPtr::<PartitionIncomingEdges>::from(
            incoming_edges.iptr_of(),
        ));

    Arc::clone(&incoming_edges)
}

fn allocate_partition(
    object_tracker: Arc<ObjectTracker>,
    graph_object_data: &mut Graph,
    is_huge: bool,
) -> Arc<Object> {
    let partition = allocate_and_init!(object_tracker, GraphPartition);
    let partition_data = partition.read_into_mut::<GraphPartition>(None).unwrap();

    partition_data
        .adjacencies
        .with_capacity(1024usize.pow(2) * 4);

    let max_num_keys = match graph_object_data.is_directed || is_huge {
        true => 1 << 18,
        false => 1 << 21,
    };

    graph_object_data
        .filters
        .push(PCuckooFilter::new(max_num_keys, 10));

    {
        let allocator = graph_object_data.get_allocator().unwrap();
        let filter = graph_object_data.filters.last().unwrap();
        filter.set_allocator(allocator);
        filter.allocate_table();
    }

    graph_object_data
        .partitions
        .push(TypedIPtr::<GraphPartition>::from(partition.iptr_of()));

    Arc::clone(&partition)
}

fn allocate_fvd_object(
    object_tracker: Arc<ObjectTracker>,
    graph_object_data: &mut Graph,
    kvs_iptr: IPtr,
) -> Arc<Object> {
    let foreign_vertex_degrees = allocate_and_init!(object_tracker, ForeignVertexDegrees);
    let foreign_vertex_degrees_data = foreign_vertex_degrees
        .read_into_mut::<ForeignVertexDegrees>(None)
        .unwrap();

    foreign_vertex_degrees_data.degree.with_capacity(1024);
    foreign_vertex_degrees_data
        .per_partition
        .with_capacity(1024);
    foreign_vertex_degrees_data.vd_table = kvs_iptr.into();

    graph_object_data
        .foreign_vertex_degrees
        .push(TypedIPtr::<ForeignVertexDegrees>::from(
            foreign_vertex_degrees.iptr_of(),
        ));

    Arc::clone(&foreign_vertex_degrees)
}

fn insert_edge(
    graph_object_data: &mut Graph,
    source: VertexId,
    dest: VertexId,
    preferred_partition_idx: Option<usize>,
    partition_refs: &mut Vec<Arc<Object>>,
    fvd_refs: &mut Vec<Arc<Object>>,
    incoming_edges_refs: &mut Vec<Arc<Object>>,
    kvs_iptr: IPtr,
    pick_random: bool,
) -> usize {
    let mut found = None;
    for (idx, filter) in graph_object_data.filters.iter().enumerate() {
        let partition = partition_refs[idx]
            .read_into_mut::<GraphPartition>(None)
            .unwrap();
        if partition.adjacencies.contains(&source) {
            found = Some(idx);
            break;
        }
    }

    let src_partition_idx = match found {
        Some(idx) => {
            let partition = partition_refs[idx]
                .read_into_mut::<GraphPartition>(None)
                .unwrap();
            let adjacencies = &mut partition.adjacencies;
            let source_adj_list = adjacencies
                .get_mut(&source)
                .expect(&format!("source vertex {} not found", source));
            source_adj_list.push(dest);

            idx
        }
        None => {
            let idx = 'select_idx: {
                if !pick_random {
                    if let Some(idx) = preferred_partition_idx {
                        if graph_object_data.filters[idx].load_factor() < 0.95
                            && !partition_refs[idx].is_under_storage_pressure()
                        {
                            break 'select_idx idx;
                        }
                    }
                }

                let numbers = if pick_random {
                    let mut rng = SmallRng::from_entropy();
                    Vec::from_iter((0..graph_object_data.partitions.len()))
                        .as_slice()
                        .choose_multiple(&mut rng, graph_object_data.partitions.len())
                        .map(|e| *e)
                        .collect::<Vec<usize>>()
                } else {
                    (0..graph_object_data.filters.len()).collect::<Vec<usize>>()
                };

                for idx in numbers.into_iter() {
                    if graph_object_data.filters[idx].load_factor() < 0.95
                        && !partition_refs[idx].is_under_storage_pressure()
                    {
                        break 'select_idx idx;
                    }
                }
                let object_tracker = object_tracker_tls::get_local_object_tracker_instance();

                // FIXME @hack this is only supposed to matter for huge graph parsing
                let num_allocations = if pick_random { 4 } else { 1 };
                for _ in 0..num_allocations {
                    partition_refs.push(allocate_partition(
                        Arc::clone(&object_tracker),
                        graph_object_data,
                        pick_random,
                    ));

                    if !graph_object_data.is_directed {
                        fvd_refs.push(allocate_fvd_object(
                            Arc::clone(&object_tracker),
                            graph_object_data,
                            kvs_iptr.clone(),
                        ));
                    }

                    if graph_object_data.is_directed {
                        incoming_edges_refs.push(allocate_incoming_edge_object(
                            Arc::clone(&object_tracker),
                            graph_object_data,
                        ));
                    }
                }

                graph_object_data.partitions.len() - 1
            };

            let partition = partition_refs[idx]
                .read_into_mut::<GraphPartition>(None)
                .unwrap();
            let allocator = partition.get_allocator().unwrap();
            partition.adjacencies.insert(source, PVec::new());

            let source_adj_list = partition
                .adjacencies
                .get_mut(&source)
                .expect(&format!("source vertex {} not found", source));
            source_adj_list.set_allocator(Arc::clone(&allocator));
            source_adj_list.resize_to_capacity(8);

            source_adj_list.push(dest);
            assert!(graph_object_data.filters[idx].add(source) == pcuckoo::Status::Ok);
            graph_object_data.n_verts += 1;

            idx
        }
    };

    src_partition_idx
}

fn insert_vertex(
    graph_object_data: &mut Graph,
    vertex_to_insert: VertexId,
    src_vertex: VertexId,
    preferred_partition_idx: Option<usize>,
    partition_refs: &mut Vec<Arc<Object>>,
    fvd_refs: &mut Vec<Arc<Object>>,
    incoming_edges_refs: &mut Vec<Arc<Object>>,
    kvs_iptr: IPtr,
    pick_random: bool,
) {
    let mut stored_idx = None;
    for (idx, _) in graph_object_data.filters.iter().enumerate() {
        let partition = partition_refs[idx]
            .read_into_mut::<GraphPartition>(None)
            .unwrap();
        let adjacencies = &mut partition.adjacencies;

        if adjacencies.contains(&vertex_to_insert) {
            stored_idx = Some(idx);
            break;
        }
    }

    if stored_idx.is_none() {
        let idx = 'select_idx: {
            if !pick_random {
                if let Some(idx) = preferred_partition_idx {
                    if graph_object_data.filters[idx].load_factor() < 0.95
                        && !partition_refs[idx].is_under_storage_pressure()
                    {
                        break 'select_idx idx;
                    }
                }
            }

            let numbers = if pick_random {
                let mut rng = SmallRng::from_entropy();
                Vec::from_iter((0..graph_object_data.partitions.len()))
                    .as_slice()
                    .choose_multiple(&mut rng, graph_object_data.partitions.len())
                    .map(|e| *e)
                    .collect::<Vec<usize>>()
            } else {
                (0..graph_object_data.filters.len()).collect::<Vec<usize>>()
            };

            for idx in numbers.into_iter() {
                if graph_object_data.filters[idx].load_factor() < 0.95
                    && !partition_refs[idx].is_under_storage_pressure()
                {
                    break 'select_idx idx;
                }
            }
            let object_tracker = object_tracker_tls::get_local_object_tracker_instance();

            // FIXME @hack this is only supposed to matter for huge graph parsing
            let num_allocations = if pick_random { 4 } else { 1 };
            for _ in 0..num_allocations {
                partition_refs.push(allocate_partition(
                    Arc::clone(&object_tracker),
                    graph_object_data,
                    pick_random,
                ));

                if !graph_object_data.is_directed {
                    fvd_refs.push(allocate_fvd_object(
                        Arc::clone(&object_tracker),
                        graph_object_data,
                        kvs_iptr.clone(),
                    ));
                }

                incoming_edges_refs.push(allocate_incoming_edge_object(
                    Arc::clone(&object_tracker),
                    graph_object_data,
                ));
            }

            graph_object_data.partitions.len() - 1
        };

        let partition = partition_refs[idx]
            .read_into_mut::<GraphPartition>(None)
            .unwrap();
        let allocator = partition.get_allocator().unwrap();
        partition.adjacencies.insert(vertex_to_insert, PVec::new());

        let source_adj_list = partition
            .adjacencies
            .get_mut(&vertex_to_insert)
            .expect(&format!("source vertex {} not found", vertex_to_insert));
        source_adj_list.set_allocator(Arc::clone(&allocator));
        source_adj_list.resize_to_capacity(8);

        graph_object_data.filters[idx].add(vertex_to_insert);
        graph_object_data.n_verts += 1;

        stored_idx = Some(idx);
    }

    let idx = stored_idx.unwrap();
    if let Some(src_partition_idx) = preferred_partition_idx {
        let partition_incoming_edges = incoming_edges_refs[idx]
            .read_into_mut::<PartitionIncomingEdges>(None)
            .unwrap();
        match partition_incoming_edges
            .per_vertex_incoming
            .get_mut(&vertex_to_insert)
        {
            None => {
                partition_incoming_edges
                    .per_vertex_incoming
                    .insert(vertex_to_insert, PVec::new());
                let allocator = partition_incoming_edges
                    .per_vertex_incoming
                    .get_allocator()
                    .unwrap();
                let adj = partition_incoming_edges
                    .per_vertex_incoming
                    .get_mut(&vertex_to_insert)
                    .unwrap();
                adj.set_allocator(Arc::clone(&allocator));
                adj.resize_to_capacity(8);

                adj.push(src_vertex);
            }
            Some(incoming_adj) => {
                incoming_adj.push(src_vertex);
            }
        }
    }
}

#[nandoize_lib]
pub fn delete_repartition_chunk(repartition_chunk: &mut RepartitionChunks) {
    let object_tracker = object_tracker_tls::get_local_object_tracker_instance();
    let chunk_object_id = iptr_of_ref!(repartition_chunk).get_object_id();
    object_tracker.delete_object(chunk_object_id);
}

#[nandoize_lib]
pub fn delete_incoming_edges_chunk(incoming_edges_chunk: &mut IncomingEdgesChunk) {
    let object_tracker = object_tracker_tls::get_local_object_tracker_instance();
    let incoming_edges_chunk_id = iptr_of_ref!(incoming_edges_chunk).get_object_id();
    object_tracker.delete_object(incoming_edges_chunk_id);
}

#[nandoize_lib]
pub fn chunk_partitions(graph: &Graph, chunk_size: usize, only_partitions: bool) {
    let graph_iptr = iptr_of_ref!(graph);
    let mut partition_chunk = Vec::with_capacity(chunk_size);
    let mut incoming_edges_chunk = Vec::with_capacity(chunk_size);
    let mut fvds_chunk = Vec::with_capacity(chunk_size);

    for (idx, partition_iptr) in graph.partitions.iter().enumerate() {
        partition_chunk.push(partition_iptr.get_inner());

        if !only_partitions {
            match graph.is_directed {
                true => incoming_edges_chunk.push(graph.incoming_edges[idx].get_inner()),
                false => fvds_chunk.push(graph.foreign_vertex_degrees[idx].get_inner()),
            }
        }

        if partition_chunk.len() == chunk_size {
            let p_chunk = partition_chunk.clone();
            let ie_chunk = incoming_edges_chunk.clone();
            let fvd_chunk = fvds_chunk.clone();
            nando_spawn!(
                "nano4r::visit_partition_chunk",
                graph_iptr,
                p_chunk,
                ie_chunk,
                fvd_chunk
            );
            partition_chunk.truncate(0);
            incoming_edges_chunk.truncate(0);
            fvds_chunk.truncate(0);
        }
    }

    if partition_chunk.is_empty() {
        return;
    }

    nando_spawn!(
        "nano4r::visit_partition_chunk",
        graph_iptr,
        partition_chunk,
        incoming_edges_chunk,
        fvds_chunk
    );
}

#[nandoize_lib]
pub fn visit_partition_chunk(
    _graph: &Graph,
    _partitions: Vec<&GraphPartition>,
    _incoming_edges: Vec<&PartitionIncomingEdges>,
    _fvds: Vec<&ForeignVertexDegrees>,
) {
    /* noop */
}
