use std::collections::{
    hash_map::{DefaultHasher, HashMap},
    HashSet,
};
use std::hash::{Hash, Hasher};
use std::marker::PhantomData;
use std::mem::size_of as mem_size_of;
use std::sync::Arc;

use nando_support::{
    allocate_and_init, format_intent_name, iptr_of_ref, nando_spawn_polymorphic,
    nando_yield_polymorphic, nando_yield_sink_polymorphic, register_initial, NandoArgument,
};
use nandoize::nandoize_lib;
use object_lib::tls as object_lib_tls;
use object_lib::{IPtr, ObjectId, Persistable};
use object_tracker::{object_tracker_tls, unit_ptr_of};
use ownership_tracker::ownership_tracker_tls;

pub use definitions::*;

pub mod definitions;
pub mod resolver;

#[allow(dead_code)]
pub(crate) const NAMESPACE: &'static str = "kvs";

fn init_bucket_object_of_type<V: Persistable>(
    object_tracker: Arc<object_tracker::ObjectTracker>,
    initial_bucket_capacity: u64,
) -> ObjectId {
    let object = allocate_and_init!(object_tracker, StorageBucket<V>);
    let object_data = object.read_into_mut::<StorageBucket<V>>(None).unwrap();
    object_data
        .inner
        .with_capacity(initial_bucket_capacity.try_into().unwrap());

    let bucket_object_id = object.id;

    object_tracker.push_initial_version(bucket_object_id, (&*object).into());
    object.flush();

    bucket_object_id
}

#[nandoize_lib]
pub fn init_kvs<V: Persistable>(initial_capacity: u64, bucket_initial_capacity: u64) -> IPtr {
    let object_tracker = object_tracker_tls::get_local_object_tracker_instance();
    let initial_capacity: usize = initial_capacity.try_into().unwrap();
    let root_object = allocate_and_init!(object_tracker, RootBlock);
    let root_object_iptr = root_object.iptr_of();
    let root_object_data = root_object.read_into_mut::<RootBlock>(None).unwrap();

    root_object_data
        .bucket_object_ids
        .resize_to_capacity(initial_capacity);

    for _idx in 0..initial_capacity {
        let bucket_object_id =
            init_bucket_object_of_type::<V>(Arc::clone(&object_tracker), bucket_initial_capacity);

        root_object_data.bucket_object_ids.push(bucket_object_id);
        ownership_tracker_tls::mark_object_owned(bucket_object_id);
    }

    let _ = root_object.bump_version();
    object_tracker.push_initial_version(root_object.id, (&*root_object).into());
    ownership_tracker_tls::mark_object_owned(root_object.id);

    root_object_iptr
}

pub fn compute_bucket_object_id(root_block: &RootBlock, key: &String) -> IPtr {
    let key_hash = {
        let mut hasher = DefaultHasher::default();
        key.hash(&mut hasher);
        hasher.finish() as u128
    };

    let bucket_object_id = {
        let mut max_weight = 0;
        let mut max_weight_bucket = 0;
        let mult_constant = 1103515245;

        for bucket_object_id in &root_block.bucket_object_ids {
            let bucket_object_id_digest: u128 = bucket_object_id & ((1u128 << 64) - 1);
            let weight: u128 = (mult_constant
                * ((mult_constant * bucket_object_id_digest + 12345) ^ key_hash)
                + 12345)
                % (2u128.pow(31) - 1);

            #[cfg(debug_assertions)]
            println!("Weight for {key} and {bucket_object_id} is {weight}");

            if max_weight >= weight {
                continue;
            }

            max_weight = weight;
            max_weight_bucket = *bucket_object_id;
        }

        max_weight_bucket
    };

    IPtr::new(bucket_object_id, 0, 0)
}

#[nandoize_lib]
pub fn put<V: Persistable>(root_block: &RootBlock, key: String, value: V)
where
    NandoArgument: From<V>,
{
    let bucket: IPtr = compute_bucket_object_id(root_block, &key);
    nando_spawn_polymorphic!("kvs::put_internal::<V>", bucket, key, value);
}

#[nandoize_lib]
pub fn put_internal<V: Persistable>(bucket: &mut StorageBucket<V>, key: String, value: V) {
    bucket.inner.insert(key, value);
}

#[nandoize_lib]
pub fn get<V: Persistable + Copy + 'static>(
    root_block: &RootBlock,
    key: String,
) -> PhantomData<Option<V>> {
    let bucket: IPtr = compute_bucket_object_id(root_block, &key);
    nando_spawn_polymorphic!("kvs::get_internal::<V>", bucket, key);

    PhantomData
}

#[nandoize_lib]
pub fn get_internal<V: Persistable + Copy + 'static>(
    bucket: &StorageBucket<V>,
    key: String,
) -> Option<V> {
    bucket.inner.get(&key).copied()
}

