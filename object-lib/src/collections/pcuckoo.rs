use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::mem::MaybeUninit;
use std::sync::Arc;

use parking_lot::RwLock;
use rand::{rngs::SmallRng, Rng, SeedableRng};

use crate::allocators::{
    bump_allocator::BumpAllocator, persistently_allocatable::PersistentlyAllocatable,
};
use crate::collections::{fixed_pvec::FixedPVec, pvec::PVec};
use crate::tls as nando_tls;
use crate::unit_ptr_of;
use crate::Persistable;

#[repr(C)]
struct PCuckooFilterTable {
    bits_per_tag: usize,
    buf: PVec<FixedPVec<u32>>,
}

impl Persistable for PCuckooFilterTable {}
impl PersistentlyAllocatable for PCuckooFilterTable {
    fn set_allocator(&mut self, allocator: Arc<RwLock<BumpAllocator>>) {
        self.buf.set_allocator(allocator);
    }

    fn get_allocator(&self) -> Option<Arc<RwLock<BumpAllocator>>> {
        self.buf.get_allocator()
    }
}

enum BucketInsertResult {
    Ok,
    Replaced(u32),
    NotInserted,
}

static ELEMS_PER_TABLE_ROW: usize = 4;

impl PCuckooFilterTable {
    pub fn new(bits_per_tag: usize) -> Self {
        if bits_per_tag > 32 {
            panic!("tag size cannot exceed 32 bits");
        }

        Self {
            bits_per_tag,
            buf: PVec::new(),
        }
    }

    pub fn resize_to_capacity(&mut self, capacity: usize) {
        self.buf.resize_to_capacity(capacity)
    }

    fn insert_at(&mut self, index: u32, tag: u32, should_kick_out: bool) -> BucketInsertResult {
        // FIXME can we avoid computing this for every call to insert_at?
        let tags_per_bucket = (32 / self.bits_per_tag) * self.buf[0].capacity();
        for tag_idx in 0..tags_per_bucket {
            if self.read_tag(index, tag_idx) != 0 {
                continue;
            }

            self.write_tag(index, tag_idx, tag);
            return BucketInsertResult::Ok;
        }

        if !should_kick_out {
            return BucketInsertResult::NotInserted;
        }

        let eviction_tag_idx = {
            let mut eviction_rng = SmallRng::from_entropy();
            eviction_rng.gen::<usize>() % tags_per_bucket
        };
        let old_tag = self.read_tag(index, eviction_tag_idx);
        self.write_tag(index, eviction_tag_idx, tag);

        BucketInsertResult::Replaced(old_tag)
    }

    #[inline]
    fn read_tag(&self, index: u32, tag_idx: usize) -> u32 {
        let row = &self.buf[index.try_into().unwrap()];
        let entry_idx: usize = tag_idx / (32 / self.bits_per_tag);
        let inner_tag_idx = tag_idx % (32 / self.bits_per_tag);
        let tag_mask = (1 << self.bits_per_tag) - 1;

        (row[entry_idx] >> (inner_tag_idx * self.bits_per_tag)) & tag_mask
    }

    #[inline]
    fn write_tag(&mut self, index: u32, tag_idx: usize, value: u32) {
        let row = &mut self.buf[index.try_into().unwrap()];
        let entry_idx: usize = tag_idx / (32 / self.bits_per_tag);
        let inner_tag_idx = tag_idx % (32 / self.bits_per_tag);
        let tag_mask = (1 << self.bits_per_tag) - 1;

        let entry_ptr = unit_ptr_of!(&row[entry_idx]);
        let mut entry = row[entry_idx];

        nando_tls::add_new_pre_image(entry_ptr, entry.as_bytes());
        entry &= u32::MAX ^ (tag_mask << (inner_tag_idx * self.bits_per_tag));
        entry |= (value & tag_mask) << (inner_tag_idx * self.bits_per_tag);
        row[entry_idx] = entry;
        nando_tls::add_new_post_image_if_changed(entry_ptr, entry.as_bytes());
    }

