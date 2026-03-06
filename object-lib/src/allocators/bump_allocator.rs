//! Implementation of a stateless bump allocator.
use std::alloc::{AllocError, Allocator, Layout};
use std::sync::Arc;

use parking_lot::RwLock;

use crate::files;

static PAGE_SIZE_BYTES: usize = 8 * 1024;

/// An [`std::alloc::Allocator`] compliant implementation of a bump allocator, currently using an
/// mmap-backed piece of memory as its underlying allocation pool.
///
/// The allocator itself is stateless, so that it can be reinstantiated correctly on any machine
/// across object movements. All the data that is needed for correctly instantiating it on a new
/// machine is derived from the underlying storage (currently, the file that is being mmap-ed).
pub struct BumpAllocator {
    // FIXME @question does this need to be wrapped in Arc<RwLock<>>?
    #[doc(hidden)]
    backing_storage: Arc<RwLock<files::FileHandle>>,
}

impl BumpAllocator {
    pub fn new(backing_storage: Arc<RwLock<files::FileHandle>>) -> Self {
        Self { backing_storage }
    }

    pub fn can_allocate(&self, allocation_size_bytes: usize) -> bool {
        let backing_storage = self.backing_storage.read();
        backing_storage.can_support_write(allocation_size_bytes)
    }

    pub fn current_write_offset(&self) -> usize {
        let backing_storage = self.backing_storage.read();
        backing_storage.next_write_offset
    }
}

unsafe impl Allocator for BumpAllocator {
    fn allocate(&self, layout: Layout) -> Result<std::ptr::NonNull<[u8]>, AllocError> {
        let mut file_handle = self.backing_storage.write();
        let current_size = file_handle.file_size;

        let old_allocation_marker = file_handle.allocation_marker;

        // Get pointer to the end of the currently allocated region
        let ptr = {
            let mut mapped_file = file_handle.mapped_file.write().unwrap();
            let region_start = mapped_file.first_mut().unwrap();

            unsafe {
                std::ptr::addr_of_mut!(*region_start)
                    .byte_offset(old_allocation_marker.try_into().unwrap())
            }
        };

        let alignment = layout.align();
        let (extra_size, offset) = match ptr.is_aligned_to(alignment) {
            true => (layout.size(), 0),
            false => {
                let offset = alignment - (ptr as usize % alignment);
                (layout.size() + offset, offset)
            }
        };

        // Check if we need to ask the OS for an allocation.
        if current_size - old_allocation_marker <= extra_size {
            let allocation_request_size = std::cmp::max(extra_size, PAGE_SIZE_BYTES);

            #[cfg(debug_assertions)]
            println!(
                "about to request {}kiB to be allocated",
                allocation_request_size as f64 / 1024.0
            );

            #[cfg(not(feature = "no-persist"))]
            match files::resize(&mut file_handle, current_size + allocation_request_size) {
                Ok(()) => (),
                Err(e) => {
                    eprintln!("[BumpAllocator] Failed to resize backing storage: {}", e);
                    return Err(AllocError);
                }
            };
        }

        // mark the start of the newly allocated region as occupied (due to the current allocation)
        file_handle.allocation_marker += extra_size;

        let aligned_ptr = {
            let mut mapped_file = file_handle.mapped_file.write().unwrap();
            let region_start = mapped_file.first_mut().unwrap();

            unsafe {
                std::ptr::addr_of_mut!(*region_start)
                    .byte_offset((old_allocation_marker + offset).try_into().unwrap())
            }
        };

        Ok(unsafe {
            std::ptr::NonNull::new_unchecked(std::ptr::slice_from_raw_parts_mut(
                aligned_ptr,
                layout.size(),
            ))
        })
    }

    unsafe fn deallocate(&self, _ptr: std::ptr::NonNull<u8>, _layout: Layout) {
        // NOTE this is meant to be a noop for now. We could make this work like an _actual_ bump
        // allocator and simply clear the entirety of the underlying storage, but for now we don't
        // need this.
        ()
    }
}
