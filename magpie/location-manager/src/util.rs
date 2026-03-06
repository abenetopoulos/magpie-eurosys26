use std::sync::Arc;

use async_lib::AsyncRuntimeManager;
use fast_rsync::Signature;
#[cfg(feature = "observability")]
use lazy_static::lazy_static;
use object_lib::ObjectId;
use object_tracker::ObjectTracker;
#[cfg(feature = "observability")]
use prometheus::{register_histogram_vec, HistogramVec};

use crate::net::data_exchange_client::DataExchangeClient;
use crate::object_move_handle::{ObjectMoveHandle, ObjectMoveStatus};
use crate::rsync_lib;
use crate::MoveHandleMap;

#[cfg(feature = "observability")]
lazy_static! {
    static ref OWNERSHIP_SIGNATURE_DIFF_HISTOGRAM: HistogramVec = register_histogram_vec!(
        "location_manager_ownership_signature_diff_milliseconds",
        "Location Manager signature diff latencies in milliseconds",
        &[],
        vec![0.1, 1.0, 10.0, 100.0],
    )
    .unwrap();
}

pub fn get_object_bytes(
    object_id: ObjectId,
    object_tracker: Arc<ObjectTracker>,
) -> Option<Vec<u8>> {
    match { object_tracker.get_as_bytes(object_id) } {
        Ok(ob) => Some(ob.to_vec()),
        Err(_) => None,
    }
}

pub async fn trigger_object_move(
    object_id: ObjectId,
    move_handles: Arc<MoveHandleMap>,
    target_host_id: String,
    foreign_signature: &Signature,
    object_bytes: &[u8],
    data_exchange_client: Arc<DataExchangeClient>,
    async_rt_manager: Arc<AsyncRuntimeManager>,
) -> () {
    {
        let move_handle = match move_handles.get(&object_id) {
            Some(mh) => Some(mh.clone()),
            None => {
                let move_handle =
                    ObjectMoveHandle::new(object_id, Some(ObjectMoveStatus::InProgress));
                move_handles.insert(object_id, move_handle);
                None
            }
        };

        if let Some(move_handle) = move_handle {
            move_handle.set_status_in_progress().await;
        }
    }

    #[cfg(any(feature = "observability", feature = "timing-ownership-transfer"))]
    let signature_diff_start = tokio::time::Instant::now();
    let out = match foreign_signature.serialized().is_empty() {
        true => object_bytes.to_vec(),
        false => {
            let mut out = vec![];
            match rsync_lib::calculate_diff(&foreign_signature, &object_bytes, &mut out) {
                Ok(()) => out,
                Err(_) => {
                    eprintln!("Could not calculate diff for object {object_id}");
                    // FIXME
                    panic!("Could not calculate diff for object {object_id}");
                }
            }
        }
    };

    #[cfg(any(feature = "observability", feature = "timing-ownership-transfer"))]
    let signature_diff_duration = signature_diff_start.elapsed();
    #[cfg(feature = "observability")]
    {
        OWNERSHIP_SIGNATURE_DIFF_HISTOGRAM
            .with_label_values(&[])
            .observe(signature_diff_duration.as_micros() as f64 / 1000.0);
    }

    #[cfg(feature = "timing-ownership-transfer")]
    {
        println!(
            "took {}ms ({}us) to compute signature diff for {object_id}",
            signature_diff_duration.as_millis(),
            signature_diff_duration.as_micros(),
        );
    }

    // asynchronously invoke `apply` on target host
    let move_handles = Arc::clone(&move_handles);
    async_rt_manager.spawn(async move {
        let result = data_exchange_client
            .apply_delta(&out, object_id, target_host_id.to_string())
            .await;

        let move_handle = move_handles.get(&object_id).unwrap().clone();
        match result {
            Ok(_) => {
                move_handle.mark_move_done().await;
            }
            Err(e) => {
                eprintln!(
                    "Failed to apply delta on remote host {} for object {}: {}",
                    target_host_id, object_id, e
                );

                move_handle.mark_move_denied().await;
            }
        }
    });
}
