use std::cmp::Ordering;
use std::collections::HashMap;
use std::mem::size_of as mem_size_of;
use std::sync::Arc;

use nando_support::{
    allocate_and_init, bump_if_changed, format_intent_name,
    iptr::{IPtr, TypedIPtr},
    iptr_of_ref, nando_spawn, nando_spawn_polymorphic, nando_yield, ObjectId,
};
use nandoize::{nandoize_lib, PersistableDeriveLib};
use object_lib::{
    allocators::{
        bump_allocator::BumpAllocator, persistently_allocatable::PersistentlyAllocatable,
    },
    collections::{pmap::PHashMap, pvec::PVec},
    tls as object_lib_tls,
};
use object_lib::{MaterializedObjectVersion, Object, Persistable};
use object_tracker::{object_tracker_tls, unit_ptr_of};
use ownership_tracker::ownership_tracker_tls;
use parking_lot::RwLock;

use crate::definitions::*;

// Note that this only really works for undirected graphs.
#[repr(C)]
#[derive(PersistableDeriveLib)]
pub struct TriangleAccumulator {
    pub count: usize,
}

impl PersistentlyAllocatable for TriangleAccumulator {}

#[repr(C)]
#[derive(PersistableDeriveLib)]
pub struct RemoteNeighbors {
    target_neighbors: PVec<VertexId>,
    remote_vertices: PVec<VertexId>,
}

impl PersistentlyAllocatable for RemoteNeighbors {
    fn set_allocator(&mut self, allocator: Arc<RwLock<BumpAllocator>>) {
        self.target_neighbors.set_allocator(Arc::clone(&allocator));
        self.remote_vertices.set_allocator(allocator);
    }

    fn get_allocator(&self) -> Option<Arc<RwLock<BumpAllocator>>> {
        self.target_neighbors.get_allocator()
    }
}

#[repr(C)]
#[derive(PersistableDeriveLib)]
pub struct PartitionEffectiveNeighbors {
    sorted_effective_neighbors: PHashMap<VertexId, PVec<VertexId>>,
    vd_table: TypedIPtr<kvs::RootBlock>,
}

impl PersistentlyAllocatable for PartitionEffectiveNeighbors {
    fn set_allocator(&mut self, allocator: Arc<RwLock<BumpAllocator>>) {
        self.sorted_effective_neighbors.set_allocator(allocator);
    }

    fn get_allocator(&self) -> Option<Arc<RwLock<BumpAllocator>>> {
        self.sorted_effective_neighbors.get_allocator()
    }
}

#[nandoize_lib]
pub fn spawn_partition_counters(graph: &Graph) -> IPtr {
    let object_tracker = object_tracker_tls::get_local_object_tracker_instance();
    let triangle_accumulator = allocate_and_init!(object_tracker, TriangleAccumulator);
    let accumulator_iptr = triangle_accumulator.iptr_of();

    // we only need one bucket, regardless of the number of partitions -- the only thing we expect
    // to store here is a set of (object id, object id) pairs (as many as the number of partitions, should fit within
    // a single bucket)
    let search_kvs: IPtr = kvs::init_kvs::<ObjectId>(1, graph.partitions.len() as u64);
    let search_kvs_id = search_kvs.object_id;

    let graph_iptr = object_lib_tls::iptr_of(unit_ptr_of!(graph)).unwrap();
    for (partition, foreign_vertex_degrees) in
        std::iter::zip(graph.partitions.iter(), graph.foreign_vertex_degrees.iter())
    {
        let partial_acc = nando_spawn!(
            "nano4r::count_triangles",
            graph_iptr,
            partition,
            foreign_vertex_degrees
        );
        nando_yield!("nano4r::tc_merge_partial", accumulator_iptr, partial_acc);
    }

    let _ = triangle_accumulator.bump_version();
    object_tracker.push_initial_version(triangle_accumulator.id, (&*triangle_accumulator).into());
    ownership_tracker_tls::mark_object_owned(triangle_accumulator.id);

    accumulator_iptr
}

