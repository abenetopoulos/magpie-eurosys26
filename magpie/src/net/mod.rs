#![allow(dead_code)]
use std::net::{Ipv4Addr, SocketAddrV4};
use std::sync::Arc;

use async_lib::AsyncRuntimeManager;
use axum::Router;
use scheduling::net::rpc::worker_rpc_server::{
    worker_rpc::worker_api_server::WorkerApiServer, WorkerRpcServer,
};
use tokio;
use tonic::transport::Server as TonicServer;

use crate::config::ClientConfig;

mod error;

pub fn spawn_servers(config: &ClientConfig, async_rt_manager: Arc<AsyncRuntimeManager>) -> () {
    for runtime in async_rt_manager.runtime_iter() {
        let server_port = config.worker_rpc_server_port;
        let socket_addr = SocketAddrV4::new(Ipv4Addr::new(0, 0, 0, 0), server_port);

        let socket = tokio::net::TcpSocket::new_v4().unwrap();
        socket.set_reuseaddr(true).expect("failed to set reuseaddr");
        socket.set_reuseport(true).expect("failed to set reuseport");
        socket
            .bind(socket_addr.into())
            .expect("failed to bind to port");

        runtime.spawn(async move {
            let listener = socket.listen(8192).unwrap();
            let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);

            let server = WorkerRpcServer::default();
            let serve = {
                let mut builder = TonicServer::builder();

                builder
                    .add_service(
                        WorkerApiServer::new(server)
                            .max_decoding_message_size(512 * 1024 * 1024)
                            .max_encoding_message_size(512 * 1024 * 1024),
                    )
                    .serve_with_incoming(incoming)
            };

            serve.await.unwrap()
        });
    }
}

// FIXME simplify server component management
pub fn spawn_axum_servers(
    config: &ClientConfig,
    router: Router,
    async_rt_manager: Arc<AsyncRuntimeManager>,
) -> () {
    for runtime in async_rt_manager.runtime_iter() {
        let execution_subsystem_port = config.execution_subsystem_port;
        let socket_addr = SocketAddrV4::new(Ipv4Addr::new(0, 0, 0, 0), execution_subsystem_port);

        let socket = tokio::net::TcpSocket::new_v4().unwrap();
        socket.set_reuseaddr(true).expect("failed to set reuseaddr");
        socket.set_reuseport(true).expect("failed to set reuseport");
        socket
            .bind(socket_addr.into())
            .expect("failed to bind to port");

        let router = router.clone();

        runtime.spawn(async move {
            let listener = socket.listen(8192).unwrap();

            let serve = axum::serve(listener, router.into_make_service());
            serve.await.unwrap();
        });
    }
}
