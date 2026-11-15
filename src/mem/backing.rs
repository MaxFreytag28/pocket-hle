/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! The host memory behind the guest's 32-bit address space.
//!
//! There are two implementations with the same interface:
//!
//! - **Flat**: one 4GiB array covering the whole address space, so a guest
//!   address is simply an index. This relies on the host OS allocating pages
//!   lazily, and needs a 64-bit host.
//! - **Compact**: for 32-bit hosts (such as WebAssembly), where 4GiB can't be
//!   reserved. The address space is divided into 1MiB sections, and only the
//!   sections that are actually used are given host memory, from a much
//!   smaller buffer. It can also be enabled on 64-bit hosts with the
//!   `compact_mem` feature, for testing.
//!
//! In compact mode, guest memory that must be contiguous for the host (a
//! segment of a binary, or the heap) is mapped in one go, so that it is
//! contiguous in the host buffer too. Accesses that would cross between two
//! separately mapped ranges are refused.

use std::ops::Range;

pub use imp::*;

#[cfg(not(any(target_pointer_width = "32", feature = "compact_mem")))]
mod imp {
    use super::Range;

    pub const COMPACT: bool = false;
    /// Where the memory allocator may put allocations, if it is restricted.
    pub const HEAP: Option<(u32, u32)> = None;

    type Bytes = [u8; 1 << 32];

    pub struct Backing {
        /// This array is 4GiB in size so that it can cover the entire 32-bit
        /// virtual address space, but it should not use that much physical
        /// memory, assuming that the host OS backs it with lazily-allocated
        /// pages and we are careful to avoid accessing most of it.
        ///
        /// This is a raw pointer because inevitably we will have to hand out
        /// pointers to memory sometimes, and being able to hold a `&mut` on
        /// this array simultaneously seems like an undefined behavior trap.
        /// This also means that the underlying memory should never be moved.
        bytes: *mut Bytes,
    }

    impl Drop for Backing {
        fn drop(&mut self) {
            let layout = std::alloc::Layout::new::<Bytes>();
            unsafe {
                std::alloc::dealloc(self.bytes as *mut _, layout);
            }
        }
    }

    impl Backing {
        pub fn new() -> Self {
            // This will hopefully get the host OS to lazily allocate the
            // memory.
            let layout = std::alloc::Layout::new::<Bytes>();
            // TODO: align memory to guest page size
            // Right now, if aligned, will cause OOM on low end Android devices.
            // See relevant [Github's issue](https://github.com/touchHLE/touchHLE/issues/498)
            let bytes = unsafe { std::alloc::alloc_zeroed(layout) as *mut Bytes };
            assert!(!bytes.is_null());
            Backing { bytes }
        }

        /// Get the range of the host buffer for `count` bytes of guest memory
        /// at `addr`, if they are all mapped (and contiguous).
        #[inline(always)]
        pub fn range(&self, addr: u32, count: u32) -> Option<Range<usize>> {
            let start = addr as usize;
            let end = start + count as usize;
            (end <= 1 << 32).then_some(start..end)
        }

        /// Make sure a range of guest memory is mapped contiguously.
        pub fn map(&mut self, _base: u32, _size: u32) {}

        pub fn bytes(&self) -> &[u8] {
            let bytes: &Bytes = unsafe { &*self.bytes };
            bytes
        }
        pub fn bytes_mut(&mut self) -> &mut [u8] {
            let bytes: &mut Bytes = unsafe { &mut *self.bytes };
            bytes
        }

        /// The inverse of [Self::range].
        pub fn guest_addr_of(&self, host_ptr: *const u8) -> Option<u32> {
            let range = self.bytes().as_ptr_range();
            range
                .contains(&host_ptr)
                .then(|| (host_ptr as usize - range.start as usize) as u32)
        }

        /// Pointer to the whole address space, for direct access by a
        /// recompiler.
        pub fn direct_ptr(&mut self) -> *mut std::ffi::c_void {
            self.bytes.cast()
        }
    }
}

#[cfg(any(target_pointer_width = "32", feature = "compact_mem"))]
mod imp {
    use super::Range;

    pub const COMPACT: bool = true;

    const SECTION_SHIFT: u32 = 20;
    const SECTION_SIZE: usize = 1 << SECTION_SHIFT;
    const SECTION_COUNT: usize = 1 << (32 - SECTION_SHIFT);

    /// The heap. iPhone OS devices of the time had 128MiB or 256MiB of RAM in
    /// total, and apps could use well under half of it.
    const HEAP_BASE: u32 = 0x4000_0000;
    const HEAP_SIZE: u32 = 96 << SECTION_SHIFT;
    pub const HEAP: Option<(u32, u32)> = Some((HEAP_BASE, HEAP_SIZE));

    /// How much host memory there is to hand out: the heap, the main thread's
    /// stack, and the sections touched by the app binary and the dynamic
    /// libraries.
    const HOST_SECTIONS: usize = (HEAP_SIZE >> SECTION_SHIFT) as usize + 1 + 63;

