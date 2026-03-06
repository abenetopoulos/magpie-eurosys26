#![feature(type_name_of_val)]
#![allow(static_mut_refs)]
pub mod activation_id;
pub mod activation_intent;
pub mod ecb_id;
pub mod epic_control;
pub mod epic_definitions;
pub mod ext_macros;
pub mod iptr;
pub mod log_entry;
pub mod nando_metadata;
pub mod utils;

pub type TxnId = u128;

pub type ObjectId = u128;
// FIXME @cache revert back to u128
pub type ObjectVersion = u64;

pub type ArgumentIdx = usize;

pub const SYSTEM_ADDR_SPACE_WIDTH: u32 = 32;

pub use activation_intent::{HostIdx, NandoActivationIntent, NandoArgument};
pub use ext_macros::strip_trailing_generics;
pub use log_entry::ImageValue;