#[nandoize_lib]
pub fn get_ref<V: Persistable + Copy + 'static>(
    root_block: &RootBlock,
    key: String,
) -> PhantomData<Option<IPtr>> {
    let bucket: IPtr = compute_bucket_object_id(root_block, &key);
    nando_spawn_polymorphic!("kvs::get_ref_internal::<V>", bucket, key);

    PhantomData
}

#[nandoize_lib]
pub fn get_ref_internal<V: Persistable + Copy + 'static>(
    bucket: &StorageBucket<V>,
    key: String,
) -> Option<IPtr> {
    match bucket.inner.get(&key) {
        Some(v) => {
            let ptr = unit_ptr_of!(v);
            let mut iptr = object_lib_tls::iptr_of(ptr).unwrap();
            iptr.size = std::mem::size_of::<V>() as u64;
            Some(iptr)
        }
        None => None,
    }
}

#[nandoize_lib]
pub fn visit_chunks<V: Persistable + Copy + 'static>(root_block: &RootBlock, chunk_size: usize) {
    if root_block.bucket_object_ids.len() % chunk_size != 0 {
        eprintln!(
            "cannot split {} buckets into equisized chunks of {chunk_size}, aborting",
            root_block.bucket_object_ids.len()
        );
        return;
    }

    let mut current_chunk = Vec::with_capacity(chunk_size);
    for bucket_object_id in &root_block.bucket_object_ids {
        let bucket_iptr: IPtr = IPtr::new(*bucket_object_id, 0, 0);
        current_chunk.push(bucket_iptr);

        if current_chunk.len() == chunk_size {
            let chunk = current_chunk.clone();
            nando_spawn_polymorphic!("kvs::visit_chunk::<V>", chunk);
            current_chunk.truncate(0);
        }
    }
}

#[nandoize_lib]
pub fn visit_chunk<V: Persistable + Copy + 'static>(_chunk: Vec<&mut StorageBucket<V>>) {
    // NOTE despite this being a no-op, we want the arguments to be mutable so that this
    // nanotransaction will claim ownership of the target objects, as it's
    // meant to be used to move a group of objects.
}

// Independent per-bucket lookups, bulk aggregation of results at the end.

#[nandoize_lib]
pub fn multi_get<V: Persistable + Copy + 'static>(
    root_block: &RootBlock,
    keys: Vec<String>,
) -> PhantomData<V> {
    let object_tracker = object_tracker_tls::get_local_object_tracker_instance();
    let multi_get_meta = allocate_and_init!(object_tracker, MultiResultMetadata<V>);
    let multi_get_meta_iptr = multi_get_meta.iptr_of();
    let multi_get_meta_data = multi_get_meta
        .read_into_mut::<MultiResultMetadata<V>>(None)
        .unwrap();
    multi_get_meta_data
        .partial_results
        .resize_to_capacity(root_block.bucket_object_ids.len());

    let mut per_bucket_keys = HashMap::with_capacity(root_block.bucket_object_ids.len());
    for key in keys {
        let bucket: IPtr = compute_bucket_object_id(root_block, &key);
        if !per_bucket_keys.contains_key(&bucket) {
            per_bucket_keys.insert(bucket, vec![]);
        }

        let bucket_keys = per_bucket_keys.get_mut(&bucket).unwrap();
        bucket_keys.push(key.clone());
    }

    let mut bucket_tasks = Vec::with_capacity(per_bucket_keys.len());
    for (bucket_iptr, keys) in &per_bucket_keys {
        let partial_result_iptr =
            nando_spawn_polymorphic!("kvs::multi_get_internal::<V>", bucket_iptr, keys);
        let append_task = nando_yield_polymorphic!(
            "kvs::append_partial_ref::<V>",
            multi_get_meta_iptr,
            partial_result_iptr
        );
        bucket_tasks.push(append_task);
    }

    nando_yield_sink_polymorphic!(
        "kvs::spawn_multi_get_merge_partial::<V>",
        &bucket_tasks,
        multi_get_meta_iptr
    );

    register_initial!(multi_get_meta, object_tracker);

    PhantomData
}

#[nandoize_lib]
pub fn append_partial_ref<V: Persistable + Copy + 'static>(
    metadata: &mut MultiResultMetadata<V>,
    partial_result_object_id: ObjectId,
) {
    let partial_result_iptr = IPtr::new(partial_result_object_id, 0, 0);
    metadata.partial_results.push(partial_result_iptr.into());
}

#[nandoize_lib]
pub fn multi_get_internal<V: Persistable + Copy + 'static>(
    bucket: &StorageBucket<V>,
    keys: Vec<String>,
) -> ObjectId {
    let object_tracker = object_tracker_tls::get_local_object_tracker_instance();
    let bucket_results_object = allocate_and_init!(object_tracker, MultiResultAccumulator<V>);
    let bucket_results = bucket_results_object
        .read_into_mut::<MultiResultAccumulator<V>>(None)
        .unwrap();
    bucket_results.results.with_capacity(keys.len());

    for key in &keys {
        match bucket.inner.get(key) {
            Some(v) => {
                bucket_results.results.insert(key.clone(), v.clone());
            }
            // FIXME special value instead of missing key
            None => {}
        }
    }

    register_initial!(bucket_results_object, object_tracker);
    bucket_results_object.iptr_of().get_object_id()
}

