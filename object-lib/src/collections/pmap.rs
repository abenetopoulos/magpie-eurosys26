//! A hash map that can be safely persisted within objects.
//!
//! This is a prototype implementation of a safely movable hash map type, implementing a (small)
//! subset of the [`std::collections::HashMap`] hash map type. There has been no effort to optimize
//! any aspect of this implementation, so predictable performance should not be expected.
use std::alloc::{Allocator, Layout};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::iter::Chain;
use std::marker::PhantomData;
use std::mem::MaybeUninit;
use std::ptr::NonNull;
#[cfg(feature = "simd")]
use std::simd;
use std::sync::Arc;

use parking_lot::RwLock;

use crate::allocators::bump_allocator::BumpAllocator;
use crate::allocators::persistently_allocatable::{CopyFrom, PersistentlyAllocatable};
use crate::collections::pvec;
use crate::tls as nando_tls;
use crate::Persistable;

macro_rules! get_bucket_for_iter {
    ($target:expr, $idx:expr) => {{
        std::ptr::addr_of!($target.table_offset)
            .byte_offset(
                ($target.table_offset + $idx * std::mem::size_of::<Bucket<K, V>>())
                    .try_into()
                    .unwrap(),
            )
            .cast_mut()
            .cast::<Bucket<K, V>>()
    }};
}

#[repr(C)]
#[doc(hidden)]
pub struct BucketEntry<K, V>
where
    K: Persistable,
    V: Persistable,
{
    key: K,
    value: V,
}

impl<K, V> Persistable for BucketEntry<K, V>
where
    K: Persistable,
    V: Persistable,
{
}

/// The initial capacity to set for newly allocated vectors backing Bucket instances.
///
/// This will be parameterizable in the future.
static BUCKET_AVERAGE_LENGTH: u8 = 20;

#[repr(C)]
#[doc(hidden)]
struct Bucket<K: Persistable, V: Persistable> {
    empty_slots: pvec::PVec<usize>,

    // NOTE the reason why the vector containing the bucket's content is not owned by the Bucket
    // is that, on resize, we want to avoid moving the contents to the new bucket that these keys
    // will hash to. Storing an offset to the vector allows us to just set `buf_offset` for the new
    // bucket and be done.
    buf: PhantomData<pvec::PVec<BucketEntry<K, V>>>,
    buf_offset: isize,
}

impl<K, V> Persistable for Bucket<K, V>
where
    K: Persistable,
    V: Persistable,
{
}

impl<K, V> Bucket<K, V>
where
    K: Eq + Default + Persistable + PersistentlyAllocatable + std::cmp::PartialEq,
    V: Persistable,
{
    fn insert<T>(&mut self, k: T, v: V)
    where
        K: CopyFrom<From = T> + PartialEq<T>,
    {
        if self.buf_offset == 0 {
            panic!("attempt to insert into uninitialized bucket");
        }

        let vector = unsafe {
            std::ptr::addr_of!(self.buf_offset)
                .byte_offset(self.buf_offset.try_into().unwrap())
                .cast_mut()
                .cast::<pvec::PVec<BucketEntry<K, V>>>()
        };

        // FIXME should be calling `to_bytes()` for both the key and the value.
        unsafe {
            match (&*vector).find(|e| e.key == k) {
                Some(idx) => {
                    #[cfg(debug_assertions)]
                    println!("found at {idx}, will replace");

                    let value_ptr = crate::unit_ptr_of!(&(&mut *vector)[idx].value);
                    nando_tls::add_new_pre_image(value_ptr, (&mut *vector)[idx].value.as_bytes());
                    (&mut *vector)[idx].value = v;
                    nando_tls::add_new_post_image_if_changed(
                        value_ptr,
                        (&mut *vector)[idx].value.as_bytes(),
                    );
                }
                None => {
                    let key = {
                        let mut key = K::default();
                        let allocator = Arc::clone(&(*vector).get_allocator().unwrap());
                        key.set_allocator(allocator);

                        key
                    };

                    let idx = match self.empty_slots.pop() {
                        Some(i) => {
                            (*vector).push_at(BucketEntry { key, value: v }, i.into());
                            i
                        }
                        None => {
                            (*vector).push(BucketEntry { key, value: v });
                            (*vector).len() - 1
                        }
                    };

                    // NOTE For the underlying offset math to work out, we first need to place the
                    // entry in its final slot in the vector, and only then copy over the original
                    // key into the newly allocated key: PString. This is definitely a mistake
                    // of the current design.
                    let entry = &mut (*vector)[idx];
                    entry.key.from(k);
                }
            }
        };
    }

    unsafe fn get_vector(&self) -> &mut pvec::PVec<BucketEntry<K, V>> {
        let vector = std::ptr::addr_of!(self.buf_offset)
            .byte_offset(self.buf_offset)
            .cast_mut()
            .cast::<pvec::PVec<BucketEntry<K, V>>>();

        &mut *vector
    }

    fn set_vector(&mut self, vector_ptr: *mut pvec::PVec<BucketEntry<K, V>>) {
        let bucket_buf_offset_addr = std::ptr::addr_of!(self.buf_offset) as usize;
        let vector_addr = vector_ptr as *const () as usize;

        let computed_offset = if vector_addr > bucket_buf_offset_addr {
            (vector_addr - bucket_buf_offset_addr).try_into().unwrap()
        } else {
            -1isize
                * <usize as TryInto<isize>>::try_into(bucket_buf_offset_addr - vector_addr).unwrap()
        };

        let buf_offset_ptr = crate::unit_ptr_of!(&self.buf_offset);
        nando_tls::add_new_pre_image(buf_offset_ptr, self.buf_offset.as_bytes());
        self.buf_offset = computed_offset;
        nando_tls::add_new_post_image_if_changed(buf_offset_ptr, self.buf_offset.as_bytes());
    }

    fn clear(&mut self) {
        let vec = unsafe { self.get_vector() };
        vec.truncate(0);
    }
}