#[nandoize_lib]
pub fn graph_compute_foreign_degrees(graph: &Graph) {
    let graph_iptr = iptr_of_ref!(graph);
    for (partition, foreign_vertex_degrees) in
        std::iter::zip(graph.partitions.iter(), graph.foreign_vertex_degrees.iter())
    {
        nando_spawn!(
            "nano4r::compute_foreign_degrees",
            graph_iptr,
            partition,
            foreign_vertex_degrees
        );
    }
}

#[nandoize_lib]
pub fn compute_foreign_degrees(
    graph: &Graph,
    partition: &GraphPartition,
    foreign_vertex_degree: &mut ForeignVertexDegrees,
) {
    let object_tracker = object_tracker_tls::get_local_object_tracker_instance();
    let foreign_vertex_degrees_iptr = iptr_of_ref!(foreign_vertex_degree);

    let graph_iptr = iptr_of_ref!(graph);
    let own_partition_iptr = iptr_of_ref!(partition);

    let mut remote_neighbor_partition_cache: HashMap<VertexId, Vec<ObjectId>> =
        HashMap::with_capacity(partition.adjacencies.len());
    for (vertex, neighbors) in &partition.adjacencies {
        for neighbor in neighbors {
            if partition.adjacencies.contains(neighbor) {
                continue;
            }

            foreign_vertex_degree.degree.insert(*neighbor, 0);
            let partition_ids = match remote_neighbor_partition_cache.get(neighbor) {
                Some(ref p) => p,
                None => {
                    let partitions = graph
                        .partitions
                        .iter()
                        .enumerate()
                        .filter(|(idx, partition_iptr)| {
                            !(partition_iptr.get_inner().get_object_id()
                                == own_partition_iptr.get_object_id()
                                || !graph.filters[*idx].contains(*neighbor))
                        })
                        .map(|(_, partition_iptr)| partition_iptr.get_inner().get_object_id())
                        .collect();

                    remote_neighbor_partition_cache.insert(*neighbor, partitions);

                    remote_neighbor_partition_cache.get(neighbor).unwrap()
                }
            };

            for partition_id in partition_ids {
                if !foreign_vertex_degree.per_partition.contains(&partition_id) {
                    foreign_vertex_degree
                        .per_partition
                        .insert(*partition_id, PHashMap::new());
                    let partition_vertex_map = foreign_vertex_degree
                        .per_partition
                        .get_mut(&partition_id)
                        .unwrap();
                    partition_vertex_map
                        .set_allocator(foreign_vertex_degree.get_allocator().unwrap());
                    partition_vertex_map.with_capacity(partition.adjacencies.len());

                    partition_vertex_map.insert(*neighbor, true);
                    continue;
                }

                let partition_vertex_map = foreign_vertex_degree
                    .per_partition
                    .get_mut(&partition_id)
                    .unwrap();
                if partition_vertex_map.contains(neighbor) {
                    continue;
                }

                partition_vertex_map.insert(*neighbor, true);
            }
        }
    }

    let partition_object_key = own_partition_iptr.object_id.to_string();
    let foreign_vertex_degrees_id = foreign_vertex_degrees_iptr.object_id;

    for (partition_id, _) in &foreign_vertex_degree.per_partition {
        let partition_iptr = IPtr::new(*partition_id, 0, 0);
        let partial_iptr = nando_spawn!(
            "nano4r::tas_extract_degrees",
            graph_iptr,
            partition_iptr,
            foreign_vertex_degrees_iptr
        );

        let partition_id = *partition_id;
        nando_yield!(
            "nano4r::update_degrees",
            partial_iptr,
            partition_id,
            foreign_vertex_degrees_iptr
        );
    }
}

