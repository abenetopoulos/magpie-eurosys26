use std::collections::HashMap;

use crate::activation_intent::NandoActivationIntent;

use crate::iptr::IPtr;
use crate::{ObjectId, ObjectVersion, TxnId};

const IMAGE_VALUE_SIZE_BYTES: usize = 128;
#[derive(Clone, Debug)]
pub struct ImageValue {
    pub data: Vec<u8>,
}

impl ImageValue {
    pub fn from_bytes(bytes: &[u8]) -> Self {
        Self {
            data: bytes.to_vec(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    pub fn len(&self) -> usize {
        self.data.len()
    }
}

impl Default for ImageValue {
    fn default() -> Self {
        Self {
            data: Vec::with_capacity(IMAGE_VALUE_SIZE_BYTES),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Image {
    field: IPtr,
    pre_value: ImageValue,
    post_value: ImageValue,
}

impl Image {
    pub fn new(iptr: IPtr) -> Self {
        Self {
            field: iptr,
            pre_value: ImageValue::default(),
            post_value: ImageValue::default(),
        }
    }

    pub fn with_pre_from_bytes(iptr: &IPtr, bytes: &[u8]) -> Self {
        let mut instance = Self {
            field: *iptr,
            pre_value: ImageValue::from_bytes(bytes),
            post_value: ImageValue::default(),
        };
        instance.post_value.data.resize(instance.pre_value.len(), 0);

        instance
    }

    pub fn set_pre_value(&mut self, bytes: &[u8]) {
        let image_len = bytes.len();
        if image_len > IMAGE_VALUE_SIZE_BYTES {
            panic!("source bytes exceed pre-image capacity");
        }

        self.pre_value.data.extend_from_slice(bytes);
        self.post_value.data.resize(self.pre_value.data.len(), 0);
    }

    fn resize_post_buffer(&mut self, for_iptr: &IPtr) -> usize {
        let start_offset = (for_iptr.offset - self.field.offset) as usize;
        let size = for_iptr.size as usize;

        if start_offset + size > self.post_value.data.len() {
            let extra_len = start_offset + size - self.post_value.data.len();
            self.post_value
                .data
                .resize(self.post_value.data.len() + extra_len, 0);

            return extra_len;
        }

        0
    }

    fn resize_pre_buffer(&mut self, for_iptr: &IPtr) -> usize {
        let start_offset = (for_iptr.offset - self.field.offset) as usize;
        let size = for_iptr.size as usize;

        if start_offset + size > self.pre_value.data.len() {
            let extra_len = start_offset + size - self.pre_value.data.len();
            self.pre_value
                .data
                .resize(self.pre_value.data.len() + extra_len, 0);

            return extra_len;
        }

        0
    }

    pub fn extend_pre_value(&mut self, iptr: &IPtr, bytes: &[u8]) {
        if self.field.offset == iptr.offset && self.field.size == iptr.size {
            return;
        }

        if (self.field.offset..self.field.offset + self.field.size).contains(&iptr.offset) {
            let extra_len = self.resize_pre_buffer(iptr);
            let start_offset = (iptr.offset - self.field.offset) as usize;
            let size = iptr.size as usize;
            self.pre_value.data[start_offset..(start_offset + size)].copy_from_slice(bytes);

            if self.field.size < self.pre_value.data.len() as u64 {
                self.field.size += extra_len as u64;
            }

            return;
        }

        self.field.size += iptr.size;
        self.pre_value.data.extend_from_slice(bytes);
    }

    pub fn set_post_value(&mut self, bytes: &[u8]) {
        let image_len = bytes.len();
        if image_len > IMAGE_VALUE_SIZE_BYTES {
            panic!("source bytes exceed post-image capacity");
        }

        if self.pre_value.data == bytes {
            return;
        }

        self.post_value.data.extend_from_slice(bytes);
    }

    pub fn update_post_value(&mut self, iptr: &IPtr, bytes: &[u8]) {
        if self.field.offset == iptr.offset && self.field.size == iptr.size {
            // simple value change, just copy the new value
            self.post_value.data[0..iptr.size as usize].copy_from_slice(bytes);
            return;
        }

        // need to extend the current image to cover the new adjacent value's memory
        if (self.field.offset..self.field.offset + self.field.size).contains(&iptr.offset) {
            let extra_len = self.resize_post_buffer(iptr);
            let start_offset = (iptr.offset - self.field.offset) as usize;
            let size = iptr.size as usize;
            self.post_value.data[start_offset..(start_offset + size)].copy_from_slice(bytes);

            if self.field.size < self.post_value.data.len() as u64 {
                self.field.size += extra_len as u64;
            }

            return;
        }

        self.field.size += iptr.size;
        self.post_value.data.extend_from_slice(bytes);
    }

    pub fn as_byte_array(&self) -> Vec<u8> {
        let field_byte_array = &self.field.as_byte_array();
        let mut res = Vec::with_capacity(
            field_byte_array.len() + self.pre_value.data.len() + self.post_value.data.len(),
        );
        res.extend_from_slice(field_byte_array);
        res.extend_from_slice(&self.pre_value.data);
        res.extend_from_slice(&self.post_value.data);

        res
    }

    pub fn get_field(&self) -> IPtr {
        self.field
    }

    pub fn get_pre_value(&self) -> &ImageValue {
        &self.pre_value
    }

    pub fn get_post_value(&self) -> &ImageValue {
        &self.post_value
    }
}

impl Default for Image {
    fn default() -> Self {
        Self {
            field: IPtr::default(),
            pre_value: ImageValue::default(),
            post_value: ImageValue::default(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ObjectVersionPair {
    object_id: ObjectId,
    version: ObjectVersion,
}

impl ObjectVersionPair {
    pub fn as_byte_array(&self) -> [u8; 32] {
        let mut res = [0; 32];
        res[0..16].copy_from_slice(&self.object_id.to_ne_bytes());
        // FIXME this should be of the same size as the size of ObjectVersion
        res[16..24].copy_from_slice(&self.version.to_ne_bytes());

        res
    }

    pub fn get_id(&self) -> ObjectId {
        self.object_id
    }

    pub fn get_version(&self) -> ObjectVersion {
        self.version
    }
}

pub enum LogEntryType {}

#[derive(Clone, Debug)]
pub struct TransactionLogEntry {
    pub txn_id: TxnId,
    pub images: HashMap<ObjectId, Vec<Image>>,
    pub read_set: Vec<ObjectVersionPair>,
    // Second element of the tuple tells us if the corresponding object was just allocated. A bit
    // wasteful space-wise (for transactions with large write sets that don't allocate objects),
    // but will do for now.
    pub write_set: Vec<(ObjectVersionPair, bool)>,

    pub current_namespace: String,
    pub pending_intents: Vec<(NandoActivationIntent, Option<usize>)>,
    pub continuation_intent: Option<NandoActivationIntent>,
    // TODO
    // timestamp: PersistableTimestamp,
}

fn iptr_offsets_overlap(iptr_1: &IPtr, iptr_2: &IPtr) -> std::cmp::Ordering {
    if iptr_1.offset == iptr_2.offset {
        return std::cmp::Ordering::Equal;
    }

    if (iptr_1.offset..(iptr_1.offset + iptr_1.size)).contains(&iptr_2.offset) {
        return std::cmp::Ordering::Equal;
    }

    iptr_1.offset.cmp(&iptr_2.offset)
}

fn iptr_extends(iptr_1: &IPtr, iptr_2: &IPtr) -> std::cmp::Ordering {
    if iptr_1.offset + iptr_1.size == iptr_2.offset {
        return std::cmp::Ordering::Equal;
    }

    iptr_1.offset.cmp(&iptr_2.offset)
}

impl TransactionLogEntry {
    pub fn new(txn_id: TxnId, num_arguments: Option<u8>) -> Self {
        Self {
            txn_id,
            images: HashMap::with_capacity(match num_arguments {
                Some(na) => na.into(),
                None => 8,
            }),
            read_set: vec![],
            write_set: vec![],
            current_namespace: String::default(),
            pending_intents: vec![],
            continuation_intent: None,
        }
    }

    pub fn add_new_pre_image(&mut self, iptr: &IPtr, bytes: &[u8]) -> () {
        if bytes.len() > IMAGE_VALUE_SIZE_BYTES {
            panic!(
                "source bytes exceed pre-image capacity ({} vs {})",
                bytes.len(),
                IMAGE_VALUE_SIZE_BYTES
            );
        }

        let object_id = iptr.object_id;

        // NOTE we only want to insert a new Image entry with an associated pre-image if we have
        // not yet encountered this specific field during this transaction.
        self.images
            .entry(object_id)
            .and_modify(|image_vec| {
                let idx = image_vec.binary_search_by(|i| iptr_offsets_overlap(&i.field, iptr));

                match idx {
                    Ok(i) => {
                        let image = image_vec.get_mut(i).unwrap();
                        image.extend_pre_value(iptr, &bytes);
                    }
                    Err(insertion_idx) => {
                        match image_vec.binary_search_by(|i| iptr_extends(&i.field, iptr)) {
                            Err(_) => image_vec
                                .insert(insertion_idx, Image::with_pre_from_bytes(iptr, bytes)),
                            Ok(i) => {
                                let image = image_vec.get_mut(i).unwrap();
                                image.extend_pre_value(iptr, &bytes);
                            }
                        }
                    }
                }
            })
            .or_insert_with(|| {
                let mut image = Image::new(*iptr);
                image.set_pre_value(bytes);

                vec![image]
            });
    }

    pub fn add_new_post_image_if_changed(&mut self, iptr: &IPtr, bytes: &[u8]) -> () {
        let object_id = iptr.object_id;
        if !self.images.contains_key(&object_id) {
            return;
        }
        self.images.entry(object_id).and_modify(|image_vec| {
            let idx = image_vec.binary_search_by(|i| iptr_offsets_overlap(&i.field, iptr));
            match idx {
                Ok(idx) => {
                    let image = image_vec
                        .get_mut(idx)
                        .expect("failed to retrieve after successful binary search");
                    image.update_post_value(iptr, bytes);
                }
                Err(e) => match image_vec.binary_search_by(|i| iptr_extends(&i.field, iptr)) {
                    Ok(i) => {
                        let image = image_vec
                            .get_mut(i)
                            .expect("failed to retrieve after successful binary search");
                        image.update_post_value(iptr, bytes);
                    }
                    Err(_) => eprintln!("ignoring post value of {:?}: {}", iptr, e),
                },
            };
        });
    }

    pub fn add_object_to_read_set(&mut self, object_id: ObjectId, version: ObjectVersion) {
        self.read_set.push(ObjectVersionPair { object_id, version });
    }

    pub fn add_object_to_write_set(&mut self, object_id: ObjectId, version: ObjectVersion) {
        self.write_set
            .push((ObjectVersionPair { object_id, version }, version == 0));
    }

    pub fn object_was_modified(&self, object_id: ObjectId) -> bool {
        self.images.contains_key(&object_id)
    }
}