pub struct BucketIter<'a, K, V>
where
    K: Persistable,
    V: Persistable,
{
    inner: pvec::PVecIter<'a, BucketEntry<K, V>>,
}

impl<'a, K, V> Iterator for BucketIter<'a, K, V>
where
    K: Persistable,
    V: Persistable,
{
    type Item = (&'a K, &'a V);

    fn next(&mut self) -> Option<Self::Item> {
        match self.inner.next() {
            None => None,
            Some(ref entry) => Some((&entry.key, &entry.value)),
        }
    }
}

impl<'a, K, V> IntoIterator for &'a Bucket<K, V>
where
    K: Persistable,
    V: Persistable,
{
    type Item = (&'a K, &'a V);

    type IntoIter = BucketIter<'a, K, V>;

    fn into_iter(self) -> Self::IntoIter {
        let vector = unsafe {
            std::ptr::addr_of!(self.buf_offset)
                .byte_offset(self.buf_offset)
                .cast_mut()
                .cast::<pvec::PVec<BucketEntry<K, V>>>()
                .as_ref()
        };

        BucketIter {
            inner: vector.unwrap().into_iter(),
        }
    }
}

// TODO we should probably make geometric resizing the default and just get rid of this enum
#[allow(dead_code)]
#[doc(hidden)]
enum ResizePolicy {
    Conservative(usize),
    Geometric(usize),
}

#[repr(C)]
/// A persistable hash map, which is safely movable between runtime instances.
///
/// This implementation leverages the [`PVec`] implementation to provide key/value pair backing
/// storage for a collection of Buckets. Note that because of how resizing is currently performed,
/// and the use of a stateless bump allocator to manage allocations, we end up leaking the space
/// for all the Bucket tables we had allocated for all past capacities.
///
/// [`PVec`]: ../pvec/struct.PVec.html
#[derive(Debug)]
pub struct PHashMap<K: Persistable, V: Persistable> {
    num_elements: usize,
    capacity: usize,
    num_buckets: usize,
    table: PhantomData<Bucket<K, V>>,
    table_offset: usize,

    #[doc(hidden)]
    allocator: MaybeUninit<Arc<RwLock<BumpAllocator>>>,
}

impl<K, V> Persistable for PHashMap<K, V>
where
    K: Persistable,
    V: Persistable,
{
}