#[nandoize_lib]
pub fn tas_extract_degrees(
    graph: &Graph,
    partition: &GraphPartition,
    foreign_vertex_degrees: &ForeignVertexDegrees,
) -> IPtr {
    let object_tracker = object_tracker_tls::get_local_object_tracker_instance();
    let partition_degrees = allocate_and_init!(object_tracker, ForeignVertexDegrees);
    let partition_degrees_iptr = partition_degrees.iptr_of();
    let partition_degrees_data = partition_degrees
        .read_into_mut::<ForeignVertexDegrees>(None)
        .unwrap();
    partition_degrees_data
        .degree
        .with_capacity(partition.adjacencies.len());

    let partition_iptr = iptr_of_ref!(partition);
    let current_partition_vertices = match foreign_vertex_degrees
        .per_partition
        .get(&partition_iptr.get_object_id())
    {
        None => unreachable!("partition entry not found"),
        Some(e) => e,
    };

    for (vertex, _) in current_partition_vertices {
        if !partition.adjacencies.contains(&vertex) {
            continue;
        }

        let degree = partition.adjacencies.get(&vertex).unwrap().len();
        partition_degrees_data.degree.insert(*vertex, degree);
    }

    let _ = partition_degrees.bump_version();
    object_tracker.push_initial_version(partition_degrees.id, (&*partition_degrees).into());
    ownership_tracker_tls::mark_object_owned(partition_degrees.id);

    partition_degrees_iptr
}

#[nandoize_lib]
pub fn update_degrees(
    partition_degrees: &ForeignVertexDegrees,
    partition_id: ObjectId,
    foreign_vertex_degrees: &mut ForeignVertexDegrees,
) {
    let current_partition_vertices = match foreign_vertex_degrees.per_partition.get(&partition_id) {
        None => return,
        Some(e) => e,
    };

    for (vertex, degree) in &partition_degrees.degree {
        foreign_vertex_degrees.degree.insert(*vertex, *degree);
    }

    // let partition_degree_object_iptr = iptr_of_ref!(partition_degrees);
    // nando_spawn!("nano4r::delete_degree_object", partition_degree_object_iptr);
}

#[nandoize_lib]
pub fn delete_degree_object(partition_degrees: &mut ForeignVertexDegrees) {
    let object_tracker = object_tracker_tls::get_local_object_tracker_instance();
    let partition_degree_object_id = iptr_of_ref!(partition_degrees).get_object_id();
    object_tracker.delete_object(partition_degree_object_id);
}

#[nandoize_lib]
pub fn tc_merge_partial(total_acc: &mut TriangleAccumulator, partial_acc: &TriangleAccumulator) {
    total_acc.count += partial_acc.count;
}

fn node_ordering(u: VertexId, degree_u: usize, v: VertexId, degree_v: VertexId) -> Ordering {
    if degree_u < degree_v || (degree_u == degree_v && u < v) {
        return Ordering::Less;
    }

    Ordering::Greater
}

fn get_intersection_cardinality(
    effective_neighbors_u: Arc<Vec<VertexId>>,
    effective_neighbors_v: Arc<Vec<VertexId>>,
) -> usize {
    let mut cardinality = 0;

    let mut idx_u = 0;
    let mut idx_v = 0;

    loop {
        if idx_u == effective_neighbors_u.len() || idx_v == effective_neighbors_v.len() {
            break;
        }

        if effective_neighbors_u[idx_u] == effective_neighbors_v[idx_v] {
            cardinality += 1;
            idx_u += 1;
            idx_v += 1;

            continue;
        } else if effective_neighbors_u[idx_u] < effective_neighbors_v[idx_v] {
            idx_u += 1;
        } else {
            idx_v += 1;
        }
    }

    cardinality
}

