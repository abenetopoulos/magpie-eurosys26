use std::fmt::Display;
use std::hash::{Hash, Hasher};
use std::ops::Range;
#[cfg(feature = "simd")]
use std::simd;
use std::sync::Arc;

use parking_lot::RwLock;

use crate::allocators::bump_allocator::BumpAllocator;
use crate::allocators::persistently_allocatable::{CopyFrom, PersistentlyAllocatable};
use crate::{
    collections::pvec::{PVec, PVecIter, SegmentedSlice},
    Persistable,
};

#[repr(C)]
#[derive(PartialEq, PartialOrd, Eq, Ord)]
pub struct PString {
    vec: PVec<u8>,
}

impl PString {
    pub fn new() -> Self {
        Self { vec: PVec::new() }
    }

    pub fn resize_to_capacity(&mut self, capacity: usize) {
        self.vec.resize_to_capacity(capacity);
    }

    pub fn len(&self) -> usize {
        self.vec.len()
    }

    pub fn from(&mut self, source: &str) {
        self.resize_to_capacity(source.len());
        for c in source.chars() {
            self.vec.push(c as u8);
        }
    }

    fn split_at_byte(&self, at_byte: u8) -> PStringSplit {
        PStringSplit {
            source_str: self,
            start: 0,
            end: self.len(),
            pred: at_byte,
            is_done: false,
        }
    }

    pub fn split(&self, at_char: char) -> PStringSplit {
        self.split_at_byte(at_char as u8)
    }

    pub fn get_slice(&self, slice_range: Range<usize>) -> SegmentedSlice<'_, u8> {
        self.vec.get_slice(slice_range)
    }

    #[cfg(feature = "simd")]
    fn simd_eq(&self, other: &str) -> bool {
        if self.len() != other.len() {
            return false;
        }

        if self.len() < 8 {
            return *self == other;
        }

        let mut start_idx = 0;
        let chunk_size = 8;
        let string_bytes = other.as_bytes();
        unsafe {
            loop {
                if start_idx > self.len() {
                    break;
                }

                let slc = std::slice::from_ptr_range(
                    self.vec
                        .get_slice_as_ptr_range(start_idx..(start_idx + chunk_size))
                        .expect("failed to get ptr range of slice"),
                );

                if slc.len() < 8 {
                    return Iterator::eq(
                        slc.iter().map(|c| *c),
                        string_bytes[start_idx..slc.len()].iter().map(|c| *c),
                    );
                }

                let pstring_slice = simd::u8x8::from_slice(slc);
                let other_slice =
                    simd::u8x8::from_slice(&string_bytes[start_idx..(start_idx + chunk_size)]);

                if pstring_slice != other_slice {
                    return false;
                }

                start_idx += chunk_size;
            }
        }

        true
    }
}

impl Persistable for PString {
    fn adjust_from(&mut self, other: &Self) {
        self.vec.adjust_from(&other.vec);
    }
}

impl PersistentlyAllocatable for PString {
    fn set_allocator(&mut self, allocator: Arc<RwLock<BumpAllocator>>) {
        self.vec.set_allocator(allocator);
    }

    fn get_allocator(&self) -> Option<Arc<RwLock<BumpAllocator>>> {
        self.vec.get_allocator()
    }
}

impl Default for PString {
    fn default() -> PString {
        PString::new()
    }
}

struct PStringIter<'a, T> {
    iter: PVecIter<'a, T>,
}

impl<'a, T> Iterator for PStringIter<'a, T>
where
    T: Persistable,
{
    type Item = &'a T;

    fn next(&mut self) -> Option<Self::Item> {
        self.iter.next()
    }
}

impl Into<String> for PString {
    fn into(self) -> String {
        self.vec.iter().map(|e| *e as char).collect()
    }
}

impl Into<String> for &PString {
    fn into(self) -> String {
        self.vec.iter().map(|e| *e as char).collect()
    }
}

impl CopyFrom for PString {
    type From = String;

    fn from(&mut self, source: String) {
        let source_bytes = source.as_bytes();
        self.vec.resize_to_capacity(source_bytes.len());
        for sb in source_bytes {
            self.vec.push(*sb);
        }
    }
}

impl Hash for PString {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.vec
            .iter()
            .map(|e| *e as char)
            .collect::<String>()
            .hash(state)
    }
}

impl PartialEq<String> for PString {
    fn eq(&self, other: &String) -> bool {
        self.eq(&other.as_str())
    }
}

impl<'a> PartialEq<&'a str> for PString {
    fn eq(&self, other: &&'a str) -> bool {
        if self.len() != other.len() {
            return false;
        }

        #[cfg(feature = "simd")]
        {
            if self.len() < 8 {
                return Iterator::eq(self.vec.iter().map(|b| *b), other.chars().map(|c| c as u8));
            }

            return self.simd_eq(other);
        }

        #[cfg(not(feature = "simd"))]
        Iterator::eq(self.vec.iter().map(|b| *b), other.chars().map(|c| c as u8))
    }
}

impl Display for PString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let p: String = self.into();
        f.write_fmt(format_args!("{}", &p))
    }
}

pub struct PStringSplit<'a> {
    source_str: &'a PString,
    start: usize,
    end: usize,
    pred: u8,
    is_done: bool,
}

impl<'a> Iterator for PStringSplit<'a> {
    // FIXME this should ideally be a string slice (or at least a reference type) -- see note
    // below.
    type Item = String;

    fn next(&mut self) -> Option<Self::Item> {
        if self.is_done {
            return None;
        }

        if self.start >= self.end {
            self.is_done = true;
            return None;
        }

        let mut current_match_end = self.start;

        loop {
            if current_match_end == self.end {
                break;
            }

            if self.source_str.vec[current_match_end] == self.pred {
                break;
            }

            current_match_end += 1;
        }

        let matched_slice: Vec<u8> = self
            .source_str
            .vec
            .get_slice(self.start..current_match_end)
            .into_iter()
            .map(|e| *e)
            .collect();
        self.start = current_match_end + 1;

        // NOTE there are two issues with this implementation of the split iterator:
        // 1) we are forced to copy and the bytes corresponding to the string slice from the vector
        //    slice (see above)
        // 2) we are forced to return an _owned_ String
        // Both of the above issues stem from the fact that, by design, a string's contents can be
        // split across multiple PVec segments. This in turn means that a matched substring for the
        // split string can span multiple segments, which makes it impossible to transparently
        // return a string slice from here. All this could end up having performance implications,
        // but I am not sure how we can do any better given the current design of pvec.
        match String::from_utf8(matched_slice) {
            Ok(e) => Some(e),
            Err(e) => {
                panic!("failed to convert slice of pstring into str: {}", e);
            }
        }
    }
}
