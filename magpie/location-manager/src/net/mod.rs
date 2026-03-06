use num_derive::FromPrimitive;

pub mod data_exchange_client;
pub mod data_exchange_server;

pub mod data_request_client;
pub mod data_request_server;

#[derive(Copy, Clone, FromPrimitive)]
pub enum ObjectMoveStatus {
    NotStarted = 0,
    InProgress,
    Completed,
    Denied,
}
