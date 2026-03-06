//! Client for supporting object moves (signature operations etc.)
use std::collections::HashMap;

use async_std::sync::Mutex;
use location_mgr::data_exchange_client;
use location_mgr::{ObjectDelta, ObjectDescription, ObjectSignature};
use object_lib::ObjectId;
use tonic::transport::channel::Channel;

use crate::error::{InternalError, MoveError};
use crate::HostId;

pub mod location_mgr {
    tonic::include_proto!("dm");
}

type ConcreteDataExchangeClient = data_exchange_client::DataExchangeClient<Channel>;

pub struct DataExchangeClient {
    server_port: u16,
    host_clients: Mutex<HashMap<HostId, ConcreteDataExchangeClient>>,
}

impl DataExchangeClient {
    pub fn new(server_port: u16) -> Self {
        Self {
            server_port,
            host_clients: Mutex::new(HashMap::new()),
        }
    }

    async fn get_client(
        &self,
        host_id: &HostId,
    ) -> Result<ConcreteDataExchangeClient, InternalError> {
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
        let client = match data_exchange_client::DataExchangeClient::connect(server_addr).await {
            Ok(c) => {
                let c = c
                    .max_decoding_message_size(512 * 1024 * 1024)
                    .max_encoding_message_size(512 * 1024 * 1024);
                /*
                .send_compressed(CompressionEncoding::Gzip)
                .accept_compressed(CompressionEncoding::Gzip);
                */

                let mut host_clients = self.host_clients.lock().await;
                host_clients.insert(key.clone(), c);
                host_clients.get(&key).unwrap().clone()
            }
            Err(e) => {
                eprintln!(
                    "[DataExchangeClient] Failed to establish connection with host {}: {}",
                    host_id, e
                );
                return Err(InternalError::UnknownError());
            }
        };

        Ok(client)
    }

    #[allow(dead_code)]
    pub async fn calculate_signature(
        &self,
        object_id: ObjectId,
        host_id: HostId,
    ) -> Result<Vec<u8>, MoveError> {
        // FIXME error type
        let mut client = match self.get_client(&host_id).await {
            Ok(c) => c,
            Err(e) => return Err(MoveError::TransportError(host_id, e.to_string())),
        };

        let request = tonic::Request::new(ObjectDescription {
            id: object_id.to_string(),
        });

        let response = match client.calculate_signature(request).await {
            Ok(r) => r,
            Err(e) => {
                eprintln!(
                    "`calculate_signature` for object {} (from host {}) failed: {:?}",
                    object_id, host_id, e,
                );
                return Err(MoveError::ServiceError(object_id, e.message().to_string()));
            }
        };
        let signature = &response.get_ref().signature;

        Ok(signature.to_vec())
    }

    pub async fn apply_delta(
        &self,
        delta: &[u8],
        object_id: ObjectId,
        target_host_id: HostId,
    ) -> Result<(), MoveError> {
        // FIXME error type
        let mut client = match self.get_client(&target_host_id).await {
            Ok(c) => c,
            Err(e) => return Err(MoveError::TransportError(target_host_id, e.to_string())),
        };

        let request = tonic::Request::new(ObjectDelta {
            object_id: object_id.to_string(),
            object_signature: Some(ObjectSignature {
                signature: delta.to_vec(),
            }),
        });

        let _response = match client.apply_delta(request).await {
            Ok(r) => r,
            Err(e) => {
                eprintln!(
                    "`apply_delta` for object {} (from host {}) failed: {:?}",
                    object_id, target_host_id, e,
                );
                return Err(MoveError::ServiceError(object_id, e.message().to_string()));
            }
        };

        Ok(())
    }
}
