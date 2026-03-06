#![allow(dead_code)]
#![feature(type_name_of_val, cell_leak)]
use std::sync::Arc;

use object_tracker::ObjectTracker;

pub mod built_ins;
pub mod config;
mod error;
pub mod nando_executor;
pub mod nando_manager;
pub mod nando_scheduler;
mod plans;
pub mod transaction_manager;

pub struct NandoManagerBase {
    #[allow(dead_code)]
    object_tracker: Arc<ObjectTracker>,
}

impl NandoManagerBase {
    pub fn new(object_tracker: Arc<ObjectTracker>) -> Self {
        Self {
            object_tracker: Arc::clone(&object_tracker),
        }
    }
}