impl<K, V> PHashMap<K, V>
where
    K: Eq + Default + Hash + Persistable + PersistentlyAllocatable,
    V: Persistable,
{
    /// Instantiates a new empty hashmap. No allocations are made as a result of this call.
    pub fn new() -> Self {
        Self {
            num_elements: 0,
            capacity: 0,
            num_buckets: 0,
            table: PhantomData,
            table_offset: 0,
            allocator: MaybeUninit::uninit(),
        }
    }

    #[doc(hidden)]
    fn get_allocator_internal(&'_ self) -> &'_ Arc<RwLock<BumpAllocator>> {
        // NOTE This should not be called before `set_allocator()` has been called for a newly
        // instantiated object.
        unsafe { self.allocator.assume_init_ref() }
    }

    #[doc(hidden)]
    pub fn set_hasher(&mut self) {
        todo!("set_hasher")
    }

    /// Returns the number of elements in the map.
    pub fn len(&self) -> usize {
        self.num_elements
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Allocates space for the [`PHashMap`] to hold at least `capacity` elements before a
    /// reallocation is needed.
    pub fn with_capacity(&mut self, capacity: usize) {
        let capacity_ptr = crate::unit_ptr_of!(&self.capacity);
        nando_tls::add_new_pre_image(capacity_ptr, self.capacity.as_bytes());

        let (table_offset, num_buckets) = {
            let (allocation_res, adjusted_capacity, num_buckets) =
                self.allocate_table_with_capacity(capacity);
            self.capacity = std::cmp::max(capacity, adjusted_capacity);

            let base = std::ptr::addr_of!(self.table_offset) as *const ();
            (
                (allocation_res.as_ptr() as *const () as usize) - (base as usize),
                num_buckets,
            )
        };

        nando_tls::add_new_post_image_if_changed(capacity_ptr, self.capacity.as_bytes());

        let table_offset_ptr = crate::unit_ptr_of!(&self.table_offset);
        nando_tls::add_new_pre_image(table_offset_ptr, self.table_offset.as_bytes());
        self.table_offset = table_offset;
        nando_tls::add_new_post_image_if_changed(table_offset_ptr, self.table_offset.as_bytes());

        let num_buckets_ptr = crate::unit_ptr_of!(&self.num_buckets);
        nando_tls::add_new_pre_image(num_buckets_ptr, self.num_buckets.as_bytes());
        self.num_buckets = num_buckets;
        nando_tls::add_new_post_image_if_changed(num_buckets_ptr, self.num_buckets.as_bytes());

        // allocate a contiguous region for the vectors of the above buckets
        let vector_start = self
            .allocate_space_for_backing_vectors(num_buckets)
            .as_ptr();

        let allocator = self.get_allocator_internal();
        for i in 0..self.num_buckets {
            unsafe {
                let bucket_addr = std::ptr::addr_of!(self.table_offset)
                    .byte_offset(
                        (self.table_offset + i * std::mem::size_of::<Bucket<K, V>>())
                            .try_into()
                            .unwrap(),
                    )
                    .cast_mut()
                    .cast::<Bucket<K, V>>();

                nando_tls::add_new_pre_image(bucket_addr as *const (), (*bucket_addr).as_bytes());
                // initialize them with the avg per bucket capacity
                *bucket_addr = Bucket {
                    empty_slots: pvec::PVec::new(),
                    buf: PhantomData,
                    buf_offset: 0,
                };

                (*bucket_addr)
                    .empty_slots
                    .set_allocator(Arc::clone(&allocator));
                (*bucket_addr)
                    .empty_slots
                    .resize_to_capacity((BUCKET_AVERAGE_LENGTH).into());

                let vector_addr = vector_start.byte_offset(
                    (i * std::mem::size_of::<pvec::PVec<BucketEntry<K, V>>>())
                        .try_into()
                        .unwrap(),
                );

                // NOTE @unstable the below should always give us a correct result, as, at this point, the vector
                // allocation is guaranteed to be further down the object than the address of the
                // Bucket table. That might break in the future, if we change the implementation of
                // the bump allocator, or use a different allocator.
                (*bucket_addr).buf_offset = (vector_addr as *const () as usize
                    - std::ptr::addr_of!((*bucket_addr).buf_offset) as usize)
                    .try_into()
                    .unwrap();
                nando_tls::add_new_post_image_if_changed(
                    bucket_addr as *const (),
                    (*bucket_addr).as_bytes(),
                );
            }
        }
    }

    /// Inserts a key-value pair into the [`PHashMap`], resizing if necessary.
    ///
    /// If the element was already in the [`PHashMap`], the entry for the key is updated, and the
    /// old value is returned. Otherwise, `None` is returned.
    // TODO set the allocator for the entry
    pub fn insert<T>(&mut self, k: T, v: V) -> Option<V>
    where
        T: std::hash::Hash,
        K: CopyFrom<From = T> + PartialEq<T>,
    {
        // FIXME this will repeat the hashing process from below to return the result, should
        // do some rewriting to avoid this kind of thing.
        let old_value = match self.get(&k) {
            None => None,
            Some(v) => Some(unsafe { std::mem::transmute_copy(v) }),
        };

        // First, we need to check if we have reached capacity
        if old_value.is_none() && self.under_pressure() {
            // #[cfg(debug_assertions)]
            println!(
                "will resize, current num elements is {}, capacity is {}",
                self.num_elements, self.capacity
            );
            // double our capacity
            unsafe { self.resize(ResizePolicy::Geometric(2)) };
        }

        let index: usize = self.get_hash_for_other_key(&k);
        let bucket = unsafe { self.get_bucket_at_index(index) };

        bucket.insert(k, v);

        // Only update the number of elements if we inserted a new key as opposed to overwriting a
        // previous value.
        if old_value.is_none() {
            let num_elements_ptr = crate::unit_ptr_of!(&self.num_elements);
            nando_tls::add_new_pre_image(num_elements_ptr, self.num_elements.as_bytes());
            self.num_elements += 1;
            nando_tls::add_new_post_image_if_changed(
                num_elements_ptr,
                self.num_elements.as_bytes(),
            );
        }

        old_value
    }

    pub fn clear(&mut self) {
        let num_elements_ptr = crate::unit_ptr_of!(&self.num_elements);
        nando_tls::add_new_pre_image(num_elements_ptr, self.num_elements.as_bytes());
        self.num_elements = 0;
        nando_tls::add_new_post_image_if_changed(num_elements_ptr, self.num_elements.as_bytes());

        for bucket_idx in 0..self.num_buckets {
            let bucket = unsafe { self.get_bucket_at_index(bucket_idx) };
            bucket.clear();
        }
    }

    /// Returns a reference to the value corresponding to the key.
    pub fn get<T>(&self, k: &T) -> Option<&V>
    where
        T: std::hash::Hash,
        K: CopyFrom<From = T> + PartialEq<T>,
    {
        let index: usize = self.get_hash_for_other_key::<T>(k);
        let bucket = unsafe { self.get_bucket_at_index(index) };
        let bucket_storage = unsafe { bucket.get_vector() };

        for e in bucket_storage.iter() {
            if e.key == *k {
                return Some(&e.value);
            }
        }

        None
    }

    fn get_pkey(&self, k: &K) -> Option<&V> {
        let index: usize = self.get_hash_for_key(k);
        let bucket = unsafe { self.get_bucket_at_index(index) };
        let bucket_storage = unsafe { bucket.get_vector() };

        for e in bucket_storage.iter() {
            if e.key == *k {
                return Some(&e.value);
            }
        }

        None
    }

    /// Returns a mutable reference to the value corresponding to the (persistable) key.
    pub fn get_mut(&self, k: &K) -> Option<&mut V> {
        let index: usize = self.get_hash_for_key(k);
        let bucket = unsafe { self.get_bucket_at_index(index) };
        let bucket_storage = unsafe { bucket.get_vector() };

        for e in bucket_storage.iter_mut() {
            if e.key == *k {
                return Some(&mut e.value);
            }
        }

        None
    }

    // FIXME bad name
    /// Returns a mutable reference to the value corresponding to the key.
    pub fn get_mut_non_persistent_key<T>(&self, k: &T) -> Option<&mut V>
    where
        T: std::hash::Hash,
        K: CopyFrom<From = T> + PartialEq<T>,
    {
        let index: usize = self.get_hash_for_other_key::<T>(k);
        let bucket = unsafe { self.get_bucket_at_index(index) };
        let bucket_storage = unsafe { bucket.get_vector() };

        for (idx, e) in bucket_storage.iter().enumerate() {
            if e.key == *k {
                return Some(&mut bucket_storage[idx].value);
            }
        }

        None
    }

    /// Returns `true` if the map contains a value for the specified key.
    pub fn contains(&self, k: &K) -> bool {
        let index: usize = self.get_hash_for_key(&k);
        let bucket = unsafe { self.get_bucket_at_index(index) };
        let bucket_storage = unsafe { bucket.get_vector() };

        for e in bucket_storage.iter() {
            if e.key == *k {
                return true;
            }
        }

        false
    }

    /// Returns `true` if the map contains a value for the specified key. Note that this supports
    /// keys that can be converted into the native key type (unlike [`contains`]).
    pub fn contains_key<T>(&self, k: &T) -> bool
    where
        T: std::hash::Hash,
        K: CopyFrom<From = T> + PartialEq<T>,
    {
        let index: usize = self.get_hash_for_other_key::<T>(k);
        let bucket = unsafe { self.get_bucket_at_index(index) };
        let bucket_storage = unsafe { bucket.get_vector() };

        for e in bucket_storage.iter() {
            if e.key == *k {
                return true;
            }
        }

        false
    }

    #[doc(hidden)]
    fn allocate_table_with_capacity(&self, capacity: usize) -> (NonNull<[u8]>, usize, usize) {
        let allocator = self.get_allocator_internal();

        // target a load factor of 0.8
        let bucket_avg_length = BUCKET_AVERAGE_LENGTH as usize;
        let (num_buckets, adjusted_capacity) = match capacity > bucket_avg_length {
            true => (
                ((capacity / bucket_avg_length) * 5 / 4).next_power_of_two(),
                capacity,
            ),
            false => {
                let num_buckets = (bucket_avg_length * 5 / 4).next_power_of_two();
                let adjusted_capacity = num_buckets * bucket_avg_length * 4 / 5;

                (num_buckets, adjusted_capacity)
            }
        };

        let source_layout = Layout::array::<Bucket<K, V>>(num_buckets).unwrap();
        let allocator = allocator.read();

        (
            allocator
                .allocate(source_layout)
                .expect("failed to allocate in `with_capacity`"),
            adjusted_capacity,
            num_buckets,
        )
    }

    #[doc(hidden)]
    fn allocate_space_for_backing_vectors(&self, num_vectors: usize) -> NonNull<[u8]> {
        let allocator = self.get_allocator_internal();

        let source_layout = Layout::array::<pvec::PVec<BucketEntry<K, V>>>(num_vectors).unwrap();
        let vector_start = {
            let allocator = allocator.read();

            allocator
                .allocate(source_layout)
                .expect("failed to allocate in `with_capacity`")
        };

        for i in 0..num_vectors {
            unsafe {
                let vector_addr = vector_start.as_ptr().byte_offset(
                    (i * std::mem::size_of::<pvec::PVec<BucketEntry<K, V>>>())
                        .try_into()
                        .unwrap(),
                );
                let vector_mut = (vector_addr as *const ())
                    .cast_mut()
                    .cast::<pvec::PVec<BucketEntry<K, V>>>();

                nando_tls::add_new_pre_image(vector_mut as *const (), (&*vector_mut).as_bytes());
                *vector_mut = pvec::PVec::new();
                (*vector_mut).set_allocator(Arc::clone(&allocator));
                (*vector_mut).resize_to_capacity(BUCKET_AVERAGE_LENGTH.into());
                nando_tls::add_new_post_image_if_changed(
                    vector_mut as *const (),
                    (&*vector_mut).as_bytes(),
                );
            }
        }

        vector_start
    }

    #[doc(hidden)]
    fn under_pressure(&self) -> bool {
        self.num_elements as f64 / self.capacity as f64 >= 0.9
    }

    #[doc(hidden)]
    fn get_hash_for_key<T>(&self, key: &T) -> usize
    where
        T: std::hash::Hash,
        K: From<T>,
    {
        let mut hasher = DefaultHasher::default();
        let key_hash = {
            key.hash(&mut hasher);
            hasher.finish()
        } as usize;

        key_hash % self.num_buckets
    }

    fn get_hash_for_other_key<T>(&self, key: &T) -> usize
    where
        T: std::hash::Hash,
        K: CopyFrom<From = T>,
    {
        let mut hasher = DefaultHasher::default();
        let key_hash = {
            key.hash(&mut hasher);
            hasher.finish()
        } as usize;

        key_hash % self.num_buckets
    }

    unsafe fn get_bucket_at_index_ptr(&self, idx: usize) -> *mut Bucket<K, V> {
        std::ptr::addr_of!(self.table_offset)
            .byte_offset(
                (self.table_offset + idx * std::mem::size_of::<Bucket<K, V>>())
                    .try_into()
                    .unwrap(),
            )
            .cast_mut()
            .cast::<Bucket<K, V>>()
    }

    #[doc(hidden)]
    unsafe fn get_bucket_at_index(&self, idx: usize) -> &mut Bucket<K, V> {
        &mut *self.get_bucket_at_index_ptr(idx)
    }

    // FIXME clean up
    #[doc(hidden)]
    unsafe fn resize(&mut self, resize_policy: ResizePolicy) {
        let capacity = match resize_policy {
            ResizePolicy::Conservative(to_add) => self.capacity + to_add,
            ResizePolicy::Geometric(to_multiply) => self.capacity * to_multiply,
        };

        let capacity_ptr = crate::unit_ptr_of!(&self.capacity);
        nando_tls::add_new_pre_image(capacity_ptr, self.capacity.as_bytes());

        // first allocate a new table with the new capacity to hold the new buckets
        let (table_offset, num_buckets) = {
            let (allocation_res, adjusted_capacity, num_buckets) =
                self.allocate_table_with_capacity(capacity);

            self.capacity = std::cmp::max(capacity, adjusted_capacity);

            let base = std::ptr::addr_of!(self.table_offset) as *const ();
            (
                (allocation_res.as_ptr() as *const () as usize) - (base as usize),
                num_buckets,
            )
        };

        nando_tls::add_new_post_image_if_changed(capacity_ptr, self.capacity.as_bytes());

        let table_offset_old = self.table_offset;
        let num_buckets_old = self.num_buckets;

        let table_offset_ptr = crate::unit_ptr_of!(&self.table_offset);
        nando_tls::add_new_pre_image(table_offset_ptr, self.table_offset.as_bytes());
        self.table_offset = table_offset;
        nando_tls::add_new_post_image_if_changed(table_offset_ptr, self.table_offset.as_bytes());

        let num_buckets_ptr = crate::unit_ptr_of!(&self.num_buckets);
        nando_tls::add_new_pre_image(num_buckets_ptr, self.num_buckets.as_bytes());
        self.num_buckets = num_buckets;
        nando_tls::add_new_post_image_if_changed(num_buckets_ptr, self.num_buckets.as_bytes());

        // allocate enough space for the new vectors, since we will be reusing the previously
        // allocated vectors after we're done rehashing their old contents.
        let allocator = self.get_allocator_internal();
        let num_new_vectors = self.num_buckets - num_buckets_old + 1;
        let vector_start = self
            .allocate_space_for_backing_vectors(num_new_vectors)
            .as_ptr();
        let mut last_used_vector = 0;

        for i in 0..num_buckets {
            let bucket = self.get_bucket_at_index(i);
            nando_tls::add_new_pre_image(
                std::ptr::addr_of!(*bucket) as *const (),
                (*bucket).as_bytes(),
            );
            *bucket = Bucket {
                empty_slots: pvec::PVec::new(),
                buf: PhantomData,
                buf_offset: 0,
            };
            (*bucket).empty_slots.set_allocator(Arc::clone(&allocator));
            (*bucket)
                .empty_slots
                .resize_to_capacity((BUCKET_AVERAGE_LENGTH).into());
            nando_tls::add_new_post_image_if_changed(
                std::ptr::addr_of!(*bucket) as *const (),
                (*bucket).as_bytes(),
            );
        }

        let mut reusable_vectors: Vec<&mut pvec::PVec<BucketEntry<K, V>>> = vec![];

        // rehash old keys, move to new map
        for old_bucket_idx in 0..num_buckets_old {
            let old_bucket = std::ptr::addr_of!(self.table_offset)
                .byte_offset(
                    (table_offset_old + old_bucket_idx * std::mem::size_of::<Bucket<K, V>>())
                        .try_into()
                        .unwrap(),
                )
                .cast_mut()
                .cast::<Bucket<K, V>>();

            let target_vector = (*old_bucket).get_vector();
            let mut target_vector_reassigned = false;

            for j in 0..target_vector.len() {
                let entry = match target_vector.get_ref(j) {
                    None => continue,
                    Some(p) => p.as_ref().unwrap(),
                };

                let new_idx = self.get_hash_for_key(&(*entry).key);
                let new_bucket = std::ptr::addr_of!(self.table_offset)
                    .byte_offset(
                        (self.table_offset + new_idx * std::mem::size_of::<Bucket<K, V>>())
                            .try_into()
                            .unwrap(),
                    )
                    .cast_mut()
                    .cast::<Bucket<K, V>>();

                nando_tls::add_new_pre_image(new_bucket as *const (), (&*new_bucket).as_bytes());

                if (*new_bucket).buf_offset == 0 {
                    let new_bucket_vector_ptr =
                        if target_vector.len() == (*old_bucket).empty_slots.len() {
                            target_vector.truncate(0);
                            target_vector_reassigned = true;

                            std::ptr::addr_of!(*target_vector).cast_mut()
                        } else if reusable_vectors.len() > 0 {
                            let vector_to_reuse = reusable_vectors.pop().unwrap();
                            std::ptr::addr_of!(*vector_to_reuse).cast_mut()
                        } else {
                            if last_used_vector >= num_new_vectors {
                                panic!(
                                    "about to use unallocated vector {} / {}",
                                    last_used_vector,
                                    self.num_buckets - num_buckets_old
                                );
                            }

                            let vector_addr = vector_start.byte_offset(
                                (last_used_vector
                                    * std::mem::size_of::<pvec::PVec<BucketEntry<K, V>>>())
                                .try_into()
                                .unwrap(),
                            );
                            last_used_vector += 1;

                            (vector_addr as *const ())
                                .cast_mut()
                                .cast::<pvec::PVec<BucketEntry<K, V>>>()
                        };

                    (*new_bucket).set_vector(new_bucket_vector_ptr);
                }

                nando_tls::add_new_post_image_if_changed(
                    new_bucket as *const (),
                    (&*new_bucket).as_bytes(),
                );

                // finally, move the rehashed entry
                let entry_to_place = match target_vector.pop_at(j) {
                    None => continue,
                    Some(e) => {
                        (*old_bucket).empty_slots.push(j);
                        e
                    }
                };

                let target_vector = (*new_bucket).get_vector();
                let placed_entry = target_vector.push(entry_to_place);
                (&mut *placed_entry).key.adjust_from(&(*entry).key);
                (&mut *placed_entry).value.adjust_from(&(*entry).value);
            }

            if !target_vector_reassigned {
                target_vector.truncate(0);
                reusable_vectors.push(target_vector);
            }
        }

        // at this point there are buckets that have not had their `buf_offset` set to an
        // appropriate value because none of the pre-existing keys ended up hashing into them. We
        // need to fix that.
        for i in 0..num_buckets {
            let bucket = self.get_bucket_at_index(i);
            if bucket.buf_offset != 0 {
                // bucket has already been initialized during rehashing, nothing to do.
                continue;
            }

            let vector_addr = match reusable_vectors.pop() {
                Some(v) => std::ptr::addr_of!(*v).cast_mut(),
                None => {
                    if last_used_vector >= num_new_vectors {
                        panic!(
                            "about to use unallocated vector {} / {}",
                            last_used_vector,
                            self.num_buckets - num_buckets_old
                        );
                    }

                    let vector_addr = (vector_start.byte_offset(
                        (last_used_vector * std::mem::size_of::<pvec::PVec<BucketEntry<K, V>>>())
                            .try_into()
                            .unwrap(),
                    ) as *const ())
                        .cast_mut()
                        .cast::<pvec::PVec<BucketEntry<K, V>>>();
                    last_used_vector += 1;

                    vector_addr
                }
            };

            bucket.set_vector(vector_addr);

            nando_tls::add_new_post_image_if_changed(
                std::ptr::addr_of!(*bucket) as *const (),
                (*bucket).as_bytes(),
            );
        }
    }

    pub fn iter(&self) -> PHashMapIter<'_, K, V> {
        let bucket_for_iter = unsafe { &mut *get_bucket_for_iter!(self, 0) };

        PHashMapIter {
            inner: self,
            current_bucket_idx: 0,
            bucket_iter: bucket_for_iter.into_iter(),
        }
    }

    pub fn group_buckets(&self, num_groups: usize) -> Vec<std::ops::Range<usize>> {
        let num_buckets = self.num_buckets;
        let mut groups = Vec::with_capacity(num_groups);

        let mut start_idx = 0;
        let elements_per_group = (num_buckets as f64 / num_groups as f64).ceil() as usize;
        while start_idx < num_buckets {
            groups.push(start_idx..std::cmp::min(num_buckets, start_idx + elements_per_group));
            start_idx += elements_per_group;
        }

        groups
    }

    pub fn iter_for_bucket_range(
        &self,
        start_bucket: usize,
        end_bucket: usize,
    ) -> PHashMapRangeIter<'_, K, V> {
        let bucket_for_iter = unsafe { &mut *get_bucket_for_iter!(self, start_bucket) };

        PHashMapRangeIter {
            inner: self,
            current_bucket_idx: start_bucket,
            end_bucket_idx: end_bucket,
            bucket_iter: bucket_for_iter.into_iter(),
        }
    }

    // NOTE The {PIter, POther} variants used below are a lazy hack, but they will do
    // for prototyping. Once we have an allocator for magpie collections that allocates on the heap
    // (instead of requiring a backing file), we can operate entirely with PHashMap instances.
    pub fn union_as_map<'a, F>(
        &'a self,
        other: &'a std::collections::HashMap<K, V>,
        value_tiebreak_fn: F,
    ) -> std::collections::HashMap<K, V>
    where
        K: Copy,
        V: Copy,
        F: Fn((&'a K, &'a V), (&'a K, &'a V)) -> (&'a K, &'a V),
    {
        let diff_self_to_other = MapDifferencePIter {
            iter: self.iter(),
            other,
        };
        let diff_other_to_self = MapDifferencePOther {
            iter: other.iter(),
            other: self,
        };

        match self.len() >= other.len() {
            true => {
                let intersection_iter = MapIntersectionPOther {
                    iter: other.iter(),
                    other: self,
                    value_tiebreak_fn,
                };
                let union_iter = MapUnionPOther {
                    iter: intersection_iter.chain(diff_self_to_other.chain(diff_other_to_self)),
                };

                union_iter.map(|e| (*e.0, *e.1)).collect()
            }

            false => {
                let intersection_iter = MapIntersectionPIter {
                    iter: self.iter(),
                    other,
                    value_tiebreak_fn,
                };
                let union_iter = MapUnionPIter {
                    iter: intersection_iter.chain(diff_self_to_other.chain(diff_other_to_self)),
                };

                union_iter.map(|e| (*e.0, *e.1)).collect()
            }
        }
    }
}