    fn bucket_contains_tag(&self, index: u32, tag: u32) -> bool {
        // FIXME can we avoid computing this for every call to bucket_contains_tag?
        let tags_per_bucket = (32 / self.bits_per_tag) * self.buf[0].capacity();
        for tag_idx in 0..tags_per_bucket {
            if self.read_tag(index, tag_idx) == tag {
                return true;
            }
        }

        false
    }

    fn delete_tag_from_bucket(&mut self, index: u32, tag: u32) -> bool {
        // FIXME can we avoid computing this for every call to bucket_contains_tag?
        let tags_per_bucket = (32 / self.bits_per_tag) * self.buf[0].capacity();
        for tag_idx in 0..tags_per_bucket {
            if self.read_tag(index, tag_idx) != tag {
                continue;
            }

            self.write_tag(index, tag_idx, 0);
            return true;
        }

        false
    }

    fn size_as_num_tags(&self) -> usize {
        let tags_per_bucket = (32 / self.bits_per_tag) * self.buf[0].capacity();
        tags_per_bucket * self.buf.capacity()
    }
}

#[repr(C)]
pub struct PCuckooFilter {
    associativity: usize,
    num_buckets: usize,
    bits_per_item: usize,
    table: PCuckooFilterTable,
    num_items: usize,

    #[doc(hidden)]
    allocator: MaybeUninit<Arc<RwLock<BumpAllocator>>>,
}

impl Persistable for PCuckooFilter {}

static MAX_KICK_COUNT: u32 = 500;

#[derive(PartialEq, Eq, Debug)]
pub enum Status {
    Ok,
    NotFound,
    NoSpace,
}

impl PCuckooFilter {
    // NOTE initialization logic taken directly from the original authors' cuckoofilter
    // implementation
    pub fn new(max_num_keys: usize, bits_per_item: usize) -> Self {
        let associativity = (32 / bits_per_item) * ELEMS_PER_TABLE_ROW;
        let mut num_buckets = std::cmp::max(1, max_num_keys / associativity).next_power_of_two();

        // check if max potential load exceeds a given threshold
        if (max_num_keys as f64 / num_buckets as f64) / associativity as f64 > 0.96 {
            num_buckets <<= 1;
        }

        Self {
            associativity,
            num_buckets,
            bits_per_item,
            table: PCuckooFilterTable::new(bits_per_item),
            num_items: 0,
            allocator: MaybeUninit::uninit(),
        }
    }

