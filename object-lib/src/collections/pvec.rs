//! A persistable dynamic array type, which is safely movable between runtime instances.
use std::alloc::{Allocator, Layout};
use std::cmp::Ordering;
use std::marker::PhantomData;
use std::mem::MaybeUninit;
use std::ops::{Index, IndexMut, Range};
use std::ptr::copy_nonoverlapping;
use std::ptr::NonNull;
use std::sync::Arc;

use parking_lot::RwLock;

use crate::allocators::bump_allocator::BumpAllocator;
use crate::allocators::persistently_allocatable::PersistentlyAllocatable;
use crate::tls as nando_tls;
use crate::Persistable;

#[repr(C)]
#[derive(Debug)]
pub enum SectionMarker {
    SectionBegin {
        section_len: u32,
        section_stride: u32,
        section_offset: u32,
        end_offset: u32,
    },
    SectionEnd {
        next_section_offset: u32,
    },
}

impl Persistable for SectionMarker {}

macro_rules! get_first_section_marker {
    ($vec:expr) => {
        unsafe {
            std::ptr::addr_of!($vec.buf_offset)
                .byte_offset($vec.buf_offset.try_into().unwrap())
                .cast_mut()
                .cast::<SectionMarker>()
        }
    };
}

#[allow(dead_code)]
#[repr(C)]
/// A persistable dynamic array type, which is safely movable between runtime instances.
///
/// This vector implementation trades off _some_ complexity and space to eliminate internal
/// fragmentation within the object it is stored in. More specifically, it models its buffer as
/// (essentially) a linked list of sections (i.e. fixed-length arrays) of type `T`. See the
/// corresponding [book
/// chapter](https://magpie-book.scarcecomputing.com/design-docs/persistable-structures/vectors.html)
/// for more details.
pub struct PVec<T> {
    #[doc(hidden)]
    len: usize,
    #[doc(hidden)]
    capacity: usize,
    #[doc(hidden)]
    buf: PhantomData<T>,
    // NOTE this is an offset (in bytes) to the first allocated section within the underlying
    // storage
    #[doc(hidden)]
    buf_offset: isize,

    #[doc(hidden)]
    allocator: MaybeUninit<Arc<RwLock<BumpAllocator>>>,
}

impl<T> Persistable for PVec<T>
where
    T: Persistable,
{
    // TODO verify pointer math
    fn adjust_from(&mut self, other: &Self) {
        let buf_offset_addr = std::ptr::addr_of!((*other).buf_offset);
        let buf_offset = unsafe {
            let buf_start = buf_offset_addr
                .byte_offset((*other).buf_offset)
                .cast_mut()
                .cast::<T>();

            let base = std::ptr::addr_of!(self.buf_offset) as *const () as isize;
            let buf_start = buf_start as isize;
            buf_start - base
        };

        let buf_offset_ptr = crate::unit_ptr_of!(&self.buf_offset);
        nando_tls::add_new_pre_image(buf_offset_ptr as *const (), self.buf_offset.as_bytes());
        self.buf_offset = buf_offset;
        nando_tls::add_new_post_image_if_changed(
            buf_offset_ptr as *const (),
            self.buf_offset.as_bytes(),
        );
    }
}