    pub struct Backing {
        /// Raw pointer for the same reasons as in the flat implementation.
        bytes: *mut u8,
        /// For each section of the guest address space: 0 if it is not
        /// mapped, otherwise the number of its host section plus one.
        sections: Box<[u16; SECTION_COUNT]>,
        next_host_section: usize,
    }

    fn layout() -> std::alloc::Layout {
        std::alloc::Layout::from_size_align(HOST_SECTIONS * SECTION_SIZE, 4096).unwrap()
    }

    impl Drop for Backing {
        fn drop(&mut self) {
            unsafe {
                std::alloc::dealloc(self.bytes, layout());
            }
        }
    }

    impl Backing {
        pub fn new() -> Self {
            let bytes = unsafe { std::alloc::alloc_zeroed(layout()) };
            assert!(!bytes.is_null());
            let mut backing = Backing {
                bytes,
                sections: Box::new([0; SECTION_COUNT]),
                next_host_section: 0,
            };
            backing.map(HEAP_BASE, HEAP_SIZE);
            backing.map(
                crate::mem::Mem::MAIN_THREAD_STACK_LOW_END,
                crate::mem::Mem::MAIN_THREAD_STACK_SIZE,
            );
            backing
        }

        #[inline(always)]
        fn host_offset(&self, addr: u32) -> Option<usize> {
            match self.sections[(addr >> SECTION_SHIFT) as usize] {
                0 => None,
                entry => Some(
                    ((entry as usize - 1) << SECTION_SHIFT) | (addr as usize & (SECTION_SIZE - 1)),
                ),
            }
        }

        /// Get the range of the host buffer for `count` bytes of guest memory
        /// at `addr`, if they are all mapped (and contiguous).
        #[inline(always)]
        pub fn range(&self, addr: u32, count: u32) -> Option<Range<usize>> {
            if count == 0 {
                // Nothing will be accessed, so even an unmapped address is
                // fine. This matters for things like empty strings at the
                // very end of a mapping.
                let start = self.host_offset(addr).unwrap_or(0);
                return Some(start..start);
            }
            let start = self.host_offset(addr)?;
            let last = addr.checked_add(count - 1)?;
            let (first_section, last_section) = (addr >> SECTION_SHIFT, last >> SECTION_SHIFT);
            if first_section != last_section {
                self.check_contiguous(first_section, last_section)?;
            }
            Some(start..start + count as usize)
        }

        #[cold]
        fn check_contiguous(&self, first_section: u32, last_section: u32) -> Option<()> {
            let first_entry = self.sections[first_section as usize];
            for section in first_section + 1..=last_section {
                let entry = self.sections[section as usize];
                if entry == 0 || u32::from(entry) != u32::from(first_entry) + (section - first_section)
                {
                    return None;
                }
            }
            Some(())
        }

        /// Make sure a range of guest memory is mapped contiguously.
        pub fn map(&mut self, base: u32, size: u32) {
            if size == 0 {
                return;
            }
            let first_section = (base >> SECTION_SHIFT) as usize;
            let last_section = ((u64::from(base) + u64::from(size) - 1) >> SECTION_SHIFT) as usize;
            for section in first_section..=last_section {
                if self.sections[section] != 0 {
                    continue;
                }
                if self.next_host_section == HOST_SECTIONS {
                    panic!(
                        "Out of host memory for the guest address space while mapping {:#x} ({:#x} bytes)",
                        base, size
                    );
                }
                self.next_host_section += 1;
                self.sections[section] = self.next_host_section as u16;
            }
            if self
                .check_contiguous(first_section as u32, last_section as u32)
                .is_none()
            {
                panic!(
                    "Guest memory at {:#x} ({:#x} bytes) overlaps an earlier mapping in a way that can't be made contiguous",
                    base, size
                );
            }
        }

        pub fn bytes(&self) -> &[u8] {
            unsafe { std::slice::from_raw_parts(self.bytes, HOST_SECTIONS * SECTION_SIZE) }
        }
        pub fn bytes_mut(&mut self) -> &mut [u8] {
            unsafe { std::slice::from_raw_parts_mut(self.bytes, HOST_SECTIONS * SECTION_SIZE) }
        }

        /// The inverse of [Self::range].
        pub fn guest_addr_of(&self, host_ptr: *const u8) -> Option<u32> {
            let range = self.bytes().as_ptr_range();
            if !range.contains(&host_ptr) {
                return None;
            }
            let offset = host_ptr as usize - range.start as usize;
            let entry = (offset >> SECTION_SHIFT) as u16 + 1;
            let section = self.sections.iter().position(|&e| e == entry)?;
            Some(((section << SECTION_SHIFT) | (offset & (SECTION_SIZE - 1))) as u32)
        }

        /// There is no single pointer to the whole address space in this mode,
        /// so recompilers that want one can't be used.
        pub fn direct_ptr(&mut self) -> *mut std::ffi::c_void {
            panic!("Direct memory access is not possible with compact guest memory")
        }
    }
}
