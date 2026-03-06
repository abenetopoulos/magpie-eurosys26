//! Client for initiating object moves between hosts and monitoring their status.
use std::collections::HashMap;

use async_std::sync::Mutex;
use location_mgr::{data_request_client, MoveProperties, TriggerMoveProperties};
use num_traits::FromPrimitive;
use object_lib::ObjectId;
use tonic::transport::channel::Channel;

use crate::error::{InternalError, MoveError};
use crate::object_move_handle::ObjectMoveHandle;
use crate::HostId;

pub mod location_mgr {
    tonic::include_proto!("dm");
}

type ConcreteDataRequestClient = data_request_client::DataRequestClient<Channel>;

impl From<location_mgr::ObjectMoveHandle> for ObjectMoveHandle {
    fn from(src: location_mgr::ObjectMoveHandle) -> Self {
        let object_id = src.id.parse::<ObjectId>().unwrap();
        Self::new(
            object_id,
            Some(FromPrimitive::from_i32(src.move_status).unwrap()),
        )
    }
}

pub struct DataRequestClient {
    server_port: u16,
    host_clients: Mutex<HashMap<HostId, ConcreteDataRequestClient>>,
}

impl DataRequestClient {
    pub fn new(server_port: u16) -> Self {
        Self {
            server_port,
            host_clients: Mutex::new(HashMap::new()),
        }
    }

    async fn get_client(
        &self,
        host_id: &HostId,
    ) -> Result<ConcreteDataRequestClient, InternalError> {
        let key = match std::thread::current().name() {
            Some(ref thread_name) => {
                format!("{}:{}", thread_name.to_string(), host_id)
            }
            None => host_id.to_string(),
        };
        let cached_client = {
            let host_clients = self.host_clients.lock().await;
            match host_clients.get(&key) {
                Some(c) => Some(c.clone()),
                None => None,
            }
        };

        if cached_client.is_some() {
            return Ok(cached_client.unwrap());
        }

        let server_addr = format!("http://{}:{}", host_id, self.server_port);
        let client = match data_request_client::DataRequestClient::connect(server_addr).await {
            Ok(c) => {
                let mut host_clients = self.host_clients.lock().await;
                host_clients.insert(key.clone(), c);
                host_clients.get(&key).unwrap().clone()
            }
            Err(e) => {
                eprintln!(
                    "[DataRequestClient] Failed to establish connection with host {}: {}",
                    host_id, e
                );
                return Err(InternalError::UnknownError());
            }
        };

        Ok(client)
    }

    pub async fn move_object(
        &self,
        object_id: ObjectId,
        source_host_id: HostId,
        target_host_id: HostId,
        signature: &[u8],
    ) -> Result<ObjectMoveHandle, MoveError> {
        //! Initiates the movement of the object identified by `object_id` by submitting a request to
        //! the host behind `source_host_id` (the current owner of the object), instructing them to
        //! submit the delta of the target object (computed through the `signature` argument) to the
        //! host identified by `target_host_id`.
        let mut client = match self.get_client(&source_host_id).await {
            Ok(c) => c,
            Err(e) => return Err(MoveError::TransportError(source_host_id, e.to_string())),
        };

        let request = tonic::Request::new(MoveProperties {
            object_id: object_id.to_string(),
            target_host_id,
            signature: signature.into(),
        });

        let response = match client.move_object(request).await {
            Ok(r) => r,
            Err(e) => {
                eprintln!(
                    "`move_object` for object {} (from host {}) failed: {:?}",
                    object_id, source_host_id, e
                );
                return Err(MoveError::ServiceError(object_id, e.message().to_string()));
            }
        };

        let handle = response.into_inner();
        Ok(handle.try_into().unwrap())
    }

    pub async fn initiate_move(
        &self,
        object_id: ObjectId,
        source_host_id: HostId,
        target_host_id: HostId,
    ) -> Result<ObjectMoveHandle, MoveError> {
        //! Signals the new owner of an object (identified by `target_host_id`) to trigger an
        //! object move from the object's current owner (identified by `source_host_id`).
        let mut client = match self.get_client(&target_host_id).await {
            Ok(c) => c,
            Err(e) => return Err(MoveError::TransportError(source_host_id, e.to_string())),
        };

        let request = tonic::Request::new(TriggerMoveProperties {
            object_id: object_id.to_string(),
            source_host_id: source_host_id.clone(),
        });
        let response = match client.trigger_move(request).await {
            Ok(r) => r,
            Err(e) => {
                eprintln!(
                    "`initiate_move` for object {} (from host {}) failed: {:?}",
                    object_id, source_host_id, e
                );
                return Err(MoveError::ServiceError(object_id, e.message().to_string()));
            }
        };

        let handle = response.into_inner();
        Ok(handle.try_into().unwrap())
    }
}