impl<K, V> Into<std::collections::HashMap<K, V>> for &PHashMap<K, V>
where
    K: Eq + Default + Hash + Persistable + PersistentlyAllocatable + Copy,
    V: Persistable + Copy,
{
    fn into(self) -> std::collections::HashMap<K, V> {
        let mut res = std::collections::HashMap::with_capacity(self.len());
        for (k, v) in self.iter() {
            res.insert(*k, *v);
        }

        res
    }
}

impl<K, V> PersistentlyAllocatable for PHashMap<K, V>
where
    K: Eq + Default + Hash + Persistable + PersistentlyAllocatable,
    V: Persistable,
{
    /// Sets the allocator that is going to be used by the [`PHashMap`] instance whenever it needs to
    /// extend its internal capacity.
    ///
    /// **NOTE** This needs to be set _before_ any subsequent calls are made that would trigger an
    /// allocation.
    fn set_allocator(&mut self, allocator: Arc<RwLock<BumpAllocator>>) {
        self.allocator.write(Arc::clone(&allocator));
        for i in 0..self.num_buckets {
            unsafe {
                let bucket = self.get_bucket_at_index(i);
                bucket.get_vector().set_allocator(Arc::clone(&allocator));
            }
        }
    }

    fn get_allocator(&self) -> Option<Arc<RwLock<BumpAllocator>>> {
        Some(Arc::clone(self.get_allocator_internal()))
    }
}