    #[doc(hidden)]
    fn get_allocator_internal(&'_ self) -> &'_ Arc<RwLock<BumpAllocator>> {
        // NOTE This should not be called before `set_allocator()` has been called for a newly
        // instantiated object.
        unsafe { self.allocator.assume_init_ref() }
    }

    #[inline]
    fn tag_hash(&self, item_hash: u64) -> u32 {
        let tag = (item_hash & ((1 << self.bits_per_item) - 1))
            .try_into()
            .expect("cannot fit tag into u32");

        if tag == 0 {
            return 1;
        }

        tag
    }

    #[inline]
    fn index_hash(&self, item_hash: u32) -> u32 {
        let num_buckets: u32 = (self.num_buckets - 1)
            .try_into()
            .expect("num buckets cannot fit into u32");
        item_hash & num_buckets
    }

    #[inline]
    fn hash_item<T>(&self, item: T) -> (u32, u32)
    where
        T: Hash,
    {
        let mut hasher = DefaultHasher::default();
        let item_hash: u64 = {
            item.hash(&mut hasher);
            hasher.finish()
        };
        let index_hash = self.index_hash(
            (item_hash >> 32)
                .try_into()
                .expect("cannot fit index into u32"),
        );
        let tag_hash = self.tag_hash(item_hash);

        (index_hash, tag_hash)
    }

    #[inline]
    fn alt_index(&self, index: u32, tag: u32) -> u32 {
        // Taken from the original cuckoo filter implementation:
        // https://github.com/efficient/cuckoofilter
        let tag_fingerprint: u32 = ((tag as u64 * 0x5bd1e995) & ((1 << 32) - 1))
            .try_into()
            .unwrap();
        self.index_hash(index ^ tag_fingerprint)
    }

    pub fn allocate_table(&mut self) {
        self.table.resize_to_capacity(self.num_buckets);

        let entries_per_bucket = 4;
        let allocator = Arc::clone(&self.get_allocator_internal());

        let zero_vec = vec![0; entries_per_bucket];
        for i in 0..self.num_buckets {
            self.table.buf.push(FixedPVec::new(entries_per_bucket));
            self.table.buf[i].allocate(Arc::clone(&allocator));

            // initialize entire bit array with zeroes to allow for straightforward tag
            // reading/writing.
            self.table.buf[i].copy_from_vec(&zero_vec);
        }
    }

    pub fn reset(&mut self) {
        for buf_entry in self.table.buf.iter_mut() {
            for e in buf_entry.iter_mut() {
                *e = 0;
            }
        }
    }

    fn add_internal(&mut self, index: u32, tag: u32) -> Status {
        let mut current_index = index;
        let mut current_tag = tag;

        for op_count in 0..MAX_KICK_COUNT {
            match self
                .table
                .insert_at(current_index, current_tag, op_count > 0)
            {
                BucketInsertResult::Ok => {
                    let num_items_ptr = unit_ptr_of!(&self.num_items);

                    nando_tls::add_new_pre_image(num_items_ptr, self.num_items.as_bytes());
                    self.num_items += 1;
                    nando_tls::add_new_post_image_if_changed(
                        num_items_ptr,
                        self.num_items.as_bytes(),
                    );

                    return Status::Ok;
                }
                BucketInsertResult::Replaced(old_tag) => {
                    current_tag = old_tag;
                }
                BucketInsertResult::NotInserted => {
                    // this should only happen if `op_count == 0`, in which case we will simply
                    // attempt to insert the target tag into its alternative index and then
                    // potentially proceed with cascading kickouts
                }
            }

            current_index = self.alt_index(current_index, current_tag);
        }

        Status::NoSpace
    }

    pub fn add<T>(&mut self, item: T) -> Status
    where
        T: Hash,
    {
        let (index, tag) = self.hash_item(item);
        self.add_internal(index, tag)
    }

    pub fn contains<T>(&self, item: T) -> bool
    where
        T: Hash,
    {
        let (index, tag) = self.hash_item(item);
        let alt_index = self.alt_index(index, tag);

        // sanity check
        assert_eq!(index, self.alt_index(alt_index, tag));

        self.table.bucket_contains_tag(index, tag) || self.table.bucket_contains_tag(alt_index, tag)
    }

    pub fn delete(&mut self, item: u32) -> Status {
        let (index, tag) = self.hash_item(item);
        let alt_index = self.alt_index(index, tag);

        if !(self.table.delete_tag_from_bucket(index, tag)
            || self.table.delete_tag_from_bucket(alt_index, tag))
        {
            return Status::NotFound;
        }

        // Deletion was successful
        let num_items_ptr = unit_ptr_of!(&self.num_items);

        nando_tls::add_new_pre_image(num_items_ptr, self.num_items.as_bytes());
        self.num_items -= 1;
        nando_tls::add_new_post_image_if_changed(num_items_ptr, self.num_items.as_bytes());

        Status::Ok
    }

    pub fn load_factor(&self) -> f64 {
        let table_size_num_tags = self.table.size_as_num_tags() as f64;
        1.0 * self.num_items as f64 / table_size_num_tags
    }
}

impl PersistentlyAllocatable for PCuckooFilter {
    fn set_allocator(&mut self, allocator: Arc<RwLock<BumpAllocator>>) {
        self.allocator.write(Arc::clone(&allocator));
        self.table.set_allocator(allocator);
    }

    fn get_allocator(&self) -> Option<Arc<RwLock<BumpAllocator>>> {
        Some(Arc::clone(self.get_allocator_internal()))
    }
}