impl<T> PVec<T>
where
    T: Persistable,
{
    pub fn new() -> Self {
        Self {
            len: 0,
            capacity: 0,
            buf: PhantomData,
            buf_offset: 0,
            allocator: MaybeUninit::uninit(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn get_allocator_internal(&'_ self) -> &'_ Arc<RwLock<BumpAllocator>> {
        unsafe { self.allocator.assume_init_ref() }
    }

    #[doc(hidden)]
    unsafe fn allocate_new_section_with_capacity(&self, capacity: usize) -> NonNull<[u8]> {
        let allocator = self.get_allocator_internal();

        // NOTE we add +2 to the slice for the section bookends
        let section_marker_layout = Layout::from_size_align(
            std::mem::size_of::<SectionMarker>(),
            std::mem::align_of::<SectionMarker>(),
        )
        .unwrap();
        let entry_layout = Layout::array::<T>(capacity).unwrap();
        let allocator = allocator.read();
        let first_slot_ptr = allocator
            .allocate(section_marker_layout)
            .expect("failed to allocate new SectionBegin marker");
        let entry_allocation = allocator
            .allocate(entry_layout)
            .expect("failed to allocate space for entries");
        let last_slot_ptr = allocator
            .allocate(section_marker_layout)
            .expect("failed to allocate new SectionEnd marker");

        // Add section bookends.
        let first_slot = first_slot_ptr.as_ptr().cast::<SectionMarker>();
        let section_offset =
            (entry_allocation.as_ptr() as *const () as usize) - (first_slot as *const () as usize);
        let last_slot_offset =
            (last_slot_ptr.as_ptr() as *const () as usize) - (first_slot as *const () as usize);
        nando_tls::add_new_pre_image(first_slot as *const (), (*first_slot).as_bytes());
        *first_slot = SectionMarker::SectionBegin {
            section_len: capacity.try_into().unwrap(),
            section_stride: std::mem::size_of::<T>().try_into().unwrap(),
            section_offset: section_offset.try_into().unwrap(),
            end_offset: last_slot_offset.try_into().unwrap(),
        };
        nando_tls::add_new_post_image_if_changed(first_slot as *const (), (*first_slot).as_bytes());
        let last_slot = last_slot_ptr.as_ptr().cast::<SectionMarker>();

        nando_tls::add_new_pre_image(last_slot as *const (), (*last_slot).as_bytes());
        *last_slot = SectionMarker::SectionEnd {
            // NOTE this should ideally be an option, but I'm lazy.
            next_section_offset: 0,
        };
        nando_tls::add_new_post_image_if_changed(last_slot as *const (), (*last_slot).as_bytes());

        first_slot_ptr
    }

    /// This call will set the _initial_ capacity of the vector. This should be called with an
    /// argument that is (roughly) of the same order-of-magnitude as the expected number of
    /// elements this vector is expected to hold at its largest, to minimize the number of sections
    /// that will have to be allocated.
    pub fn resize_to_capacity(&mut self, capacity: usize) {
        if self.capacity != 0 {
            return;
        }

        let buf_offset: isize = {
            let new_region_start = unsafe { self.allocate_new_section_with_capacity(capacity) };
            let base = std::ptr::addr_of!(self.buf_offset) as *const ();
            ((new_region_start.as_ptr() as *const () as usize) - (base as usize))
                .try_into()
                .unwrap()
        };

        let capacity_ptr = crate::unit_ptr_of!(&self.capacity);
        nando_tls::add_new_pre_image(capacity_ptr, self.capacity.as_bytes());
        self.capacity = capacity;
        nando_tls::add_new_post_image_if_changed(capacity_ptr, self.capacity.as_bytes());

        let buf_offset_ptr = crate::unit_ptr_of!(&self.buf_offset);
        nando_tls::add_new_pre_image(buf_offset_ptr, self.buf_offset.as_bytes());
        self.buf_offset = buf_offset as isize;
        nando_tls::add_new_post_image_if_changed(buf_offset_ptr, self.buf_offset.as_bytes());
    }

    /// Appends an element to the end of the vector, potentially allocating more space if the
    /// current capacity has already been exhausted.
    pub fn push(&mut self, e: T) -> *mut T {
        let value = e;
        let ptr = unsafe {
            // NOTE this should always be safe to cast, as it should be the first section's
            // SectionBegin.
            let mut section_ptr = std::ptr::addr_of!(self.buf_offset)
                .byte_offset(self.buf_offset.try_into().unwrap())
                .cast_mut()
                .cast::<SectionMarker>();
            let mut remaining_len = self.len() + 1;

            // NOTE ideally we should be leveraging [`self.get_element_at_index`] here, which
            // should _probably_ be rewritten to accept a flag that dictates whether or not we call
            // extend once we've reached the end of the last allocated section (up to now), but for
            // now I'm not going to bother with that because it does not feel all that clean.
            let current_ptr = loop {
                if let SectionMarker::SectionBegin {
                    section_len,
                    section_stride,
                    section_offset,
                    end_offset,
                } = *section_ptr
                {
                    if remaining_len <= section_len as usize {
                        break section_ptr
                            .byte_offset(
                                (section_offset as usize
                                    + (remaining_len - 1) * section_stride as usize)
                                    .try_into()
                                    .unwrap(),
                            )
                            .cast::<T>();
                    }

                    let section_end_ptr = section_ptr.byte_offset((end_offset).try_into().unwrap());
                    if let SectionMarker::SectionEnd {
                        next_section_offset,
                    } = *section_end_ptr.cast::<SectionMarker>()
                    {
                        let offset = if next_section_offset == 0 {
                            self.extend(section_end_ptr)
                        } else {
                            next_section_offset
                        };

                        remaining_len -= section_len as usize;
                        section_ptr = section_end_ptr.byte_offset(offset.try_into().unwrap());
                    } else {
                        panic!(
                            "attempt to access invalid index {} of section with capacity {}: {:?}",
                            remaining_len, section_len, *section_ptr,
                        );
                    }
                }
            };

            nando_tls::add_new_pre_image(current_ptr as *const (), (&*current_ptr).as_bytes());
            *current_ptr = value;
            nando_tls::add_new_post_image_if_changed(
                current_ptr as *const (),
                (&*current_ptr).as_bytes(),
            );
            current_ptr
        };

        let len_ptr = crate::unit_ptr_of!(&self.len);
        nando_tls::add_new_pre_image(len_ptr, self.len.as_bytes());
        self.len += 1;
        nando_tls::add_new_post_image_if_changed(len_ptr, self.len.as_bytes());
        ptr
    }

    /// Returns the last element from the vector (if not empty).
    pub fn pop(&mut self) -> Option<T> {
        if self.len() == 0 {
            return None;
        }

        unsafe {
            let e = self.get_element_at_index(self.len - 1);
            let len_ptr = crate::unit_ptr_of!(&self.len);
            nando_tls::add_new_pre_image(len_ptr, self.len.as_bytes());
            self.len -= 1;
            nando_tls::add_new_post_image_if_changed(len_ptr, self.len.as_bytes());

            Some(std::ptr::read(e))
        }
    }

    pub fn push_at(&mut self, e: T, idx: usize) {
        // FIXME replace assert, return type should be result or something
        assert!(self.len() > idx);
        let value = e;

        unsafe {
            let e = self.get_element_at_index(idx);
            nando_tls::add_new_pre_image(e as *const (), (*e).as_bytes());
            *e = value;
            nando_tls::add_new_post_image_if_changed(e as *const (), (*e).as_bytes());
        }
    }

    pub fn last(&self) -> Option<&mut T> {
        if self.len() == 0 {
            return None;
        }

        unsafe {
            let e = self.get_element_at_index(self.len() - 1);

            e.as_mut()
        }
    }

    pub fn get_ref(&self, idx: usize) -> Option<*const T> {
        if self.len() <= idx {
            return None;
        }

        unsafe {
            let e = self.get_element_at_index(idx);

            Some(e)
        }
    }

    // NOTE @experimental To be used only by the hashmap for its backing storage
    #[doc(hidden)]
    pub fn pop_at(&mut self, idx: usize) -> Option<T> {
        if self.len() <= idx {
            return None;
        }

        unsafe {
            let e = self.get_element_at_index(idx);

            // we cannot decrement the length of the vector because that would affect the
            // correctness of `push()`. Instead, it is the responsibility of the **caller** to
            // maintain a list of open spaces in the vector, and to insert elements in them by
            // calling `push_at()`

            Some(std::ptr::read(e))
        }
    }

    /// Shortens the vector, keeping the first `len` elements and dropping the rest.
    ///
    /// The allocated space of the vector remains unaffected.
    pub fn truncate(&mut self, len: usize) {
        if self.len() < len {
            return;
        }

        // We maintain our allocated space (and therefore capacity) but only reset the number of
        // elements currently in the vector so that calls to `push()` will start overwriting
        // existing entries.
        let len_ptr = crate::unit_ptr_of!(&self.len);
        nando_tls::add_new_pre_image(len_ptr, self.len.as_bytes());
        self.len = len;
        nando_tls::add_new_post_image_if_changed(len_ptr, self.len.as_bytes());
    }

    /// Returns the number of elements in the vector.
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn find<P>(&self, predicate: P) -> Option<usize>
    where
        P: Fn(&T) -> bool,
    {
        for (idx, e) in self.iter().enumerate() {
            if predicate(e) {
                return Some(idx);
            }
        }

        None
    }

    #[doc(hidden)]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    #[doc(hidden)]
    fn extend_to_capacity(
        &mut self,
        previous_section_end: *mut SectionMarker,
        capacity: usize,
    ) -> u32 {
        let capacity_ptr = crate::unit_ptr_of!(&self.capacity);
        nando_tls::add_new_pre_image(capacity_ptr, self.capacity.as_bytes());
        self.capacity += capacity;
        nando_tls::add_new_post_image_if_changed(capacity_ptr, self.capacity.as_bytes());

        let new_section_start = unsafe { self.allocate_new_section_with_capacity(capacity) };
        let base = previous_section_end;
        let offset: u32 = (new_section_start.as_ptr() as *const () as usize
            - base as *const () as usize)
            .try_into()
            .unwrap();
        unsafe {
            nando_tls::add_new_pre_image(
                previous_section_end as *const (),
                (*previous_section_end).as_bytes(),
            );
            *previous_section_end = SectionMarker::SectionEnd {
                next_section_offset: offset,
            };
            nando_tls::add_new_post_image_if_changed(
                previous_section_end as *const (),
                (*previous_section_end).as_bytes(),
            );
        }

        offset
    }

    #[doc(hidden)]
    fn extend(&mut self, previous_section_end: *mut SectionMarker) -> u32 {
        let capacity = self.capacity() * 2;
        self.extend_to_capacity(previous_section_end, capacity)
    }

    #[doc(hidden)]
    unsafe fn get_element_at_index(&self, idx: usize) -> *mut T {
        if idx >= self.len() {
            panic!("index {} out of bounds (len is {})", idx, self.len());
        }

        let mut section_ptr = std::ptr::addr_of!(self.buf_offset)
            .byte_offset(self.buf_offset.try_into().unwrap())
            .cast_mut()
            .cast::<SectionMarker>();
        // NOTE skipping over the initial `SectionBegin` entry.
        let mut remaining_len = idx + 1;

        let current_ptr = loop {
            if let SectionMarker::SectionBegin {
                section_len,
                section_stride,
                section_offset,
                end_offset,
            } = *section_ptr
            {
                if remaining_len <= section_len as usize {
                    break section_ptr
                        .byte_offset(
                            (section_offset as usize
                                + (remaining_len - 1) * section_stride as usize)
                                .try_into()
                                .unwrap(),
                        )
                        .cast::<T>();
                }

                let section_end_ptr = section_ptr.byte_offset((end_offset).try_into().unwrap());
                if let SectionMarker::SectionEnd {
                    next_section_offset,
                } = *section_end_ptr.cast::<SectionMarker>()
                {
                    remaining_len -= section_len as usize;
                    section_ptr =
                        section_end_ptr.byte_offset(next_section_offset.try_into().unwrap());
                } else {
                    panic!(
                        "attempt to access invalid index {} of section with capacity {}",
                        remaining_len, section_len
                    );
                }
            }
        };

        current_ptr
    }

    pub fn iter(&self) -> PVecIter<T> {
        let initial_section_ptr = get_first_section_marker!(self);
        PVecIter {
            current_idx: 0,
            pvec: self,
            section_start: initial_section_ptr,
            len_before_section_start: 0,
        }
    }

    pub fn iter_mut(&mut self) -> PVecIterMut<T> {
        let initial_section_ptr = get_first_section_marker!(self);
        PVecIterMut {
            current_idx: 0,
            pvec: self,
            section_start: initial_section_ptr,
            len_before_section_start: 0,
        }
    }

    pub fn buf_offset(&self) -> isize {
        self.buf_offset
    }

    pub fn get_slice<'a>(&'a self, slice_range: Range<usize>) -> SegmentedSlice<'a, T> {
        match slice_range.end > self.len() {
            false => {}
            true => panic!("end index is {} but len is {}", slice_range.end, self.len()),
        }

        SegmentedSlice {
            start_idx: slice_range.start,
            end_idx: slice_range.end,
            underlying_vec: self,
        }
    }

    unsafe fn get_section_marker_for_idx(&self, idx: usize) -> (*const SectionMarker, usize) {
        if idx >= self.capacity() {
            panic!(
                "index {} out of bounds (capacity is {})",
                idx,
                self.capacity()
            );
        }

        let mut section_start_idx: usize = 0;
        let mut section_ptr = std::ptr::addr_of!(self.buf_offset)
            .byte_offset(self.buf_offset.try_into().unwrap())
            .cast_mut()
            .cast::<SectionMarker>();

        // NOTE skipping over the initial `SectionBegin` entry.
        let mut remaining_len = idx + 1;

        loop {
            if let SectionMarker::SectionBegin {
                section_len,
                end_offset,
                ..
            } = *section_ptr
            {
                if remaining_len <= section_len as usize {
                    break;
                }

                section_start_idx += section_len as usize;

                let section_end_ptr = section_ptr.byte_offset((end_offset).try_into().unwrap());
                if let SectionMarker::SectionEnd {
                    next_section_offset,
                } = *section_end_ptr.cast::<SectionMarker>()
                {
                    remaining_len -= section_len as usize;
                    section_ptr =
                        section_end_ptr.byte_offset(next_section_offset.try_into().unwrap());
                } else {
                    panic!(
                        "attempt to access invalid index {} of section with capacity {}",
                        remaining_len, section_len
                    );
                }
            }
        }

        if section_start_idx == 0 {
            return (section_ptr.as_ref().unwrap(), 0);
        }

        (section_ptr as *const SectionMarker, section_start_idx - 1)
    }

    unsafe fn get_section_end_marker(
        &mut self,
        marker: *const SectionMarker,
    ) -> *mut SectionMarker {
        if let SectionMarker::SectionBegin { end_offset, .. } = *marker {
            let section_end_ptr = marker.byte_offset((end_offset).try_into().unwrap());
            if let SectionMarker::SectionEnd { .. } = *section_end_ptr.cast::<SectionMarker>() {
                return section_end_ptr.cast_mut().cast::<SectionMarker>();
            } else {
                unreachable!("could not find section end");
            }
        } else {
            unreachable!("was not passed section start");
        }
    }

    unsafe fn is_last_section(&self, marker: *const SectionMarker) -> bool {
        if let SectionMarker::SectionBegin { end_offset, .. } = *marker {
            let section_end_ptr = marker.byte_offset((end_offset).try_into().unwrap());
            if let SectionMarker::SectionEnd {
                next_section_offset,
            } = *section_end_ptr.cast::<SectionMarker>()
            {
                if next_section_offset == 0 {
                    return true;
                }

                return false;
            } else {
                unreachable!("attempted to get next section but could not find section end");
            }
        } else {
            unreachable!("attempted to get next section but was not passed section start");
        }
    }

    unsafe fn get_next_section_marker(
        &self,
        marker: *const SectionMarker,
    ) -> Option<*const SectionMarker> {
        if let SectionMarker::SectionBegin { end_offset, .. } = *marker {
            let section_end_ptr = marker.byte_offset((end_offset).try_into().unwrap());
            if let SectionMarker::SectionEnd {
                next_section_offset,
            } = *section_end_ptr.cast::<SectionMarker>()
            {
                if next_section_offset == 0 {
                    return None;
                }

                return Some(section_end_ptr.byte_offset(next_section_offset.try_into().unwrap()));
            } else {
                unreachable!("attempted to get next section but could not find section end");
            }
        } else {
            unreachable!("attempted to get next section but was not passed section start");
        }
    }

    fn elements_in_same_segment(&self, idx_1: usize, idx_2: usize) -> bool {
        unsafe {
            let (segment_header, section_starting_idx) = self.get_section_marker_for_idx(idx_1);
            let SectionMarker::SectionBegin { section_len, .. } = *segment_header else {
                panic!("failed to get section marker");
            };

            idx_2 - section_starting_idx <= section_len as usize
        }
    }

    pub unsafe fn get_slice_as_ptr_range<'a>(
        &'a self,
        slice_range: Range<usize>,
    ) -> Result<Range<*const T>, usize> {
        let start = slice_range.start;
        let end = match slice_range.end > self.len() {
            false => slice_range.end,
            true => self.len(),
        };

        if !self.elements_in_same_segment(start, end - 1) {
            let (segment_header, section_starting_idx) = self.get_section_marker_for_idx(start);
            let SectionMarker::SectionBegin { section_len, .. } = *segment_header else {
                panic!("failed to get section marker");
            };

            return Err(section_starting_idx + section_len as usize);
        }

        Ok(Range {
            start: self.get_ref(start).expect("failed to get start ptr"),
            end: self.get_ref(end - 1).expect("failed to get end ptr"),
        })
    }

    pub unsafe fn copy_from_vec(&mut self, other: &Vec<T>) -> bool {
        if self.capacity < other.capacity() {
            return false;
        }
        self.copy_from_slice(other.as_slice())
    }

    pub unsafe fn copy_from_slice(&mut self, other: &[T]) -> bool {
        if self.capacity < other.len() {
            return false;
        }

        let section_marker = get_first_section_marker!(self);
        let first_element = {
            let SectionMarker::SectionBegin { section_offset, .. } = *section_marker else {
                panic!(
                    "invalid pointer stored in section start pointer: {:?}",
                    *section_marker
                );
            };

            section_marker
                .byte_offset((section_offset as usize).try_into().unwrap())
                .cast::<T>()
                .as_mut()
                .unwrap()
        };

        nando_tls::add_new_pre_image(first_element as *const T as *const (), &[0x0]);
        copy_nonoverlapping(other.as_ptr(), first_element as *mut T, other.len());
        nando_tls::add_new_post_image_if_changed(
            first_element as *const T as *const (),
            std::slice::from_raw_parts(
                first_element as *const T as *const u8,
                other.len() * std::mem::size_of::<T>(),
            ),
        );

        let len_ptr = crate::unit_ptr_of!(&self.len);
        nando_tls::add_new_pre_image(len_ptr, self.len.as_bytes());
        self.len = other.len();
        nando_tls::add_new_post_image_if_changed(len_ptr, self.len.as_bytes());
        true
    }

    pub unsafe fn extend_from_vec(&mut self, other: &Vec<T>) -> bool {
        if (self.capacity - self.len()) < other.capacity() {
            return false;
        }

        let first_element = match self.len() {
            0 => {
                let section_marker = get_first_section_marker!(self);
                let SectionMarker::SectionBegin { section_offset, .. } = *section_marker else {
                    panic!(
                        "invalid pointer stored in section start pointer: {:?}",
                        *section_marker
                    );
                };

                section_marker
                    .byte_offset((section_offset as usize).try_into().unwrap())
                    .cast::<T>()
                    .as_mut()
                    .unwrap()
            }
            _ => {
                let target_idx = self.len() - 1;
                let (dst_section_start, dst_len_before_start) =
                    self.get_section_marker_for_idx(target_idx);
                assert!(dst_len_before_start == 0);
                let SectionMarker::SectionBegin {
                    section_offset,
                    section_stride,
                    ..
                } = *dst_section_start
                else {
                    panic!(
                        "invalid pointer stored in section start pointer: {:?}",
                        *dst_section_start
                    );
                };

                let section_offset = if target_idx > dst_len_before_start {
                    let start = match dst_len_before_start {
                        0 => 0,
                        _ => unreachable!(),
                    };
                    section_offset as usize + (target_idx - start) * section_stride as usize
                } else {
                    section_offset as usize
                };

                dst_section_start
                    .byte_offset(section_offset.try_into().unwrap())
                    .cast_mut()
                    .cast::<T>()
            }
        };

        let other_slice = other.as_slice();
        nando_tls::add_new_pre_image(first_element as *const T as *const (), &[0x0]);
        copy_nonoverlapping(
            other_slice.as_ptr(),
            first_element as *mut T,
            other_slice.len(),
        );
        nando_tls::add_new_post_image_if_changed(
            first_element as *const T as *const (),
            std::slice::from_raw_parts(
                first_element as *const T as *const u8,
                other.len() * std::mem::size_of::<T>(),
            ),
        );

        let len_ptr = crate::unit_ptr_of!(&self.len);
        nando_tls::add_new_pre_image(len_ptr, self.len.as_bytes());
        self.len = other.len();
        nando_tls::add_new_post_image_if_changed(len_ptr, self.len.as_bytes());
        true
    }

    /// Fast copy between `PVec` instances.
    ///
    /// This method tries to repeatedly memcpy the largest possible chunks of data between two
    /// `PVec` instances, potentially allocating new sections in `self`. The segmented layout of
    /// `PVec` makes this method necessary if we want to have fast copying.
    // FIXME clean up
    pub fn copy_from_pvec(
        &mut self,
        other: &PVec<T>,
        dst_start_idx: Option<usize>,
        source_start_idx: Option<usize>,
    ) -> bool {
        let source_start_idx = match source_start_idx {
            Some(idx) => idx,
            None => 0,
        };
        let mut num_elements_to_copy = other.len() - source_start_idx;

        let dst_start_idx = match dst_start_idx {
            Some(idx) => {
                if idx >= self.capacity() {
                    unsafe {
                        let (dst_last_section_start, _) =
                            self.get_section_marker_for_idx(self.len() - 1);
                        let new_capacity = self.capacity() + num_elements_to_copy;
                        let section_end_marker =
                            self.get_section_end_marker(dst_last_section_start);
                        self.extend_to_capacity(section_end_marker, new_capacity);
                    }
                }
                idx
            }
            None => 0,
        };

        let num_elements_copied = num_elements_to_copy;

        unsafe {
            let (mut src_section_start, mut src_len_before_start) =
                other.get_section_marker_for_idx(source_start_idx);
            let (mut dst_section_start, mut dst_len_before_start) =
                self.get_section_marker_for_idx(dst_start_idx);

            let mut src_current_idx = source_start_idx;
            let mut dst_current_idx = dst_start_idx;

            while num_elements_to_copy > 0 {
                // adjust read ptr
                let (first_src_element, src_chunk_size) = {
                    let SectionMarker::SectionBegin {
                        section_offset,
                        section_len,
                        section_stride,
                        ..
                    } = *src_section_start
                    else {
                        panic!(
                            "invalid pointer stored in section start pointer: {:?}",
                            *src_section_start
                        );
                    };

                    let section_len: usize = section_len as usize;
                    let section_offset = if src_current_idx > src_len_before_start {
                        let start = match src_len_before_start {
                            0 => 0,
                            _ => src_len_before_start + 1,
                        };
                        section_offset as usize
                            + (src_current_idx - start) * section_stride as usize
                    } else {
                        section_offset as usize
                    };

                    let remaining_in_section =
                        section_len - (src_current_idx - src_len_before_start);
                    let src_chunk_size = if num_elements_to_copy > remaining_in_section {
                        num_elements_to_copy -= remaining_in_section;
                        remaining_in_section
                    } else {
                        let src_chunk_size = num_elements_to_copy;
                        num_elements_to_copy = 0;
                        src_chunk_size
                    };

                    src_current_idx = src_len_before_start + section_len;
                    src_len_before_start += section_len;

                    (
                        src_section_start
                            .byte_offset(section_offset.try_into().unwrap())
                            .cast_mut()
                            .cast::<T>(),
                        src_chunk_size,
                    )
                };

                // adjust write ptr
                let (first_dst_element, dst_section_remaining_capacity) = {
                    let SectionMarker::SectionBegin {
                        section_offset,
                        section_len,
                        section_stride,
                        ..
                    } = *dst_section_start
                    else {
                        panic!(
                            "invalid pointer stored in section start pointer: {:?}",
                            *dst_section_start
                        );
                    };

                    let section_len: usize = section_len as usize;
                    let section_offset = if dst_current_idx > dst_len_before_start {
                        let start = match dst_len_before_start {
                            0 => 0,
                            _ => dst_len_before_start + 1,
                        };
                        section_offset as usize
                            + (dst_current_idx - start) * section_stride as usize
                    } else {
                        section_offset as usize
                    };

                    let remaining_capacity = section_len - (dst_current_idx - dst_len_before_start);

                    (
                        dst_section_start
                            .byte_offset(section_offset.try_into().unwrap())
                            .cast_mut()
                            .cast::<T>(),
                        remaining_capacity,
                    )
                };

                if src_chunk_size <= dst_section_remaining_capacity {
                    // case 1: source section fits, simply copy
                    nando_tls::add_new_pre_image(
                        first_dst_element as *const T as *const (),
                        &[0x0],
                    );
                    copy_nonoverlapping(
                        first_src_element as *const T,
                        first_dst_element as *mut T,
                        src_chunk_size,
                    );
                    nando_tls::add_new_post_image_if_changed(
                        first_dst_element as *const T as *const (),
                        std::slice::from_raw_parts(
                            first_dst_element as *const T as *const u8,
                            src_chunk_size * std::mem::size_of::<T>(),
                        ),
                    );
                    dst_current_idx += src_chunk_size;
                } else {
                    // case 2: source section is larger than dst section remaining capacity, copy
                    // as many as we can in this section and then allocate and copy the rest
                    nando_tls::add_new_pre_image(
                        first_dst_element as *const T as *const (),
                        &[0x0],
                    );
                    copy_nonoverlapping(
                        first_src_element as *const T,
                        first_dst_element as *mut T,
                        dst_section_remaining_capacity,
                    );
                    nando_tls::add_new_post_image_if_changed(
                        first_dst_element as *const T as *const (),
                        std::slice::from_raw_parts(
                            first_dst_element as *const T as *const u8,
                            dst_section_remaining_capacity * std::mem::size_of::<T>(),
                        ),
                    );
                    dst_current_idx += dst_section_remaining_capacity;
                    let mut src_section_remaining_elements =
                        src_chunk_size - dst_section_remaining_capacity;

                    let first_src_element = {
                        first_src_element
                            .byte_offset(
                                (dst_section_remaining_capacity * std::mem::size_of::<T>())
                                    as isize,
                            )
                            .cast::<T>()
                    };

                    if self.is_last_section(dst_section_start) {
                        let new_capacity = self.capacity()
                            + (num_elements_to_copy + src_section_remaining_elements);
                        let section_end_marker = self.get_section_end_marker(dst_section_start);
                        self.extend_to_capacity(section_end_marker, new_capacity);
                        dst_section_start =
                            self.get_next_section_marker(dst_section_start).unwrap();
                        let first_dst_element = {
                            let SectionMarker::SectionBegin { section_offset, .. } =
                                *dst_section_start
                            else {
                                panic!(
                                    "invalid pointer stored in section start pointer: {:?}",
                                    *dst_section_start
                                );
                            };
                            dst_section_start
                                .byte_offset(section_offset.try_into().unwrap())
                                .cast_mut()
                                .cast::<T>()
                        };

                        nando_tls::add_new_pre_image(
                            first_dst_element as *const T as *const (),
                            &[0x0],
                        );
                        copy_nonoverlapping(
                            first_src_element as *const T,
                            first_dst_element as *mut T,
                            src_section_remaining_elements,
                        );
                        nando_tls::add_new_post_image_if_changed(
                            first_dst_element as *const T as *const (),
                            std::slice::from_raw_parts(
                                first_dst_element as *const T as *const u8,
                                src_section_remaining_elements * std::mem::size_of::<T>(),
                            ),
                        );
                    } else {
                        let mut first_src_element = first_src_element;
                        // NOTE we might need to keep allocating if the source section has way more
                        // elements that pre-allocated dst sections, which is why we loop here until
                        // we've drained the src section
                        while src_section_remaining_elements > 0 {
                            dst_section_start = match self
                                .get_next_section_marker(dst_section_start)
                            {
                                None => {
                                    let new_capacity = self.capacity()
                                        + (num_elements_to_copy + src_section_remaining_elements);
                                    let section_end_marker =
                                        self.get_section_end_marker(dst_section_start);
                                    self.extend_to_capacity(section_end_marker, new_capacity);
                                    self.get_next_section_marker(dst_section_start).unwrap()
                                }
                                Some(s) => s,
                            };

                            let (first_dst_element, section_len) = {
                                let SectionMarker::SectionBegin {
                                    section_offset,
                                    section_len,
                                    ..
                                } = *dst_section_start
                                else {
                                    panic!(
                                        "invalid pointer stored in section start pointer: {:?}",
                                        *dst_section_start
                                    );
                                };
                                (
                                    dst_section_start
                                        .byte_offset(section_offset.try_into().unwrap())
                                        .cast_mut()
                                        .cast::<T>(),
                                    section_len as usize,
                                )
                            };

                            let bytes_to_write = if section_len <= src_section_remaining_elements {
                                src_section_remaining_elements -= section_len;
                                section_len
                            } else {
                                let remaining_elements = src_section_remaining_elements;
                                src_section_remaining_elements = 0;
                                remaining_elements
                            };

                            nando_tls::add_new_pre_image(
                                first_dst_element as *const T as *const (),
                                &[0x0],
                            );
                            copy_nonoverlapping(
                                first_src_element as *const T,
                                first_dst_element as *mut T,
                                bytes_to_write,
                            );

                            nando_tls::add_new_post_image_if_changed(
                                first_dst_element as *const T as *const (),
                                std::slice::from_raw_parts(
                                    first_dst_element as *const T as *const u8,
                                    bytes_to_write * std::mem::size_of::<T>(),
                                ),
                            );

                            first_src_element = {
                                first_src_element
                                    .byte_offset(
                                        (bytes_to_write * std::mem::size_of::<T>()) as isize,
                                    )
                                    .cast::<T>()
                            };
                        }
                    }

                    dst_current_idx += src_chunk_size - dst_section_remaining_capacity;
                    dst_len_before_start = dst_current_idx - 1;
                }

                src_section_start = match other.get_next_section_marker(src_section_start) {
                    None => break,
                    Some(s) => s,
                };
            }

            assert!(num_elements_to_copy == 0);
        }

        let len_ptr = crate::unit_ptr_of!(&self.len);
        nando_tls::add_new_pre_image(len_ptr, self.len.as_bytes());
        if dst_start_idx > 0 {
            self.len += num_elements_copied;
        } else {
            self.len = dst_start_idx + num_elements_copied;
        }
        nando_tls::add_new_post_image_if_changed(len_ptr, self.len.as_bytes());

        true
    }
}

impl<T> PersistentlyAllocatable for PVec<T>
where
    T: Persistable,
{
    /// Sets the allocator that is going to be used by the [`PVec`] instance whenever it needs to
    /// extend its internal capacity.
    ///
    /// **NOTE** This needs to be set _before_ any subsequent calls are made that would trigger an
    /// allocation.
    fn set_allocator(&mut self, allocator: Arc<RwLock<BumpAllocator>>) {
        self.allocator.write(allocator);
    }

    fn get_allocator(&self) -> Option<Arc<RwLock<BumpAllocator>>> {
        Some(Arc::clone(self.get_allocator_internal()))
    }
}

// FIXME implement for `SliceIndex`
impl<T> Index<usize> for PVec<T>
where
    T: Persistable,
{
    type Output = T;

    fn index(&self, index: usize) -> &Self::Output {
        match index > self.len() {
            false => {}
            true => panic!("index is {index} but len is {}", self.len()),
        }

        unsafe { &*self.get_element_at_index(index) }
    }
}

// FIXME implement for `SliceIndex`
impl<T> IndexMut<usize> for PVec<T>
where
    T: Persistable,
{
    fn index_mut(&mut self, index: usize) -> &mut Self::Output {
        let e = unsafe { &mut *self.get_element_at_index(index) };

        e
    }
}

impl<T> PartialEq for PVec<T>
where
    T: Persistable + PartialEq,
{
    fn eq(&self, other: &Self) -> bool {
        if self.len() != other.len() {
            return false;
        }

        for i in 0..self.len() {
            if self[i] != other[i] {
                return false;
            }
        }

        true
    }
}

impl<T> Eq for PVec<T> where T: Persistable + Ord {}

impl<T> PartialOrd for PVec<T>
where
    T: PartialOrd + Persistable,
{
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        let own_length = self.len();
        let other_length = other.len();

        let max_length = std::cmp::max(own_length, other_length);
        for i in 0..max_length {
            if i >= own_length {
                return Some(Ordering::Less);
            }

            if i >= other_length {
                return Some(Ordering::Greater);
            }

            match PartialOrd::partial_cmp(&self[i], &other[i]) {
                Some(Ordering::Equal) => (),
                o @ _ => return o,
            }
        }

        None
    }
}