pub struct PHashMapIter<'a, K, V>
where
    K: Persistable,
    V: Persistable,
{
    inner: &'a PHashMap<K, V>,
    current_bucket_idx: usize,
    bucket_iter: BucketIter<'a, K, V>,
}

impl<'a, K, V> Iterator for PHashMapIter<'a, K, V>
where
    K: Persistable,
    V: Persistable,
{
    type Item = (&'a K, &'a V);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            match self.bucket_iter.next() {
                Some(i) => return Some(i),
                None => {}
            }

            if self.current_bucket_idx == self.inner.num_buckets - 1 {
                return None;
            }

            self.current_bucket_idx += 1;
            let bucket_for_iter =
                unsafe { &mut *get_bucket_for_iter!(self.inner, self.current_bucket_idx) };
            self.bucket_iter = bucket_for_iter.into_iter();
        }
    }
}

impl<'a, K, V> IntoIterator for &'a PHashMap<K, V>
where
    K: Persistable,
    V: Persistable,
{
    type Item = (&'a K, &'a V);

    type IntoIter = PHashMapIter<'a, K, V>;

    fn into_iter(self) -> Self::IntoIter {
        let bucket_for_iter = unsafe { &mut *get_bucket_for_iter!(self, 0) };
        PHashMapIter {
            inner: self,
            current_bucket_idx: 0,
            bucket_iter: bucket_for_iter.into_iter(),
        }
    }
}