fn get_intersection_cardinality_persisted(
    effective_neighbors_u: &PVec<VertexId>,
    effective_neighbors_v: &PVec<VertexId>,
) -> usize {
    let mut cardinality = 0;

    let mut iter_u = effective_neighbors_u.iter();
    let mut iter_v = effective_neighbors_v.iter();

    let mut current_u = iter_u.next();
    let mut current_v = iter_v.next();

    loop {
        if current_u.is_none() || current_v.is_none() {
            break;
        }

        let inner_u = current_u.unwrap();
        let inner_v = current_v.unwrap();
        if inner_u == inner_v {
            cardinality += 1;
            current_u = iter_u.next();
            current_v = iter_v.next();

            continue;
        } else if inner_u < inner_v {
            current_u = iter_u.next();
        } else {
            current_v = iter_v.next();
        }
    }

    cardinality
}

#[inline]
fn compute_effective_neighbors(
    vertex: VertexId,
    partition: &GraphPartition,
    foreign_vertex_degrees: &ForeignVertexDegrees,
) -> Vec<VertexId> {
    let neighbors = partition.adjacencies.get(&vertex).unwrap();
    let current_vertex_degree = neighbors.len();

    let mut effective_neighbors: Vec<(VertexId, usize)> = neighbors
        .iter()
        .map(|e| {
            let degree = match partition.adjacencies.get(e) {
                None => *foreign_vertex_degrees.degree.get(e).unwrap(),
                Some(ref a) => a.len(),
            };

            (*e, degree)
        })
        .filter(|(n, degree)| {
            if *degree < current_vertex_degree || (*degree == current_vertex_degree && *n < vertex)
            {
                return false;
            }

            true
        })
        .collect();

    effective_neighbors
        .sort_by(|(u, degree_u), (v, degree_v)| node_ordering(*u, *degree_u, *v, *degree_v));

    effective_neighbors.into_iter().map(|(v, _)| v).collect()
}

type CachedNeighborLists = (Arc<Vec<VertexId>>, Arc<Vec<VertexId>>);

#[inline]
fn get_and_cache_effective_neighbors(
    vertex: VertexId,
    partition: &GraphPartition,
    foreign_vertex_degrees: &ForeignVertexDegrees,
    cached_effective_neighbors: &mut HashMap<VertexId, CachedNeighborLists>,
) -> (CachedNeighborLists, bool) {
    if let Some(ref en) = cached_effective_neighbors.get(&vertex) {
        return ((Arc::clone(&en.0), Arc::clone(&en.1)), true);
    }

    let effective_neighbors =
        compute_effective_neighbors(vertex, partition, foreign_vertex_degrees);
    let mut sorted_neighbors: Vec<VertexId> = effective_neighbors.iter().cloned().collect();
    sorted_neighbors.sort();
    cached_effective_neighbors.insert(
        vertex,
        (Arc::new(effective_neighbors), Arc::new(sorted_neighbors)),
    );
    let res = cached_effective_neighbors.get(&vertex).unwrap();
    ((Arc::clone(&res.0), Arc::clone(&res.1)), false)
}

