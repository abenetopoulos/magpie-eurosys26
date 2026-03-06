use std::sync::Arc;

use async_lib::AsyncRuntimeManager;
use async_std::sync::Arc as AsyncArc;
use fast_rsync::SignatureOptions;
use object_lib::ObjectId;
use object_tracker::ObjectTracker;
use tokio::task::spawn_blocking;
use tonic::{Code, Request, Response, Status};

use crate::config;
use crate::net::data_exchange_client::DataExchangeClient;
use crate::net::data_request_client::location_mgr::data_request_server::DataRequest as ProtoDataRequest;
use crate::net::data_request_client::location_mgr::{
    MoveProperties, ObjectMoveHandle as ProtoObjectMoveHandle, TriggerMoveProperties,
};
use crate::net::data_request_client::DataRequestClient;
use crate::object_move_handle::{ObjectMoveHandle, ObjectMoveStatus};
use crate::rsync_lib;
use crate::util;
use crate::MoveHandleMap;

pub mod location_mgr {
    tonic::include_proto!("dm");
}

pub struct DataRequestServer {
    // FIXME @hack this is only used to retrieve the current host's id when triggering an object
    // move.
    mod_config: config::Config,
    object_tracker: Arc<ObjectTracker>,
    move_handles: Arc<MoveHandleMap>,
    data_request_client: AsyncArc<DataRequestClient>,
    data_exchange_client: AsyncArc<DataExchangeClient>,
    signature_options: SignatureOptions,
    async_rt_manager: Arc<AsyncRuntimeManager>,
}

impl DataRequestServer {
    pub fn new(
        mod_config: config::Config,
        object_tracker: Arc<ObjectTracker>,
        move_handles: Arc<MoveHandleMap>,
        data_request_client: AsyncArc<DataRequestClient>,
        data_exchange_client: AsyncArc<DataExchangeClient>,
        config: &config::RSyncConfig,
        async_rt_manager: Arc<AsyncRuntimeManager>,
    ) -> Self {
        Self {
            mod_config,
            object_tracker,
            move_handles,
            data_request_client,
            data_exchange_client,
            signature_options: SignatureOptions {
                block_size: config.block_size,
                crypto_hash_size: config.hash_size,
            },
            async_rt_manager,
        }
    }

    async fn get_object_bytes(&self, object_id: ObjectId) -> Option<Vec<u8>> {
        let object_tracker = Arc::clone(&self.object_tracker);
        spawn_blocking(move || util::get_object_bytes(object_id, object_tracker))
            .await
            .unwrap()
    }

    async fn get_move_status(&self, object_id: ObjectId) -> Option<ObjectMoveStatus> {
        let move_handle = match self.move_handles.get(&object_id) {
            Some(mh) => mh.clone(),
            None => return None,
        };

        let move_status = move_handle.move_status.read().await;
        Some(*move_status)
    }
}

#[tonic::async_trait]
impl ProtoDataRequest for DataRequestServer {
    async fn get_move_status(
        &self,
        request: Request<ProtoObjectMoveHandle>,
    ) -> Result<Response<ProtoObjectMoveHandle>, Status> {
        let handle = request.get_ref();
        let object_id = handle.id.parse().unwrap();

        let move_status = match self.get_move_status(object_id).await {
            Some(s) => s,
            None => {
                return Err(Status::new(
                    Code::NotFound,
                    "could not find status for object",
                ))
            }
        };

        Ok(Response::new(ProtoObjectMoveHandle {
            id: object_id.to_string(),
            move_status: move_status as i32,
        }))
    }

    async fn move_object(
        &self,
        request: Request<MoveProperties>,
    ) -> Result<Response<ProtoObjectMoveHandle>, Status> {
        let move_properties = request.get_ref();

        let object_id = move_properties.object_id.parse().unwrap();
        let object_bytes: Vec<u8> = match self.get_object_bytes(object_id).await {
            Some(ob) => ob,
            None => vec![],
        };

        let serialized_foreign_signature = &move_properties.signature;
        let foreign_signature = rsync_lib::deserialize_signature(
            serialized_foreign_signature,
            rsync_lib::SignatureCalculationConfig::SignatureCalculationOptions(
                self.signature_options,
            ),
        )
        .unwrap();

        util::trigger_object_move(
            object_id,
            Arc::clone(&self.move_handles),
            move_properties.target_host_id.to_string(),
            &foreign_signature,
            &object_bytes,
            AsyncArc::clone(&self.data_exchange_client),
            Arc::clone(&self.async_rt_manager),
        )
        .await;

        Ok(Response::new(ProtoObjectMoveHandle {
            id: object_id.to_string(),
            move_status: ObjectMoveStatus::InProgress as i32,
        }))
    }

    async fn trigger_move(
        &self,
        request: Request<TriggerMoveProperties>,
    ) -> Result<Response<ProtoObjectMoveHandle>, Status> {
        let move_properties = request.get_ref();

        let object_id = move_properties.object_id.parse().unwrap();

        {
            let serialized_signature = {
                let signature_options = self.signature_options.clone();
                let object_tracker = Arc::clone(&self.object_tracker);
                spawn_blocking(move || {
                    rsync_lib::calculate_signature(
                        object_id,
                        object_tracker,
                        rsync_lib::SignatureCalculationConfig::SignatureCalculationOptions(
                            signature_options,
                        ),
                    )
                })
                .await
                .unwrap()
            };

            self.move_handles
                .insert(object_id, ObjectMoveHandle::new(object_id, None));

            let source_host = move_properties.source_host_id.to_string();
            let target_host =
                self.mod_config.data_server_hosts[self.mod_config.client_id as usize].clone();
            let data_request_client = AsyncArc::clone(&self.data_request_client);

            tokio::spawn(async move {
                data_request_client
                    .move_object(object_id, source_host, target_host, &serialized_signature)
                    .await
                    .expect("failed to move object");
            });
        }

        Ok(Response::new(ProtoObjectMoveHandle {
            id: object_id.to_string(),
            move_status: ObjectMoveStatus::InProgress as i32,
        }))
    }
}
