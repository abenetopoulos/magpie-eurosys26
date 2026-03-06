use std::sync::Arc;

use nando_support::iptr::TypedIPtr;
use nandoize::PersistableDeriveLib;
use object_lib::Persistable;
use object_lib::{
    allocators::{
        bump_allocator::BumpAllocator, persistently_allocatable::PersistentlyAllocatable,
    },
    collections::{pcuckoo::PCuckooFilter, pmap::PHashMap, pvec::PVec},
    ObjectId,
};
use parking_lot::RwLock;

pub type VertexId = usize;

#[repr(C)]
#[derive(PersistableDeriveLib)]
pub struct ForeignVertexDegrees {
    pub(crate) degree: PHashMap<VertexId, usize>,
    pub(crate) per_partition: PHashMap<ObjectId, PHashMap<VertexId, bool>>,
    pub(crate) vd_table: TypedIPtr<kvs::RootBlock>,
}

impl PersistentlyAllocatable for ForeignVertexDegrees {
    fn set_allocator(&mut self, allocator: Arc<RwLock<BumpAllocator>>) {
        self.degree.set_allocator(Arc::clone(&allocator));
        self.per_partition.set_allocator(allocator);
    }

    fn get_allocator(&self) -> Option<Arc<RwLock<BumpAllocator>>> {
        self.degree.get_allocator()
    }
}

impl ForeignVertexDegrees {
    pub fn get_degree(&self, vertex: &VertexId) -> Option<usize> {
        self.degree.get(vertex).copied()
    }
}

#[repr(C)]
#[derive(PersistableDeriveLib)]
pub struct Graph {
    pub(crate) filters: PVec<PCuckooFilter>,
    pub(crate) partitions: PVec<TypedIPtr<GraphPartition>>,
    // only contains elements in the case of a directed graph.
    pub(crate) incoming_edges: PVec<TypedIPtr<PartitionIncomingEdges>>,
    pub(crate) foreign_vertex_degrees: PVec<TypedIPtr<ForeignVertexDegrees>>,
    pub(crate) n_verts: usize,
    pub(crate) n_edges: usize,
    pub(crate) is_directed: bool,
    pub(crate) min_vertex_id: VertexId,
    pub(crate) max_vertex_id: VertexId,
}

impl PersistentlyAllocatable for Graph {
    fn set_allocator(&mut self, allocator: Arc<RwLock<BumpAllocator>>) {
        self.filters.set_allocator(Arc::clone(&allocator));
        self.partitions.set_allocator(Arc::clone(&allocator));
        self.incoming_edges.set_allocator(Arc::clone(&allocator));
        self.foreign_vertex_degrees.set_allocator(allocator);
    }

    fn get_allocator(&self) -> Option<Arc<RwLock<BumpAllocator>>> {
        self.partitions.get_allocator()
    }
}

#[repr(C)]
#[derive(PersistableDeriveLib)]
pub struct GraphPartition {
    pub(crate) adjacencies: PHashMap<VertexId, PVec<VertexId>>,
}

impl PersistentlyAllocatable for GraphPartition {
    fn set_allocator(&mut self, allocator: Arc<RwLock<BumpAllocator>>) {
        self.adjacencies.set_allocator(allocator);
    }

    fn get_allocator(&self) -> Option<Arc<RwLock<BumpAllocator>>> {
        self.adjacencies.get_allocator()
    }
}

#[repr(C)]
#[derive(PersistableDeriveLib)]
pub struct PartitionIncomingEdges {
    pub(crate) per_vertex_incoming: PHashMap<VertexId, PVec<VertexId>>,
}

impl PersistentlyAllocatable for PartitionIncomingEdges {
    fn set_allocator(&mut self, allocator: Arc<RwLock<BumpAllocator>>) {
        self.per_vertex_incoming.set_allocator(allocator);
    }

    fn get_allocator(&self) -> Option<Arc<RwLock<BumpAllocator>>> {
        self.per_vertex_incoming.get_allocator()
    }
}
