use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddrV4};
use std::sync::Arc;

use async_lib::AsyncRuntimeManager;
use async_std::sync::Arc as AsyncArc;
use config::Config;
use dashmap::DashMap;
use error::InternalError;
use fast_rsync::SignatureOptions;
#[cfg(feature = "observability")]
use lazy_static::lazy_static;
use net::data_exchange_client::location_mgr::data_exchange_server::DataExchangeServer as ProtoDataExchangeServer;
use net::data_exchange_client::DataExchangeClient;
use net::data_exchange_server::DataExchangeServer;
use net::data_request_client::location_mgr::data_request_server::DataRequestServer as ProtoDataRequestServer;
use net::data_request_client::DataRequestClient;
use net::data_request_server::DataRequestServer;
use object_lib::ObjectId;
use object_move_handle::ObjectMoveHandle;
use object_tracker::ObjectTracker;
#[cfg(feature = "observability")]
use prometheus::{register_histogram_vec, HistogramVec};
use tokio;
use tonic::transport::Server;

pub mod config;
mod error;
mod net;
mod object_move_handle;
mod rsync_lib;
mod util;

#[cfg(feature = "observability")]
lazy_static! {
    static ref OWNERSHIP_SIGNATURE_DESERIALIZATION_HISTOGRAM: HistogramVec =
        register_histogram_vec!(
            "location_manager_ownership_signature_deserialization_milliseconds",
            "Location Manager signature deserialization latencies in milliseconds",
            &[],
            vec![0.1, 1.0, 10.0, 100.0],
        )
        .unwrap();
}

pub type HostId = String;
pub type MoveHandleMap = DashMap<ObjectId, ObjectMoveHandle>;

pub struct LocationManager {
    // FIXME this should be replaced with a concurrent hashmap (like flurry, dashmap, etc.), or at
    // the very least protected by some kind of mutex.
    pub location_map: HashMap<ObjectId, HostId>,
    pub move_handles: Arc<MoveHandleMap>,

    async_rt_manager: Arc<AsyncRuntimeManager>,
    config: Config,
    object_tracker: Arc<ObjectTracker>,

    data_request_client: AsyncArc<DataRequestClient>,
    data_exchange_client: AsyncArc<DataExchangeClient>,
}

impl LocationManager {
    pub fn new(
        config: Config,
        async_rt_manager: Arc<AsyncRuntimeManager>,
        object_tracker: Arc<ObjectTracker>,
    ) -> Self {
        Self {
            config: config.clone(),
            location_map: HashMap::new(),
            async_rt_manager,
            object_tracker,
            move_handles: Arc::new(DashMap::new()),
            data_request_client: AsyncArc::new(DataRequestClient::new(config.data_server_port)),
            data_exchange_client: AsyncArc::new(DataExchangeClient::new(config.data_server_port)),
        }
    }

    pub fn get_current_host_id(&self) -> HostId {
        self.config.client_id.to_string()
    }

    pub async fn init_server(&self) -> Result<(), InternalError> {
        for runtime in self.async_rt_manager.runtime_iter() {
            let server_port = self.config.data_server_port;
            let socket_addr = SocketAddrV4::new(Ipv4Addr::new(0, 0, 0, 0), server_port);

            let socket = tokio::net::TcpSocket::new_v4().unwrap();
            socket.set_reuseaddr(true).expect("failed to set reuseaddr");
            socket.set_reuseport(true).expect("failed to set reuseport");
            socket
                .bind(socket_addr.into())
                .expect("failed to bind to port");

            let data_request_server = DataRequestServer::new(
                self.config.clone(),
                Arc::clone(&self.object_tracker),
                Arc::clone(&self.move_handles),
                AsyncArc::clone(&self.data_request_client),
                AsyncArc::clone(&self.data_exchange_client),
                &self.config.rsync_config,
                Arc::clone(&self.async_rt_manager),
            );
            let data_exchange_server = DataExchangeServer::new(
                self.config.clone(),
                Arc::clone(&self.object_tracker),
                Arc::clone(&self.move_handles),
            );

            runtime.spawn(async move {
                let listener = socket.listen(8192).unwrap();
                let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);

                let serve = Server::builder()
                    .tcp_nodelay(true)
                    .add_service(ProtoDataRequestServer::new(data_request_server).max_decoding_message_size(512 * 1024 * 1024).max_encoding_message_size(512 * 1024 * 1024)/*.accept_compressed(CompressionEncoding::Gzip).send_compressed(CompressionEncoding::Gzip)*/)
                    .add_service(ProtoDataExchangeServer::new(data_exchange_server).max_decoding_message_size(512 * 1024 * 1024).max_encoding_message_size(512 * 1024 * 1024)/*.accept_compressed(CompressionEncoding::Gzip).send_compressed(CompressionEncoding::Gzip)*/)
                    .serve_with_incoming(incoming);

                serve.await.unwrap()
            });
        }