pub struct PHashMapRangeIter<'a, K, V>
where
    K: Persistable,
    V: Persistable,
{
    inner: &'a PHashMap<K, V>,
    current_bucket_idx: usize,
    end_bucket_idx: usize,
    bucket_iter: BucketIter<'a, K, V>,
}

impl<'a, K, V> Iterator for PHashMapRangeIter<'a, K, V>
where
    K: Persistable,
    V: Persistable,
{
    type Item = (&'a K, &'a V);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            match self.bucket_iter.next() {
                Some(i) => return Some(i),
                None => {}
            }

            if self.current_bucket_idx == self.inner.num_buckets - 1
                || self.current_bucket_idx == self.end_bucket_idx - 1
            {
                return None;
            }

            self.current_bucket_idx += 1;
            let bucket_for_iter =
                unsafe { &mut *get_bucket_for_iter!(self.inner, self.current_bucket_idx) };
            self.bucket_iter = bucket_for_iter.into_iter();
        }
    }
}

pub struct MapIntersection<'a, K, V, F>
where
    K: Eq + Default + Hash + Persistable + PersistentlyAllocatable,
    V: Persistable,
{
    iter: PHashMapIter<'a, K, V>,
    other: &'a PHashMap<K, V>,
    value_tiebreak_fn: F,
}

