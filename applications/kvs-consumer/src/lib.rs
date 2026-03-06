use kvs::{self, definitions::*};
// NOTE the allow() here is used to silence warnings about unused macro imports that are a result
// of the nandoize macro short-circuiting the below calls to nando_spawn_polymorphic.
#[allow(unused_imports)]
use nando_support::{iptr::IPtr, nando_spawn, nando_spawn_polymorphic};
use nandoize::nandoize_lib;

pub mod resolver;
pub(crate) const NAMESPACE: &'static str = "kvs_consumer";

#[nandoize_lib]
pub fn init_kvs_consumer(initial_capacity: u64, initial_bucket_capacity: u64) -> IPtr {
    kvs::init_kvs::<i32>(initial_capacity, initial_bucket_capacity)
}

#[nandoize_lib]
pub fn get_i32(root_block: &RootBlock, key: String) {
    nando_spawn_polymorphic!("kvs::get::<i32>", root_block, key);
}

#[nandoize_lib]
pub fn multi_get_i32(root_block: &RootBlock, keys: Vec<String>) {
    nando_spawn_polymorphic!("kvs::multi_get::<i32>", root_block, keys);
}

#[nandoize_lib]
pub fn log_multi_get_results_i32(acc: &MultiResultAccumulator<i32>) {
    nando_spawn_polymorphic!("kvs::log_multi_get_results::<i32>", acc);
}

#[nandoize_lib]
pub fn init_multi_get_batch_i32(initial_capacity: usize) -> IPtr {
    kvs::init_multi_get_output::<i32>(initial_capacity)
}

#[nandoize_lib]
pub fn multi_get_batch_i32(
    root_block: &RootBlock,
    acc: &mut MultiResultAccumulator<LookupEntry<i32>>,
    keys: Vec<String>,
) {
    nando_spawn_polymorphic!("kvs::multi_get_batch::<i32>", root_block, acc, keys);
}

#[nandoize_lib]
pub fn log_multi_get_batch_results_i32(acc: &MultiResultAccumulator<LookupEntry<i32>>) {
    nando_spawn_polymorphic!("kvs::log_multi_get_batch_results::<i32>", acc);
}

#[nandoize_lib]
pub fn put_i32(root_block: &RootBlock, key: String, value: i32) {
    nando_spawn_polymorphic!("kvs::put::<i32>", root_block, key, value);
}

#[nandoize_lib]
pub fn visit_chunks_i32(root_block: &RootBlock, chunk_size: usize) {
    nando_spawn_polymorphic!("kvs::visit_chunks::<i32>", root_block, chunk_size);
}

#[nandoize_lib]
pub fn get_i32_internal(bucket: &mut StorageBucket<i32>, key: String) -> Option<i32> {
    kvs::get_internal::<i32>(bucket, key)
}

#[nandoize_lib]
pub fn put_i32_internal(bucket: &mut StorageBucket<i32>, key: String, value: i32) {
    kvs::put_internal::<i32>(bucket, key, value);
}

#[nandoize_lib]
pub fn get_and_increment_i32(root_block: &RootBlock, key: String) {
    let cloned_key = key.clone();
    let bucket: IPtr = kvs::compute_bucket_object_id(root_block, &cloned_key);
    nando_spawn!("kvs_consumer::set_or_increment_i32", bucket, key);
}

#[nandoize_lib]
pub fn set_or_increment_i32(bucket: &mut StorageBucket<i32>, key: String) {
    // NOTE ideally we would simply retrieve a mutable reference and modify the entry through that
    // in order to avoid repeated hashmap probes. However, our current effect extraction mechanism
    // in nandoize doesn't support generating IPtrs from "random" elements retrieved from
    // within a collection, so for now we will rely on the internal tracking performed by the
    // PHashMap.

    let existing_value = bucket.inner.get(&key);
    if existing_value.is_none() {
        bucket.inner.insert(key, 1);
        return;
    };

    let existing_value = existing_value.unwrap();
    if *existing_value < 100000 {
        bucket.inner.insert(key, existing_value * 2);
        return;
    }

    bucket.inner.insert(key, existing_value + 1);
}