impl<T> Ord for PVec<T>
where
    T: Persistable + Ord,
{
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Ord::cmp(&self, &other)
        PartialOrd::partial_cmp(&self, &other).expect("failed to compare pvecs")
    }
}

#[derive(Clone)]
pub struct PVecIter<'a, T> {
    current_idx: usize,
    pvec: &'a PVec<T>,
    section_start: *const SectionMarker,
    len_before_section_start: usize,
}

impl<'a, T> PVecIter<'a, T> {
    fn bump_index(&mut self) -> usize {
        let idx = self.current_idx;
        self.current_idx += 1;

        idx
    }
}

impl<'a, T> Iterator for PVecIter<'a, T>
where
    T: Persistable,
{
    type Item = &'a T;

    fn next(&mut self) -> Option<Self::Item> {
        let idx = self.bump_index();
        if idx >= self.pvec.len() {
            return None;
        }

        unsafe {
            loop {
                let SectionMarker::SectionBegin {
                    section_len,
                    section_offset,
                    section_stride,
                    ..
                } = *self.section_start
                else {
                    panic!(
                        "invalid pointer stored in section start pointer: {:?}",
                        *self.section_start
                    );
                };

                let adjusted_idx = match self.len_before_section_start {
                    0 => idx,
                    // _ => idx - self.len_before_section_start - 1,
                    _ => idx - self.len_before_section_start,
                };

                if adjusted_idx < section_len as usize {
                    return self
                        .section_start
                        .byte_offset(
                            (section_offset as usize + adjusted_idx * section_stride as usize)
                                .try_into()
                                .unwrap(),
                        )
                        .cast::<T>()
                        .as_ref();
                }

                self.len_before_section_start += section_len as usize;
                let new_section_start = self
                    .pvec
                    .get_next_section_marker(self.section_start)
                    .expect("failed to get next section");
                self.section_start = new_section_start;
            }
        }
    }
}