impl<'a, K, V, F> Iterator for MapIntersection<'a, K, V, F>
where
    K: Eq + Default + Hash + Persistable + PersistentlyAllocatable,
    V: Persistable,
    F: Fn((&'a K, &'a V), (&'a K, &'a V)) -> (&'a K, &'a V),
{
    type Item = (&'a K, &'a V);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let element = self.iter.next()?;

            match self.other.get_pkey(&element.0) {
                None => {}
                Some(value) => {
                    return Some((self.value_tiebreak_fn)(
                        (element.0, element.1),
                        (element.0, value),
                    ))
                }
            }
        }
    }
}

pub struct MapIntersectionPIter<'a, K, V, F>
where
    K: Eq + Default + Hash + Persistable + PersistentlyAllocatable,
    V: Persistable,
{
    iter: PHashMapIter<'a, K, V>,
    other: &'a std::collections::HashMap<K, V>,
    value_tiebreak_fn: F,
}

impl<'a, K, V, F> Iterator for MapIntersectionPIter<'a, K, V, F>
where
    K: Eq + Default + Hash + Persistable + PersistentlyAllocatable,
    V: Persistable,
    F: Fn((&'a K, &'a V), (&'a K, &'a V)) -> (&'a K, &'a V),
{
    type Item = (&'a K, &'a V);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let element = self.iter.next()?;

            match self.other.get(&element.0) {
                None => {}
                Some(value) => {
                    return Some((self.value_tiebreak_fn)(
                        (element.0, element.1),
                        (element.0, value),
                    ))
                }
            }
        }
    }
}