#[nandoize_lib]
pub fn count_triangles(
    graph: &Graph,
    partition: &mut GraphPartition,
    foreign_vertex_degrees: &ForeignVertexDegrees,
) -> IPtr {
    let object_tracker = object_tracker_tls::get_local_object_tracker_instance();
    let partition_accumulator = allocate_and_init!(object_tracker, TriangleAccumulator);
    let partition_accumulator_data = partition_accumulator
        .read_into_mut::<TriangleAccumulator>(None)
        .unwrap();
    let partition_accumulator_iptr = partition_accumulator.iptr_of();

    let mut cached_effective_neighbors = HashMap::with_capacity(partition.adjacencies.len());
    let cached_effective_neighbors_object =
        allocate_and_init!(object_tracker, PartitionEffectiveNeighbors);
    let cached_effective_neighbors_data = cached_effective_neighbors_object
        .read_into_mut::<PartitionEffectiveNeighbors>(None)
        .unwrap();
    cached_effective_neighbors_data
        .sorted_effective_neighbors
        .with_capacity(partition.adjacencies.len());
    cached_effective_neighbors_data.vd_table = {
        let inner = foreign_vertex_degrees.vd_table.get_inner();
        TypedIPtr::from(inner)
    };

    let graph_iptr = object_lib_tls::iptr_of(unit_ptr_of!(graph)).unwrap();
    let own_partition_iptr = object_lib_tls::iptr_of(unit_ptr_of!(partition)).unwrap();
    let foreign_vertex_degree_iptr =
        object_lib_tls::iptr_of(unit_ptr_of!(foreign_vertex_degrees)).unwrap();

    let mut remote_neighbor_partition_cache: HashMap<VertexId, Vec<ObjectId>> =
        HashMap::with_capacity(foreign_vertex_degrees.degree.len());
    let mut per_partition_remote_neighbors: HashMap<ObjectId, Arc<Object>> =
        HashMap::with_capacity(graph.partitions.len());
    for (vertex, neighbors) in &partition.adjacencies {
        let ((effective_neighbors, sorted_neighbors), was_cached) =
            get_and_cache_effective_neighbors(
                *vertex,
                partition,
                foreign_vertex_degrees,
                &mut cached_effective_neighbors,
            );

        if !was_cached {
            cached_effective_neighbors_data
                .sorted_effective_neighbors
                .insert(*vertex, PVec::new());
            let mut entry = cached_effective_neighbors_data
                .sorted_effective_neighbors
                .get_mut(vertex)
                .unwrap();
            entry.set_allocator(cached_effective_neighbors_data.get_allocator().unwrap());
            entry.resize_to_capacity(sorted_neighbors.len());
            unsafe { entry.copy_from_vec(sorted_neighbors.as_ref()) };
        }

        for neighbor in effective_neighbors.iter() {
            if partition.adjacencies.contains(neighbor) {
                let ((_, adjacency_sorted_neighbors), was_cached) =
                    get_and_cache_effective_neighbors(
                        *neighbor,
                        partition,
                        foreign_vertex_degrees,
                        &mut cached_effective_neighbors,
                    );

                if !was_cached {
                    cached_effective_neighbors_data
                        .sorted_effective_neighbors
                        .insert(*neighbor, PVec::new());
                    let mut entry = cached_effective_neighbors_data
                        .sorted_effective_neighbors
                        .get_mut(neighbor)
                        .unwrap();
                    entry.set_allocator(cached_effective_neighbors_data.get_allocator().unwrap());
                    entry.resize_to_capacity(adjacency_sorted_neighbors.len());
                    unsafe { entry.copy_from_vec(adjacency_sorted_neighbors.as_ref()) };
                }

                partition_accumulator_data.count += get_intersection_cardinality(
                    Arc::clone(&sorted_neighbors),
                    adjacency_sorted_neighbors,
                );

                continue;
            }

            let partition_ids = match remote_neighbor_partition_cache.get(neighbor) {
                Some(ref p) => p,
                None => {
                    let partitions = graph
                        .partitions
                        .iter()
                        .enumerate()
                        .filter(|(idx, partition_iptr)| {
                            !(partition_iptr.get_inner().get_object_id()
                                == own_partition_iptr.get_object_id()
                                || !graph.filters[*idx].contains(*neighbor))
                        })
                        .map(|(_, partition_iptr)| partition_iptr.get_inner().get_object_id())
                        .collect();

                    remote_neighbor_partition_cache.insert(*neighbor, partitions);

                    remote_neighbor_partition_cache.get(neighbor).unwrap()
                }
            };

            for partition_id in partition_ids {
                match per_partition_remote_neighbors.get_mut(&partition_id) {
                    Some(ref e) => {
                        let remote_neighbors_data =
                            e.read_into_mut::<RemoteNeighbors>(None).unwrap();

                        remote_neighbors_data.target_neighbors.push(*neighbor);
                        remote_neighbors_data.remote_vertices.push(*vertex);
                    }
                    None => {
                        let object_tracker =
                            object_tracker_tls::get_local_object_tracker_instance();
                        let remote_neighbors = allocate_and_init!(object_tracker, RemoteNeighbors);
                        let remote_neighbors_data = remote_neighbors
                            .read_into_mut::<RemoteNeighbors>(None)
                            .unwrap();
                        remote_neighbors_data
                            .target_neighbors
                            .resize_to_capacity(64);
                        remote_neighbors_data.remote_vertices.resize_to_capacity(64);

                        remote_neighbors_data.target_neighbors.push(*neighbor);
                        remote_neighbors_data.remote_vertices.push(*vertex);

                        per_partition_remote_neighbors.insert(*partition_id, remote_neighbors);
                    }
                }
            }
        }
    }

    let cached_effective_neighbors_iptr = cached_effective_neighbors_object.iptr_of();
    let table_ptr: IPtr = foreign_vertex_degrees.vd_table.get_inner();

    // Publish our object with our cached effective neighbor results.
    let cached_effective_neighbors_key = format!("cn_{}", own_partition_iptr.get_object_id());
    let cached_effective_neighbors_id = cached_effective_neighbors_object.id;
    let partition_neighbors_store = nando_spawn_polymorphic!(
        "kvs::put::<ObjectId>",
        table_ptr,
        cached_effective_neighbors_key,
        cached_effective_neighbors_id
    );

    for (partition_id, neighbors_object) in &per_partition_remote_neighbors {
        let _ = neighbors_object.bump_version();
        object_tracker.push_initial_version(neighbors_object.id, (&**neighbors_object).into());
        ownership_tracker_tls::mark_object_owned(neighbors_object.id);

        let neighbors_object_iptr = neighbors_object.iptr_of();
        let cached_neighbors_key = format!("cn_{}", *partition_id);
        let cached_effective_neighbors_object_id =
            nando_spawn_polymorphic!("kvs::get::<ObjectId>", table_ptr, cached_neighbors_key);

        let partition_id = *partition_id;
        nando_yield!(
            "nano4r::tas_update_triangle_count_from_neighbors",
            graph_iptr,
            neighbors_object_iptr,
            partition_id,
            cached_effective_neighbors_object_id,
            partition_accumulator_iptr,
            cached_effective_neighbors_iptr
        );
    }

    let _ = partition_accumulator.bump_version();
    object_tracker.push_initial_version(partition_accumulator.id, (&*partition_accumulator).into());
    ownership_tracker_tls::mark_object_owned(partition_accumulator.id);

    let _ = cached_effective_neighbors_object.bump_version();
    object_tracker.push_initial_version(
        cached_effective_neighbors_object.id,
        (&*cached_effective_neighbors_object).into(),
    );
    ownership_tracker_tls::mark_object_owned(cached_effective_neighbors_object.id);

    partition_accumulator.iptr_of()
}