pub struct PVecIterMut<'a, T> {
    current_idx: usize,
    pvec: &'a mut PVec<T>,
    section_start: *const SectionMarker,
    len_before_section_start: usize,
}

impl<'a, T> PVecIterMut<'a, T> {
    fn bump_index(&mut self) -> usize {
        let idx = self.current_idx;
        self.current_idx += 1;

        idx
    }
}

impl<'a, T> Iterator for PVecIterMut<'a, T>
where
    T: Persistable,
{
    type Item = &'a mut T;

    fn next(&mut self) -> Option<Self::Item> {
        let idx = self.bump_index();
        if idx >= self.pvec.len() {
            return None;
        }

        unsafe {
            loop {
                let SectionMarker::SectionBegin {
                    section_len,
                    section_offset,
                    section_stride,
                    ..
                } = *self.section_start
                else {
                    panic!(
                        "invalid pointer stored in section start pointer: {:?}",
                        *self.section_start
                    );
                };

                let adjusted_idx = match self.len_before_section_start {
                    0 => idx,
                    // _ => idx - self.len_before_section_start - 1,
                    _ => idx - self.len_before_section_start,
                };

                if adjusted_idx < section_len as usize {
                    return self
                        .section_start
                        .byte_offset(
                            (section_offset as usize + adjusted_idx * section_stride as usize)
                                .try_into()
                                .unwrap(),
                        )
                        .cast_mut()
                        .cast::<T>()
                        .as_mut();
                }

                self.len_before_section_start += section_len as usize;
                let new_section_start = self
                    .pvec
                    .get_next_section_marker(self.section_start)
                    .expect("failed to get next section");
                self.section_start = new_section_start;
            }
        }
    }
}

