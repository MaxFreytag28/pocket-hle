/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! CPU backend using the C++ library dynarmic, which is a dynamic recompiler.
//!
//! This is the fast backend, but it can only be used on platforms where
//! generating machine code at runtime is possible and dynarmic supports the
//! host architecture.

use super::{CpuError, CpuState};
use crate::mem::{ConstPtr, GuestUSize, Mem, MutPtr, Ptr, SafeRead, SafeWrite};

// Import functions from C++
use touchHLE_dynarmic_wrapper::*;

type VAddr = u32;

fn touchHLE_cpu_read_impl<T: SafeRead + Default>(
    mem: *mut touchHLE_Mem,
    addr: VAddr,
    error: *mut bool,
) -> T {
    // If a panic occurs (probably due to a null-pointer access), we can't let
    // it keep unwinding as it will hit non-Rust stack frames (dynarmic).
    // Instead we catch the unwind and then tell the C++ code a problem occurred
    // so it can immediately halt CPU execution and then panic itself, now
    // with only Rust stack frames to worry about and with CPU state information
    // available that's useful for debugging.
    //
    // TODO: Disable this in debug mode? This relies on dynarmic's
    // check_halt_on_memory_access option which surely has a significant
    // performance impact.
    //
    // I'm not sure if this actually is unwind-safe, but considering
    // the emulator will crash anyway, maybe this is okay.
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mem = unsafe { &mut *mem.cast::<Mem>() };
        let ptr: ConstPtr<T> = Ptr::from_bits(addr);
        mem.read(ptr)
    }));
    unsafe {
        error.write(res.is_err());
    }
    res.unwrap_or_default()
}

fn touchHLE_cpu_write_impl<T: SafeWrite>(mem: *mut touchHLE_Mem, addr: VAddr, value: T) -> bool {
    // See comments above about catch_unwind
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mem = unsafe { &mut *mem.cast::<Mem>() };
        let ptr: MutPtr<T> = Ptr::from_bits(addr);
        mem.write(ptr, value)
    }));
    res.is_err()
}

// Export functions for use by C++
#[no_mangle]
extern "C" fn touchHLE_cpu_read_u8(mem: *mut touchHLE_Mem, addr: VAddr, error: *mut bool) -> u8 {
    touchHLE_cpu_read_impl(mem, addr, error)
}
#[no_mangle]
extern "C" fn touchHLE_cpu_read_u16(mem: *mut touchHLE_Mem, addr: VAddr, error: *mut bool) -> u16 {
    touchHLE_cpu_read_impl(mem, addr, error)
}
#[no_mangle]
extern "C" fn touchHLE_cpu_read_u32(mem: *mut touchHLE_Mem, addr: VAddr, error: *mut bool) -> u32 {
    touchHLE_cpu_read_impl(mem, addr, error)
}
#[no_mangle]
extern "C" fn touchHLE_cpu_read_u64(mem: *mut touchHLE_Mem, addr: VAddr, error: *mut bool) -> u64 {
    touchHLE_cpu_read_impl(mem, addr, error)
}
#[no_mangle]
extern "C" fn touchHLE_cpu_write_u8(mem: *mut touchHLE_Mem, addr: VAddr, value: u8) -> bool {
    touchHLE_cpu_write_impl(mem, addr, value)
}
#[no_mangle]
extern "C" fn touchHLE_cpu_write_u16(mem: *mut touchHLE_Mem, addr: VAddr, value: u16) -> bool {
    touchHLE_cpu_write_impl(mem, addr, value)
}
#[no_mangle]
extern "C" fn touchHLE_cpu_write_u32(mem: *mut touchHLE_Mem, addr: VAddr, value: u32) -> bool {
    touchHLE_cpu_write_impl(mem, addr, value)
}
#[no_mangle]
extern "C" fn touchHLE_cpu_write_u64(mem: *mut touchHLE_Mem, addr: VAddr, value: u64) -> bool {
    touchHLE_cpu_write_impl(mem, addr, value)
}

pub struct DynarmicCpu {
    dynarmic_wrapper: *mut touchHLE_DynarmicWrapper,
    /// Copy of the direct memory access pointer used to check it has not
    /// changed. If this is null, direct memory access is not in use.
    direct_memory_access_ptr: *const std::ffi::c_void,
}

impl Drop for DynarmicCpu {
    fn drop(&mut self) {
        unsafe { touchHLE_DynarmicWrapper_delete(self.dynarmic_wrapper) }
    }
}

/// Object for storing the state of a CPU (registers etc), useful when switching
/// threads.
pub struct DynarmicContext {
    context: *mut Dynarmic_A32_Context,
}
impl DynarmicContext {
    pub fn new() -> Self {
        let context = unsafe { touchHLE_DynarmicWrapper_Context_new() };
        DynarmicContext { context }
    }
}
impl Drop for DynarmicContext {
    fn drop(&mut self) {
        unsafe { touchHLE_DynarmicWrapper_Context_delete(self.context) }
    }
}