pub struct MapIntersectionPOther<'a, K, V, F>
where
    K: Eq + Default + Hash + Persistable + PersistentlyAllocatable,
    V: Persistable,
{
    iter: std::collections::hash_map::Iter<'a, K, V>,
    other: &'a PHashMap<K, V>,
    value_tiebreak_fn: F,
}

impl<'a, K, V, F> Iterator for MapIntersectionPOther<'a, K, V, F>
where
    K: Eq + Default + Hash + Persistable + PersistentlyAllocatable,
    V: Persistable,
    F: Fn((&'a K, &'a V), (&'a K, &'a V)) -> (&'a K, &'a V),
{
    type Item = (&'a K, &'a V);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let element = self.iter.next()?;

            match self.other.get_pkey(&element.0) {
                None => {}
                Some(value) => {
                    return Some((self.value_tiebreak_fn)(
                        (element.0, element.1),
                        (element.0, value),
                    ))
                }
            }
        }
    }
}

pub struct MapDifference<'a, K, V>
where
    K: Persistable,
    V: Persistable,
{
    iter: PHashMapIter<'a, K, V>,
    other: &'a PHashMap<K, V>,
}

impl<'a, K, V> Iterator for MapDifference<'a, K, V>
where
    K: Eq + Default + Hash + Persistable + PersistentlyAllocatable,
    V: Persistable,
{
    type Item = (&'a K, &'a V);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let element = self.iter.next()?;

            if !self.other.contains(element.0) {
                return Some(element);
            }
        }
    }
}

pub struct MapDifferencePIter<'a, K, V>
where
    K: Persistable,
    V: Persistable,
{
    iter: PHashMapIter<'a, K, V>,
    other: &'a std::collections::HashMap<K, V>,
}

impl<'a, K, V> Iterator for MapDifferencePIter<'a, K, V>
where
    K: Eq + Default + Hash + Persistable + PersistentlyAllocatable,
    V: Persistable,
{
    type Item = (&'a K, &'a V);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let element = self.iter.next()?;

            if !self.other.contains_key(element.0) {
                return Some(element);
            }
        }
    }
}

pub struct MapDifferencePOther<'a, K, V>
where
    K: Persistable,
    V: Persistable,
{
    iter: std::collections::hash_map::Iter<'a, K, V>,
    other: &'a PHashMap<K, V>,
}

impl<'a, K, V> Iterator for MapDifferencePOther<'a, K, V>
where
    K: Eq + Default + Hash + Persistable + PersistentlyAllocatable,
    V: Persistable,
{
    type Item = (&'a K, &'a V);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let element = self.iter.next()?;

            if !self.other.contains(element.0) {
                return Some(element);
            }
        }
    }
}

pub struct MapUnion<'a, K, V, F>
where
    K: Eq + Default + Hash + Persistable + PersistentlyAllocatable,
    V: Persistable,
    F: Fn((&'a K, &'a V), (&'a K, &'a V)) -> (&'a K, &'a V),
{
    iter: Chain<
        MapIntersection<'a, K, V, F>,
        Chain<MapDifferencePIter<'a, K, V>, MapDifferencePOther<'a, K, V>>,
    >,
}

impl<'a, K, V, F> Iterator for MapUnion<'a, K, V, F>
where
    K: Eq + Default + Hash + Persistable + PersistentlyAllocatable,
    V: Persistable,
    F: Fn((&'a K, &'a V), (&'a K, &'a V)) -> (&'a K, &'a V),
{
    type Item = (&'a K, &'a V);

    fn next(&mut self) -> Option<Self::Item> {
        self.iter.next()
    }
}

pub struct MapUnionPIter<'a, K, V, F>
where
    K: Eq + Default + Hash + Persistable + PersistentlyAllocatable,
    V: Persistable,
    F: Fn((&'a K, &'a V), (&'a K, &'a V)) -> (&'a K, &'a V),
{
    iter: Chain<
        MapIntersectionPIter<'a, K, V, F>,
        Chain<MapDifferencePIter<'a, K, V>, MapDifferencePOther<'a, K, V>>,
    >,
}

impl<'a, K, V, F> Iterator for MapUnionPIter<'a, K, V, F>
where
    K: Eq + Default + Hash + Persistable + PersistentlyAllocatable,
    V: Persistable,
    F: Fn((&'a K, &'a V), (&'a K, &'a V)) -> (&'a K, &'a V),
{
    type Item = (&'a K, &'a V);

    fn next(&mut self) -> Option<Self::Item> {
        self.iter.next()
    }
}
pub struct MapUnionPOther<'a, K, V, F>
where
    K: Eq + Default + Hash + Persistable + PersistentlyAllocatable,
    V: Persistable,
    F: Fn((&'a K, &'a V), (&'a K, &'a V)) -> (&'a K, &'a V),
{
    iter: Chain<
        MapIntersectionPOther<'a, K, V, F>,
        Chain<MapDifferencePIter<'a, K, V>, MapDifferencePOther<'a, K, V>>,
    >,
}

impl<'a, K, V, F> Iterator for MapUnionPOther<'a, K, V, F>
where
    K: Eq + Default + Hash + Persistable + PersistentlyAllocatable,
    V: Persistable,
    F: Fn((&'a K, &'a V), (&'a K, &'a V)) -> (&'a K, &'a V),
{
    type Item = (&'a K, &'a V);

    fn next(&mut self) -> Option<Self::Item> {
        self.iter.next()
    }
}