impl<'a, T> IntoIterator for &'a PVec<T>
where
    T: Persistable,
{
    type Item = &'a T;

    type IntoIter = PVecIter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        let initial_section_ptr = get_first_section_marker!(self);
        PVecIter {
            current_idx: 0,
            pvec: self,
            section_start: initial_section_ptr,
            len_before_section_start: 0,
        }
    }
}

// NOTE I am not sure if we need all of the below to do slicing over pvecs, but given that there
// are no stdlib traits that allow iterating over borrowed data structures within certain bounds,
// and also given that this implementation is here mostly to facilitate the mapreduce applications,
// it should be ok for now. Chances are the current iteration logic over pvecs is going to be too
// slow anyway so it will all be rewritten.
pub struct SegmentedSlice<'a, T> {
    start_idx: usize,
    end_idx: usize,
    // NOTE for now, we will not allow mutations to the underlying vec through a slice.
    underlying_vec: &'a PVec<T>,
}

pub struct SegmentedSliceIter<'a, 'b, T> {
    current_idx: usize,
    slice: &'b SegmentedSlice<'a, T>,
}

impl<'a, 'b, T> Iterator for SegmentedSliceIter<'a, 'b, T>
where
    T: Persistable,
{
    type Item = &'a T;

    fn next(&mut self) -> Option<Self::Item> {
        if self.current_idx + self.slice.start_idx >= self.slice.end_idx {
            return None;
        }

        let idx = self.current_idx + self.slice.start_idx;
        self.current_idx += 1;

        Some(&self.slice.underlying_vec[idx])
    }
}

impl<'a, 'b, T> IntoIterator for &'b SegmentedSlice<'a, T>
where
    T: Persistable,
{
    type Item = &'a T;

    type IntoIter = SegmentedSliceIter<'a, 'b, T>;

    fn into_iter(self) -> Self::IntoIter {
        SegmentedSliceIter {
            current_idx: 0,
            slice: self,
        }
    }
}

impl<T> std::fmt::Debug for PVec<T>
where
    T: Persistable + std::fmt::Debug + std::string::ToString,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.len() == 0 {
            return f.write_str("[]");
        }

        let mut iter = self.iter();
        let first = iter.next().unwrap();
        let some_string = iter.fold(first.to_string(), |acc, val| acc + ", " + &val.to_string());

        f.write_fmt(format_args!("[{}]", some_string))
    }
}