#[nandoize_lib]
pub fn tas_update_triangle_count_from_neighbors(
    graph: &Graph,
    remote_neighbors: &mut RemoteNeighbors,
    target_neighbor_partition_id: ObjectId,
    cached_effective_neighbors_object_id: Option<ObjectId>,
    partition_accumulator: &TriangleAccumulator,
    cached_effective_neighbors: &PartitionEffectiveNeighbors,
) {
    let graph_iptr = object_lib_tls::iptr_of(unit_ptr_of!(graph)).unwrap();

    let partition_accumulator_iptr =
        object_lib_tls::iptr_of(unit_ptr_of!(partition_accumulator)).unwrap();

    let remote_neighbors_iptr = iptr_of_ref!(remote_neighbors);
    let cached_effective_neighbors_iptr = iptr_of_ref!(cached_effective_neighbors);

    if cached_effective_neighbors_object_id.is_none() {
        /*
        println!(
            "kvs returned empty result for target neighbor vertex degree object, calling self"
        );
        */

        let table_ptr = cached_effective_neighbors.vd_table.get_inner();
        let cached_neighbors_key = format!("cn_{}", target_neighbor_partition_id);
        let cached_effective_neighbors_object_id =
            nando_spawn_polymorphic!("kvs::get::<ObjectId>", table_ptr, cached_neighbors_key);

        nando_yield!(
            "nano4r::tas_update_triangle_count_from_neighbors",
            graph_iptr,
            remote_neighbors_iptr,
            target_neighbor_partition_id,
            cached_effective_neighbors_object_id,
            partition_accumulator_iptr,
            cached_effective_neighbors_iptr
        );

        return;
    };

    let target_partition_cached_effective_neighbors_object_id =
        cached_effective_neighbors_object_id.unwrap();
    let target_partition_cached_effective_neighbors_iptr =
        IPtr::new(target_partition_cached_effective_neighbors_object_id, 0, 0);

    let partial = nando_spawn!(
        "nano4r::update_triangle_count_from_neighbors",
        graph_iptr,
        remote_neighbors_iptr,
        target_partition_cached_effective_neighbors_iptr,
        cached_effective_neighbors_iptr
    );

    nando_yield!(
        "nano4r::update_partial",
        partial,
        partition_accumulator_iptr
    );
}