#[nandoize_lib]
pub fn spawn_multi_get_merge_partial<V: Persistable + Copy + Clone>(
    metadata: &MultiResultMetadata<V>,
) {
    let partial_results_iptrs: Vec<IPtr> = metadata
        .partial_results
        .iter()
        .map(|b| b.get_inner().clone())
        .collect();

    nando_spawn_polymorphic!("kvs::multi_get_merge_partial::<V>", partial_results_iptrs);
}

#[nandoize_lib]
pub fn multi_get_merge_partial<V: Persistable + Copy + Clone>(
    partial_results: Vec<&MultiResultAccumulator<V>>,
) -> IPtr {
    let object_tracker = object_tracker_tls::get_local_object_tracker_instance();
    let final_result = allocate_and_init!(object_tracker, MultiResultAccumulator<V>);
    let final_result_data = final_result
        .read_into_mut::<MultiResultAccumulator<V>>(None)
        .unwrap();
    let num_results = partial_results
        .iter()
        .fold(0, |acc, pr| acc + pr.results.len());
    final_result_data.results.with_capacity(num_results);

    for partial_result in &partial_results {
        for (key, value) in &partial_result.results {
            // FIXME slow, can we support a faster copy?
            //  - avoid having to do the PString -> String -> PString conversion
            //  - avoid having to do this element-wise
            final_result_data
                .results
                .insert(key.to_string(), value.clone());
        }
    }

    register_initial!(final_result, object_tracker);
    final_result.iptr_of()
}

#[nandoize_lib]
pub fn log_multi_get_results<V: Persistable + std::fmt::Display>(
    result_accumulator: &MultiResultAccumulator<V>,
) {
    for (k, v) in &result_accumulator.results {
        println!("{k}: {v}");
    }
}

// Batch lookups.
#[nandoize_lib]
pub fn init_multi_get_output<V: Persistable + Copy + 'static>(initial_capacity: usize) -> IPtr {
    let object_tracker = object_tracker_tls::get_local_object_tracker_instance();
    let result_object = allocate_and_init!(object_tracker, MultiResultAccumulator<LookupEntry<V>>);
    result_object.set_mv_enabled(false);
    let result_object_data = result_object
        .read_into_mut::<MultiResultAccumulator<LookupEntry<V>>>(None)
        .unwrap();
    result_object_data.results.with_capacity(initial_capacity);

    register_initial!(result_object, object_tracker);

    result_object.iptr_of()
}

#[nandoize_lib]
pub fn multi_get_batch<V: Persistable + Copy + 'static>(
    root_block: &RootBlock,
    result_object: &mut MultiResultAccumulator<LookupEntry<V>>,
    keys: Vec<String>,
) {
    result_object.results.clear();

    let mut bucket_iptrs = HashSet::new();
    for key in &keys {
        let bucket: IPtr = compute_bucket_object_id(root_block, key);
        bucket_iptrs.insert(bucket);
        result_object
            .results
            .insert(key.clone(), LookupEntry::LookupIn(bucket.get_object_id()));
    }

    let result_object_iptr = iptr_of_ref!(result_object);
    let bucket_iptrs: Vec<IPtr> = bucket_iptrs.into_iter().collect();
    nando_spawn_polymorphic!(
        "kvs::multi_get_batch_internal::<V>",
        result_object_iptr,
        bucket_iptrs,
        keys
    );
}

#[nandoize_lib]
pub fn multi_get_batch_internal<V: Persistable + Copy + Clone + 'static>(
    result_object: &mut MultiResultAccumulator<LookupEntry<V>>,
    buckets: Vec<&StorageBucket<V>>,
    keys: Vec<String>,
) {
    let mut bucket_map = HashMap::with_capacity(buckets.len());
    for bucket in buckets.iter() {
        let bucket_iptr = iptr_of_ref!(*bucket);
        bucket_map.insert(bucket_iptr.get_object_id(), bucket);
    }

    for key in &keys {
        let Some(LookupEntry::LookupIn(bucket_object_id)) = result_object.results.get(key) else {
            continue;
        };

        let bucket_object = bucket_map.get(&bucket_object_id).unwrap();
        match bucket_object.inner.get(key) {
            None => result_object
                .results
                .insert(key.clone(), LookupEntry::NotFound),
            Some(v) => result_object
                .results
                .insert(key.clone(), LookupEntry::Found(v.clone())),
        };
    }
}

#[nandoize_lib]
pub fn log_multi_get_batch_results<V: Persistable + std::fmt::Display>(
    result_accumulator: &MultiResultAccumulator<LookupEntry<V>>,
) {
    for (k, v) in &result_accumulator.results {
        match v {
            LookupEntry::Found(v) => println!("{k}: {v}"),
            LookupEntry::NotFound => println!("{k}: not found"),
            _ => println!("{k}: never looked up"),
        }
    }
}
