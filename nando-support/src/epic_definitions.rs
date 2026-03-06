use serde::{Deserialize, Serialize};

use crate::{activation_intent::NandoResultSerializable, ecb_id::EcbId, epic_control::TaskStatus};

#[derive(Serialize, Deserialize)]
#[serde(remote = "TaskStatus")]
pub enum TaskStatusDef {
    Pending,
    PendingUnresolvedDependencies,
    InProgress,
    Success,
    Failure,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct GetEpicStatusRequest {
    pub ecb_id: EcbId,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct EpicStatus {
    #[serde(with = "TaskStatusDef")]
    pub status: TaskStatus,
    pub result: Option<NandoResultSerializable>,
    // TODO maybe we should also include the spawned ecbs that correspond to subgraphs that are yet
    // to complete?
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub enum EpicStatusResponse {
    NotFound(String),
    Status(EpicStatus),
}

#[derive(Clone, Serialize, Deserialize)]
pub struct AwaitEpicResultRequest {
    pub ecb_id: EcbId,
}
