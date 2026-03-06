use std::sync::Arc;

use fast_rsync::{diff, Signature, SignatureOptions};
use object_lib::ObjectId;
use object_tracker::ObjectTracker;

use crate::config::RSyncConfig;
use crate::util;

pub enum SignatureCalculationConfig<'a> {
    SignatureCalculationOptions(SignatureOptions),
    SignatureCalculationConfig(&'a RSyncConfig),
}

impl Into<SignatureOptions> for SignatureCalculationConfig<'_> {
    fn into(self) -> SignatureOptions {
        match self {
            SignatureCalculationConfig::SignatureCalculationOptions(so) => so,
            SignatureCalculationConfig::SignatureCalculationConfig(c) => SignatureOptions {
                block_size: c.block_size,
                crypto_hash_size: c.hash_size,
            },
        }
    }
}

pub fn calculate_signature(
    object_id: ObjectId,
    object_tracker: Arc<ObjectTracker>,
    signature_calculation_config: SignatureCalculationConfig<'_>,
) -> Vec<u8> {
    let object_bytes: Vec<u8> = match util::get_object_bytes(object_id, object_tracker) {
        Some(ob) => ob,
        None => vec![],
    };

    let signature_options = signature_calculation_config.into();
    Signature::calculate(&object_bytes, signature_options).into_serialized()
}

pub fn get_empty_signature(
    signature_calculation_config: SignatureCalculationConfig<'_>,
) -> Signature {
    let signature_options = signature_calculation_config.into();
    Signature::calculate(&vec![], signature_options)
}

pub fn deserialize_signature<'a>(
    serialized_signature: &'a [u8],
    signature_calculation_config: SignatureCalculationConfig<'_>,
) -> Result<Signature, ()> {
    // TODO error type
    if serialized_signature.len() == 0 {
        // NOTE: `fast_rsync` currently has problems deserializing an empty signature, so
        // we have to create an empty signature object here.
        return Ok(get_empty_signature(signature_calculation_config));
    }

    match Signature::deserialize(serialized_signature.to_vec()) {
        Ok(s) => Ok(s),
        Err(e) => {
            eprintln!("Could not deserialize signature: {}", e);
            return Err(());
        }
    }
}

pub fn calculate_diff(
    foreign_signature: &Signature,
    object_bytes: &[u8],
    out: &mut Vec<u8>,
) -> Result<(), ()> {
    match diff(&foreign_signature.index(), &object_bytes, out) {
        Ok(()) => Ok(()),
        Err(e) => {
            eprintln!("Could not calculate signature diff: {}", e);
            return Err(());
        }
    }
}