        Ok(())
    }

    pub async fn move_object_to(
        &self,
        object_id: ObjectId,
        target_host: HostId,
    ) -> Result<(), anyhow::Error> {
        let source_host_id = self.config.client_id;
        let source_host = self.config.data_server_hosts[source_host_id as usize].clone();
        if source_host == target_host {
            return Err(anyhow::Error::msg(
                "Cannot be the target of requested object move",
            ));
        }

        self.data_request_client
            .initiate_move(object_id, source_host, target_host)
            .await
            .expect("failed to initiate move");

        return Ok(());
    }

    pub async fn move_object(
        &self,
        object_id: ObjectId,
        source_host_id: u16,
        target_host_id: u16,
    ) -> Result<(), anyhow::Error> {
        let client_id = self.config.client_id;
        let source_host = self.config.data_server_hosts[source_host_id as usize].clone();
        let target_host = self.config.data_server_hosts[target_host_id as usize].clone();

        // If the current host is not the recipient in this data movement, we only need to notify
        // the remote recipient to trigger the data movement.
        if client_id == source_host_id || client_id != target_host_id {
            self.data_request_client
                .initiate_move(object_id, source_host, target_host)
                .await
                .expect("failed to initiate move");

            return Ok(());
        }

        // This host is the recipient, so first we need to do some bookkeeping.
        self.move_handles
            .insert(object_id, ObjectMoveHandle::new(object_id, None));

        let signature = {
            let rsync_config = self.config.rsync_config.clone();
            let object_tracker = Arc::clone(&self.object_tracker);
            rsync_lib::calculate_signature(
                object_id,
                object_tracker,
                rsync_lib::SignatureCalculationConfig::SignatureCalculationConfig(&rsync_config),
            )
        };

        let data_request_client = AsyncArc::clone(&self.data_request_client);
        self.async_rt_manager.spawn(async move {
            data_request_client
                .move_object(object_id, source_host, target_host, &signature)
                .await
                .expect(&format!("failed to move object {}", object_id));
        });

        Ok(())
    }

    pub async fn wait_for_object(&self, object_id: ObjectId) {
        match self.object_tracker.get(object_id).is_none() {
            true => {
                let move_handle = self
                    .move_handles
                    .get(&object_id)
                    .expect(&format!("object {} is not in flight", object_id))
                    .clone();
                move_handle.wait_until_done().await;
            }
            false => (),
        }
    }

    /// Unconditionally waits for an in-flight object without first checking for residency.
    ///
    /// It is up to the caller of this function to ensure that the right wait semantics are
    /// enforced.
    pub async fn wait_for_object_or_update(&self, object_id: ObjectId) -> Option<()> {
        let move_handle = match self.move_handles.get(&object_id) {
            None => return None,
            Some(h) => h.clone(),
        };
        move_handle.wait_until_done().await;

        Some(())
    }

    pub fn get_object_tracker(&self) -> Arc<ObjectTracker> {
        self.object_tracker.clone()
    }

    pub async fn calculate_signature(&self, object_id: ObjectId) -> Vec<u8> {
        let rsync_config = self.config.rsync_config.clone();
        let object_tracker = Arc::clone(&self.object_tracker);
        rsync_lib::calculate_signature(
            object_id,
            object_tracker,
            rsync_lib::SignatureCalculationConfig::SignatureCalculationConfig(&rsync_config),
        )
    }

    pub async fn insert_move_handle(&self, object_id: ObjectId) {
        self.move_handles
            .insert(object_id, ObjectMoveHandle::new(object_id, None));
    }

    pub async fn trigger_move(
        &self,
        object_id: ObjectId,
        foreign_signature: &Vec<u8>,
        target_host_id: String,
    ) {
        #[cfg(feature = "timing-ownership-transfer")]
        let get_bytes_start = tokio::time::Instant::now();
        let object_bytes: Vec<u8> =
            match util::get_object_bytes(object_id, Arc::clone(&self.object_tracker)) {
                Some(ob) => ob,
                None => vec![],
            };
        #[cfg(feature = "timing-ownership-transfer")]
        let get_bytes_duration = get_bytes_start.elapsed();

        #[cfg(any(feature = "observability", feature = "timing-ownership-transfer"))]
        let signature_deserialization_start = tokio::time::Instant::now();
        let foreign_signature = rsync_lib::deserialize_signature(
            foreign_signature,
            rsync_lib::SignatureCalculationConfig::SignatureCalculationOptions(SignatureOptions {
                block_size: self.config.rsync_config.block_size,
                crypto_hash_size: self.config.rsync_config.hash_size,
            }),
        )
        .unwrap();
        #[cfg(feature = "observability")]
        {
            let signature_deserialization_duration = signature_deserialization_start.elapsed();
            OWNERSHIP_SIGNATURE_DESERIALIZATION_HISTOGRAM
                .with_label_values(&[])
                .observe(signature_deserialization_duration.as_micros() as f64 / 1000.0);
        }

        #[cfg(feature = "timing-ownership-transfer")]
        {
            let signature_deserialization_duration = signature_deserialization_start.elapsed();
            println!(
                "took {}ms ({}us) to get object {object_id} signature, {}ms ({}us) to deserialize foreign signature",
                get_bytes_duration.as_millis(),
                get_bytes_duration.as_micros(),
                signature_deserialization_duration.as_millis(),
                signature_deserialization_duration.as_micros(),
            );
        }

        util::trigger_object_move(
            object_id,
            Arc::clone(&self.move_handles),
            target_host_id,
            &foreign_signature,
            &object_bytes,
            AsyncArc::clone(&self.data_exchange_client),
            Arc::clone(&self.async_rt_manager),
        )
        .await;
    }
}
