use std::collections::HashMap;

use nando_support::{iptr::IPtr, ObjectVersion};
pub use nando_support::{HostIdx, ObjectId};
use serde::{Deserialize, Serialize};

pub type Hostname = String;

#[derive(Serialize, Deserialize)]
pub struct ScheduleResponse {
    pub target_host: Hostname,
    pub had_to_consolidate: bool,
}

// TODO We should remove this definition -- I have a hard time imagining a good use case for
// standalone consolidation requests (i.e. requests for consolidation that do not directly
// correspond to a scheduling request)
#[derive(Serialize, Deserialize)]
pub struct ConsolidationIntent {
    pub to_host: HostIdx,
    pub args: Vec<ObjectId>,
    pub versions: Vec<ObjectVersion>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct PublishRequest {
    pub host_idx: HostIdx,
    pub object: IPtr,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct MultiPublishRequest {
    pub host_idx: HostIdx,
    pub objects: Vec<IPtr>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct RegisterWorkerRequest {
    pub host_idx: Option<HostIdx>,
    pub hostname: Hostname,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct RegisterWorkerResponse {
    pub host_idx: HostIdx,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkerMapping {
    pub mapping: HashMap<HostIdx, Hostname>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct MoveOwnershipRequest {
    // NOTE Ideally this would be a Vec<IPtr>, but serde cannot currently support deserializing
    // remotes within container types (https://github.com/serde-rs/serde/issues/723), and I don't
    // want to wrap the vec's contents in some kind of singleton enum, so we will just do object
    // ids for now, which is fine.
    pub object_refs: Vec<ObjectId>,
    pub new_host: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct MoveOwnershipResponse {
    pub whomstone_versions: Vec<(ObjectId, ObjectVersion)>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct AssumeOwnershipRequest {
    // TODO batching
    pub object_id: ObjectId,
    pub first_version: ObjectVersion,
    pub get_signature: bool,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct AssumeOwnershipResponse {
    pub signature: Vec<u8>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct AddCacheMappingRequest {
    pub original_object_id: ObjectId,
    pub cached_object_id: ObjectId,
    pub first_version: ObjectVersion,
    pub original_owner_idx: HostIdx,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct AddCacheMappingResponse {}

#[derive(Serialize, Deserialize)]
pub struct OwnershipSnapshotResponse {
    pub snapshot: HashMap<ObjectId, HostIdx>,
}