#[nandoize_lib]
pub fn update_triangle_count_from_neighbors(
    graph: &Graph,
    remote_neighbors: &mut RemoteNeighbors,
    target_cached_effective_neighbors: &PartitionEffectiveNeighbors,
    cached_effective_neighbors: &PartitionEffectiveNeighbors,
) -> IPtr {
    let object_tracker = object_tracker_tls::get_local_object_tracker_instance();
    let partition_accumulator = allocate_and_init!(object_tracker, TriangleAccumulator);
    let partition_accumulator_data = partition_accumulator
        .read_into_mut::<TriangleAccumulator>(None)
        .unwrap();
    let partition_accumulator_iptr = partition_accumulator.iptr_of();

    let mut target_neighbors_iter = remote_neighbors.target_neighbors.iter();
    let mut remote_vertices_iter = remote_neighbors.remote_vertices.iter();

    let mut target_neighbor = target_neighbors_iter.next();
    let mut remote_neighbor = remote_vertices_iter.next();
    while target_neighbor.is_some() {
        let target_neighbor_inner = target_neighbor.unwrap();
        let remote_neighbor_inner = remote_neighbor.unwrap();
        if !target_cached_effective_neighbors
            .sorted_effective_neighbors
            .contains(&target_neighbor_inner)
        {
            target_neighbor = target_neighbors_iter.next();
            remote_neighbor = remote_vertices_iter.next();
            continue;
        }

        let target_effective_neighbors = target_cached_effective_neighbors
            .sorted_effective_neighbors
            .get(target_neighbor_inner)
            .expect("no entry for target neighbor");
        let cached_effective_neighbors = cached_effective_neighbors
            .sorted_effective_neighbors
            .get(remote_neighbor_inner);
        let Some(cached_effective_neighbors) = cached_effective_neighbors else {
            panic!(
                "no entry for cached remote neighbor {}",
                remote_neighbor_inner
            );
        };

        partition_accumulator_data.count += get_intersection_cardinality_persisted(
            target_effective_neighbors,
            cached_effective_neighbors,
        );

        target_neighbor = target_neighbors_iter.next();
        remote_neighbor = remote_vertices_iter.next();
    }

    let _ = partition_accumulator.bump_version();
    object_tracker.push_initial_version(partition_accumulator.id, (&*partition_accumulator).into());
    ownership_tracker_tls::mark_object_owned(partition_accumulator.id);

    partition_accumulator_iptr
}

#[nandoize_lib]
pub fn update_partial(
    partial_acc: &TriangleAccumulator,
    partition_accumulator: &mut TriangleAccumulator,
) {
    partition_accumulator.count += partial_acc.count;
}

#[nandoize_lib]
pub fn print_number_of_triangles(accumulator: &TriangleAccumulator) {
    println!("Total number of triangles detected: {}", accumulator.count);
}
