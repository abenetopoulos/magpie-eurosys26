use std::fs::File;
use std::sync::Arc;
#[cfg(feature = "timing-ownership-transfer")]
use std::time::{SystemTime, UNIX_EPOCH};

use fast_rsync::{apply, SignatureOptions};
use object_lib::{files as object_file_lib, ObjectId};
use object_tracker::ObjectTracker;
use ownership_tracker::OwnershipTracker;
use tokio::task::spawn_blocking;
use tonic::{Code, Request, Response, Status};

use crate::config;
use crate::net::data_exchange_client::location_mgr::data_exchange_server::DataExchange as ProtoDataExchange;
use crate::net::data_exchange_client::location_mgr::{
    ApplyDeltaResult, ApplyStatus, ObjectDelta, ObjectDescription, ObjectSignature,
};
use crate::object_move_handle::ObjectMoveHandle;
use crate::rsync_lib;
use crate::util;
use crate::MoveHandleMap;

pub mod location_mgr {
    tonic::include_proto!("dm");
}

pub(crate) struct DataExchangeServer {
    _mod_config: config::Config,
    object_tracker: Arc<ObjectTracker>,
    move_handles: Arc<MoveHandleMap>,
    signature_options: SignatureOptions,
}

impl DataExchangeServer {
    pub fn new(
        mod_config: config::Config,
        object_tracker: Arc<ObjectTracker>,
        move_handles: Arc<MoveHandleMap>,
    ) -> Self {
        let signature_options = SignatureOptions {
            block_size: mod_config.rsync_config.block_size,
            crypto_hash_size: mod_config.rsync_config.hash_size,
        };

        Self {
            _mod_config: mod_config,
            object_tracker,
            move_handles,
            signature_options,
        }
    }

    async fn get_object_bytes(&self, object_id: ObjectId) -> Option<Vec<u8>> {
        let object_tracker = Arc::clone(&self.object_tracker);
        spawn_blocking(move || util::get_object_bytes(object_id, object_tracker))
            .await
            .unwrap()
    }

    async fn get_backing_storage_path(&self, object_id: ObjectId) -> Option<std::path::PathBuf> {
        match self.object_tracker.get_backing_storage_path(object_id) {
            Ok(ob) => Some(ob),
            Err(_) => {
                // NOTE we can pass any positive value as size here and it won't make a
                // difference
                let new_object_file_handle = object_file_lib::open_for_id(object_id, 8).unwrap();
                Some(new_object_file_handle.file_path)
            }
        }
    }

    async fn reload(&self, object_id: ObjectId) -> Result<(), std::io::Error> {
        match self.object_tracker.reload(object_id) {
            Ok(()) => Ok(()),
            Err(_e) => {
                self.object_tracker
                    .open(object_id)
                    .expect("failed to open object");
                self.object_tracker
                    .reset_header(object_id)
                    .expect("failed to reset header");

                Ok(())
            }
        }
    }

    async fn push_initial_version(&self, object_id: ObjectId) -> Result<(), std::io::Error> {
        self.object_tracker.push_initial_version_by_id(object_id);
        Ok(())
    }
}

#[tonic::async_trait]
impl ProtoDataExchange for DataExchangeServer {
    async fn calculate_signature(
        &self,
        request: Request<ObjectDescription>,
    ) -> Result<Response<ObjectSignature>, Status> {
        let object_description = request.get_ref();
        let object_id: ObjectId = object_description.id.parse().unwrap();

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

        Ok(Response::new(ObjectSignature {
            signature: serialized_signature,
        }))
    }

    async fn apply_delta(
        &self,
        request: Request<ObjectDelta>,
    ) -> Result<Response<ApplyDeltaResult>, Status> {
        let object_delta = &request.get_ref();
        let object_id: ObjectId = object_delta.object_id.parse().unwrap();
        let signature = object_delta
            .object_signature
            .as_ref()
            .unwrap()
            .signature
            .clone();

        let object_bytes: Vec<u8> = match self.get_object_bytes(object_id).await {
            Some(ob) => ob,
            None => vec![],
        };

        // apply delta on locally stored object version to bring it up to latest
        // NOTE this is a very unsafe operation because it relies on no one concurrently accessing
        // the target object we're applying the delta to
        let backing_storage_path = self.get_backing_storage_path(object_id).await.unwrap();
        #[cfg(feature = "timing-ownership-transfer")]
        let apply_start = tokio::time::Instant::now();
        #[cfg(feature = "timing-ownership-transfer")]
        let signature_len = signature.len();

        let apply_res = tokio::task::spawn_blocking(move || {
            let mut backing_storage = File::options()
                .read(true)
                .write(true)
                .append(false)
                .open(backing_storage_path)
                .unwrap();

            #[cfg(feature = "timing-ownership-transfer")]
            let apply_inner_start = tokio::time::Instant::now();
            // TODO invoke through rsync_lib
            let r = apply(&object_bytes, &signature, &mut backing_storage);
            #[cfg(feature = "timing-ownership-transfer")]
            {
                let apply_inner_duration = apply_inner_start.elapsed();
                println!(
                    "took {}ms ({}us) to apply delta (inner) of length {signature_len} to object {object_id}",
                    apply_inner_duration.as_millis(),
                    apply_inner_duration.as_micros(),
                );
            }

            r
        })
        .await
        .unwrap();

        #[cfg(feature = "timing-ownership-transfer")]
        {
            let apply_duration = apply_start.elapsed();
            println!(
                "took {}ms ({}us) to apply delta of length {signature_len} to object {object_id}",
                apply_duration.as_millis(),
                apply_duration.as_micros(),
            );
        }

        match apply_res {
            Ok(()) => match self.reload(object_id).await {
                Ok(_) => {
                    {
                        self.push_initial_version(object_id)
                            .await
                            .expect("failed to push version of copy");
                        let ownership_tracker = OwnershipTracker::get_ownership_tracker(None);
                        ownership_tracker.mark_owned(object_id);
                    }

                    #[cfg(feature = "timing-ownership-transfer")]
                    {
                        let done = SystemTime::now();
                        println!(
                            "object {object_id} ready to use at {}",
                            done.duration_since(UNIX_EPOCH)
                                .expect("time moved backwards")
                                .as_millis()
                        );
                    }

                    // mark move as done, so that pending work can start
                    {
                        let move_handle = self.move_handles.get(&object_id).unwrap().clone();
                        move_handle.mark_move_done().await;
                    }

                    return Ok(Response::new(ApplyDeltaResult {
                        status: ApplyStatus::Ok.into(),
                    }));
                }
                Err(e) => {
                    eprintln!("Could not flush updates for object {}: {}", object_id, e);
                    return Err(Status::new(Code::Internal, "Failed to persist updates"));
                }
            },
            Err(e) => {
                eprintln!("Could not apply delta to object {}: {}", object_id, e);
                return Err(Status::new(Code::Internal, "Failed to apply delta"));
            }
        }
    }
}