impl DynarmicCpu {
    /// Construct a new CPU instance. If a mutable reference to a [Mem] instance
    /// is provided, direct memory access is enabled, and the CPU instance
    /// becomes bound to that [Mem] instance (subsequent calls must use the same
    /// one).
    pub fn new(direct_memory_access: Option<&mut Mem>) -> DynarmicCpu {
        // Null page count is in pages rather than bytes. Mem ensures it is
        // page aligned.
        let null_page_count: usize = direct_memory_access
            .as_ref()
            .map_or(0, |mem| mem.null_segment_size() / 0x1000)
            .try_into()
            .unwrap();
        // Safety: the direct memory access pointer will be retained directly by
        // the dynarmic wrapper and indirectly by cached JIT code, so we must
        // ensure we only execute the CPU while holding a &mut on the Mem object
        // to which that pointer belongs.
        let direct_memory_access_ptr = direct_memory_access
            .map_or(std::ptr::null_mut(), |mem| unsafe {
                mem.direct_memory_access_ptr()
            });
        let dynarmic_wrapper =
            unsafe { touchHLE_DynarmicWrapper_new(direct_memory_access_ptr, null_page_count) };
        DynarmicCpu {
            dynarmic_wrapper,
            direct_memory_access_ptr,
        }
    }

    pub fn regs(&self) -> &[u32; 16] {
        unsafe {
            let ptr = touchHLE_DynarmicWrapper_regs_const(self.dynarmic_wrapper);
            &*(ptr as *const [u32; 16])
        }
    }
    pub fn regs_mut(&mut self) -> &mut [u32; 16] {
        unsafe {
            let ptr = touchHLE_DynarmicWrapper_regs_mut(self.dynarmic_wrapper);
            &mut *(ptr as *mut [u32; 16])
        }
    }

    pub fn cpsr(&self) -> u32 {
        unsafe { touchHLE_DynarmicWrapper_cpsr(self.dynarmic_wrapper) }
    }
    pub fn set_cpsr(&mut self, cpsr: u32) {
        unsafe { touchHLE_DynarmicWrapper_set_cpsr(self.dynarmic_wrapper, cpsr) }
    }

    /// The VFP/NEON registers d0-d31. Only used by the lockstep mode.
    pub fn ext_regs(&mut self) -> [u64; 32] {
        let halves = unsafe {
            let ptr = touchHLE_DynarmicWrapper_ext_regs(self.dynarmic_wrapper);
            &*(ptr as *const [u32; 64])
        };
        std::array::from_fn(|i| u64::from(halves[i * 2]) | (u64::from(halves[i * 2 + 1]) << 32))
    }
    pub fn fpscr(&self) -> u32 {
        unsafe { touchHLE_DynarmicWrapper_fpscr(self.dynarmic_wrapper) }
    }

    /// Swap the current state of the CPU (registers etc) with the state stored
    /// in the context object.
    pub fn swap_context(&mut self, context: &mut DynarmicContext) {
        unsafe { touchHLE_DynarmicWrapper_swap_context(self.dynarmic_wrapper, context.context) }
    }

    /// Clear dynarmic's instruction cache for some range of addresses.
    /// This is of interest to the dynamic linker, which will sometimes rewrite
    /// code.
    pub fn invalidate_cache_range(&mut self, base: VAddr, size: GuestUSize) {
        unsafe {
            touchHLE_DynarmicWrapper_invalidate_cache_range(self.dynarmic_wrapper, base, size)
        }
    }

    /// See [super::Cpu::run_or_step].
    #[must_use]
    pub fn run_or_step(&mut self, mem: &mut Mem, ticks: Option<&mut u64>) -> CpuState {
        // See ::new() for why this is done.
        if !self.direct_memory_access_ptr.is_null() {
            assert!(self.direct_memory_access_ptr == unsafe { mem.direct_memory_access_ptr() });
        }

        let res = unsafe {
            touchHLE_DynarmicWrapper_run_or_step(
                self.dynarmic_wrapper,
                mem as *mut Mem as *mut touchHLE_Mem,
                ticks,
            )
        };
        match res {
            -1 => CpuState::Normal,
            -2 => CpuState::Error(CpuError::MemoryError),
            -3 => CpuState::Error(CpuError::UndefinedInstruction),
            -4 => CpuState::Error(CpuError::Breakpoint),
            _ if res < -4 => panic!("Unexpected CPU execution result"),
            svc => CpuState::Svc(svc as u32),
        }
    }
}
