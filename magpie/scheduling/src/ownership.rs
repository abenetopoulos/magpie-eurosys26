use object_lib::{ObjectId, ObjectVersion};
use serde::{Deserialize, Serialize};

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
}

#[derive(Clone, Serialize, Deserialize)]
pub struct AssumeOwnershipResponse {
    pub signature: Vec<u8>,
}
