/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! Stand-in for the dynarmic backend in builds without the `jit` feature
//! (for example, for platforms where dynarmic can't work at all).
//!
//! The types here can never be constructed, so the rest of the CPU module can
//! be written as if the backend existed, and the compiler removes all the
//! code that would use it.

use super::CpuState;
use crate::mem::{GuestUSize, Mem};

pub enum DynarmicCpu {}
pub enum DynarmicContext {}

impl DynarmicContext {
    pub fn new() -> Self {
        unreachable!("This build of touchHLE has no JIT CPU backend")
    }
}

impl DynarmicCpu {
    pub fn new(_direct_memory_access: Option<&mut Mem>) -> DynarmicCpu {
        unreachable!("This build of touchHLE has no JIT CPU backend")
    }
    pub fn regs(&self) -> &[u32; 16] {
        match *self {}
    }
    pub fn regs_mut(&mut self) -> &mut [u32; 16] {
        match *self {}
    }
    pub fn cpsr(&self) -> u32 {
        match *self {}
    }
    pub fn set_cpsr(&mut self, _cpsr: u32) {
        match *self {}
    }
    pub fn ext_regs(&mut self) -> [u64; 32] {
        match *self {}
    }
    pub fn fpscr(&self) -> u32 {
        match *self {}
    }
    pub fn swap_context(&mut self, _context: &mut DynarmicContext) {
        match *self {}
    }
    pub fn invalidate_cache_range(&mut self, _base: u32, _size: GuestUSize) {
        match *self {}
    }
    pub fn run_or_step(&mut self, _mem: &mut Mem, _ticks: Option<&mut u64>) -> CpuState {
        match *self {}
    }
}
