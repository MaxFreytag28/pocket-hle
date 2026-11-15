/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! CPU emulation.
//!
//! iPhone OS apps used either ARMv6 or ARMv7-A, which are both 32-bit ISAs.
//!
//! There are two backends behind the [Cpu] type:
//! - [dynarmic], a dynamic recompiler (fast, but needs a host that allows
//!   generating machine code at runtime).
//! - [interpreter], a portable interpreter (slow, but works anywhere).
//!
//! The backend is picked with the `TOUCHHLE_CPU` environment variable:
//! `dynarmic` (the default), `interpreter`, or `lockstep`. The last one is a
//! debugging mode that executes every instruction on both backends and
//! compares the results, treating dynarmic as the reference. It is meant for
//! finding interpreter bugs and missing instructions with real apps.

#[cfg(feature = "jit")]
mod dynarmic;
#[cfg(not(feature = "jit"))]
#[path = "cpu/dynarmic_unavailable.rs"]
mod dynarmic;
mod interpreter;

use crate::abi::GuestFunction;
use crate::mem::{ConstVoidPtr, GuestUSize, Mem, Ptr};
use std::collections::HashMap;
use std::sync::OnceLock;

type VAddr = u32;

/// Why CPU execution ended.
#[derive(Debug)]
pub enum CpuState {
    /// Execution halted due to using up all remaining ticks (normal execution)
    /// or after the single instruction was executed (step execution).
    Normal,
    /// SVC instruction encountered.
    Svc(u32),
    /// An error was encountered.
    Error(CpuError),
}

/// A reason that can cause CPU execution to be interrupted.
#[derive(Debug)]
pub enum CpuError {
    /// Memory error during execution (probably a null page access).
    MemoryError,
    /// Undefined instruction (perhaps from a GDB software breakpoint).
    UndefinedInstruction,
    /// Breakpoint (`bkpt` instruction).
    Breakpoint,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum BackendKind {
    Dynarmic,
    Interpreter,
    Lockstep,
}

fn backend_kind() -> BackendKind {
    static KIND: OnceLock<BackendKind> = OnceLock::new();
    *KIND.get_or_init(|| {
        // dynarmic accesses guest memory through a pointer to the whole
        // address space, which doesn't exist with the compact memory layout.
        let has_jit = cfg!(feature = "jit") && !crate::mem::COMPACT_MEM;
        let kind = match std::env::var("TOUCHHLE_CPU").as_deref() {
            Ok("interpreter") => BackendKind::Interpreter,
            Ok("lockstep") => BackendKind::Lockstep,
            Ok("dynarmic") => BackendKind::Dynarmic,
            Err(_) if has_jit => BackendKind::Dynarmic,
            Err(_) => BackendKind::Interpreter,
            Ok(other) => panic!("Unknown TOUCHHLE_CPU value {other:?}"),
        };
        if kind != BackendKind::Interpreter && !has_jit {
            panic!("TOUCHHLE_CPU={kind:?} needs a build of touchHLE with the JIT CPU backend");
        }
        if kind != BackendKind::Dynarmic {
            echo!("touchHLE::cpu: using CPU backend {:?}", kind);
        }
        kind
    })
}

/// State of the lockstep debugging mode, see the module documentation.
struct Lockstep {
    reference: dynarmic::DynarmicCpu,
    tested: interpreter::InterpreterCpu,
    /// Instructions the interpreter refused, by description.
    unimplemented: HashMap<&'static str, u64>,
    steps: u64,
    compared: u64,
}

enum Backend {
    Dynarmic(dynarmic::DynarmicCpu),
    Interpreter(interpreter::InterpreterCpu),
    Lockstep(Box<Lockstep>),
}

pub struct Cpu {
    backend: Backend,
}

enum ContextInner {
    Dynarmic(dynarmic::DynarmicContext),
    Interpreter(Box<interpreter::State>),
    Lockstep(dynarmic::DynarmicContext, Box<interpreter::State>),
}

/// Object for storing the state of a CPU (registers etc), useful when switching
/// threads.
pub struct CpuContext {
    inner: ContextInner,
}
impl CpuContext {
    // Without the "jit" feature, the dynarmic types can't be constructed.
    #[cfg_attr(not(feature = "jit"), allow(unreachable_code))]
    pub fn new() -> Self {
        let inner = match backend_kind() {
            BackendKind::Dynarmic => ContextInner::Dynarmic(dynarmic::DynarmicContext::new()),
            BackendKind::Interpreter => {
                ContextInner::Interpreter(Box::new(interpreter::State::new()))
            }
            BackendKind::Lockstep => ContextInner::Lockstep(
                dynarmic::DynarmicContext::new(),
                Box::new(interpreter::State::new()),
            ),
        };
        CpuContext { inner }
    }
}

impl Cpu {
    /// The register number of the stack pointer.
    pub const SP: usize = 13;
    /// The register number of the link register.
    #[allow(unused)]
    pub const LR: usize = 14;
    /// The register number of the program counter.
    pub const PC: usize = 15;

    /// When this bit is set in CPSR, the CPU is in Thumb mode.
    pub const CPSR_THUMB: u32 = 0x00000020;

    /// When this bit is set in CPSR, the CPU is in user mode.
    pub const CPSR_USER_MODE: u32 = 0x00000010;

    /// Construct a new CPU instance. If a mutable reference to a [Mem] instance
    /// is provided, direct memory access is enabled, and the CPU instance
    /// becomes bound to that [Mem] instance (subsequent calls must use the same
    /// one).
    #[cfg_attr(not(feature = "jit"), allow(unreachable_code))]
    pub fn new(direct_memory_access: Option<&mut Mem>) -> Cpu {
        let backend = match backend_kind() {
            BackendKind::Dynarmic => {
                Backend::Dynarmic(dynarmic::DynarmicCpu::new(direct_memory_access))
            }
            BackendKind::Interpreter => Backend::Interpreter(interpreter::InterpreterCpu::new()),
            BackendKind::Lockstep => Backend::Lockstep(Box::new(Lockstep {
                reference: dynarmic::DynarmicCpu::new(direct_memory_access),
                tested: interpreter::InterpreterCpu::new(),
                unimplemented: HashMap::new(),
                steps: 0,
                compared: 0,
            })),
        };
        Cpu { backend }
    }

    pub fn regs(&self) -> &[u32; 16] {
        match &self.backend {
            Backend::Dynarmic(cpu) => cpu.regs(),
            Backend::Interpreter(cpu) => &cpu.state.regs,
            Backend::Lockstep(lockstep) => lockstep.reference.regs(),
        }
    }
    pub fn regs_mut(&mut self) -> &mut [u32; 16] {
        match &mut self.backend {
            Backend::Dynarmic(cpu) => cpu.regs_mut(),
            Backend::Interpreter(cpu) => &mut cpu.state.regs,
            Backend::Lockstep(lockstep) => lockstep.reference.regs_mut(),
        }
    }

    pub fn dump_regs(&self) {
        let regs = self.regs();
        for row in 0..4 {
            use std::fmt::Write;
            let mut line = String::new();
            for col in 0..4 {
                let reg_idx = row * 4 + col;
                match reg_idx {
                    Self::SP => write!(&mut line, "\t SP: "),
                    Self::LR => write!(&mut line, "\t LR: "),
                    Self::PC => write!(&mut line, "\t PC: "),
                    _ if reg_idx <= 9 => write!(&mut line, "\t R{reg_idx}: "),
                    _ => write!(&mut line, "\tR{reg_idx}: "),
                }
                .unwrap();
                write!(&mut line, "{:#010x}", regs[reg_idx]).unwrap();
            }
            echo!("{}", line);
        }
    }

    pub fn cpsr(&self) -> u32 {
        match &self.backend {
            Backend::Dynarmic(cpu) => cpu.cpsr(),
            Backend::Interpreter(cpu) => cpu.state.cpsr,
            Backend::Lockstep(lockstep) => lockstep.reference.cpsr(),
        }
    }
    pub fn set_cpsr(&mut self, cpsr: u32) {
        match &mut self.backend {
            Backend::Dynarmic(cpu) => cpu.set_cpsr(cpsr),
            Backend::Interpreter(cpu) => cpu.state.cpsr = cpsr,
            Backend::Lockstep(lockstep) => lockstep.reference.set_cpsr(cpsr),
        }
    }

    /// Swap the current state of the CPU (registers etc) with the state stored
    /// in the context object.
    pub fn swap_context(&mut self, context: &mut CpuContext) {
        match (&mut self.backend, &mut context.inner) {
            (Backend::Dynarmic(cpu), ContextInner::Dynarmic(context)) => cpu.swap_context(context),
            (Backend::Interpreter(cpu), ContextInner::Interpreter(context)) => {
                std::mem::swap(&mut cpu.state, &mut **context)
            }
            (Backend::Lockstep(lockstep), ContextInner::Lockstep(reference, tested)) => {
                lockstep.reference.swap_context(reference);
                std::mem::swap(&mut lockstep.tested.state, &mut **tested);
            }
            _ => unreachable!("CPU context from a different backend"),
        }
    }

    /// Get PC with the Thumb bit appropriately set.
    pub fn pc_with_thumb_bit(&self) -> GuestFunction {
        let pc = self.regs()[Self::PC];
        let thumb = (self.cpsr() & Self::CPSR_THUMB) == Self::CPSR_THUMB;
        GuestFunction::from_addr_and_thumb_flag(pc, thumb)
    }

    /// Set PC and the Thumb flag for executing a guest function. Note that this
    /// does not touch LR.
    pub fn branch(&mut self, new_pc: GuestFunction) {
        self.regs_mut()[Self::PC] = new_pc.addr_without_thumb_bit();
        let cpsr_without_thumb = self.cpsr() & (!Self::CPSR_THUMB);
        self.set_cpsr(cpsr_without_thumb | ((new_pc.is_thumb() as u32) * Self::CPSR_THUMB))
    }

    /// Set the PC and Thumb flag (like [Self::branch]), but also set the LR,
    /// and return the original PC and LR.
    pub fn branch_with_link(
        &mut self,
        new_pc: GuestFunction,
        new_lr: GuestFunction,
    ) -> (GuestFunction, GuestFunction) {
        let old_pc = self.pc_with_thumb_bit();
        let old_lr = GuestFunction::from_addr_with_thumb_bit(self.regs()[Self::LR]);
        self.branch(new_pc);
        self.regs_mut()[Self::LR] = new_lr.addr_with_thumb_bit();
        (old_pc, old_lr)
    }

    /// Clear cached translations of code for some range of addresses.
    /// This is of interest to the dynamic linker, which will sometimes rewrite
    /// code.
    pub fn invalidate_cache_range(&mut self, base: VAddr, size: GuestUSize) {
        match &mut self.backend {
            Backend::Dynarmic(cpu) => cpu.invalidate_cache_range(base, size),
            // The interpreter doesn't cache anything.
            Backend::Interpreter(_) => (),
            Backend::Lockstep(lockstep) => lockstep.reference.invalidate_cache_range(base, size),
        }
    }

    /// Start CPU execution.
    ///
    /// If `ticks` is [Some], it is used as an abstract time limit. The value
    /// will be reduced proportionately with the amount of ticks expended.
    ///
    /// If `ticks` is [None], the CPU executes only a single instruction. This
    /// is also known as "stepping".
    ///
    /// This will return either because the CPU ran out of time, or because
    /// something else happened which requires attention from the host.
    #[must_use]
    pub fn run_or_step(&mut self, mem: &mut Mem, ticks: Option<&mut u64>) -> CpuState {
        match &mut self.backend {
            Backend::Dynarmic(cpu) => cpu.run_or_step(mem, ticks),
            Backend::Interpreter(cpu) => cpu.run_or_step(mem, ticks),
            Backend::Lockstep(lockstep) => lockstep.run_or_step(mem, ticks),
        }
    }
}

impl Lockstep {
    /// Flags (NZCVQ), GE bits and the Thumb bit.
    const CPSR_COMPARE_MASK: u32 = 0xF80F0020;

    fn run_or_step(&mut self, mem: &mut Mem, mut ticks: Option<&mut u64>) -> CpuState {
        loop {
            if let Some(ticks) = ticks.as_deref_mut() {
                if *ticks == 0 {
                    return CpuState::Normal;
                }
                *ticks -= 1;
            }
            let state = self.step(mem);
            if ticks.is_none() || !matches!(state, CpuState::Normal) {
                return state;
            }
        }
    }

    /// Execute one instruction on both backends and compare the results.
    fn step(&mut self, mem: &mut Mem) -> CpuState {
        self.steps += 1;

        // Each instruction is checked independently: the interpreter always
        // starts from the reference's integer state. (The floating-point state
        // can't be read from dynarmic, so there the interpreter is on its own,
        // and errors show up once a value reaches an integer register.)
        let regs_before = *self.reference.regs();
        let cpsr_before = self.reference.cpsr();
        self.tested.state.regs = regs_before;
        self.tested.state.cpsr = cpsr_before;
        let d_before = self.reference.ext_regs();
        let fpscr_before = self.reference.fpscr();
        self.tested.state.d = d_before;
        self.tested.state.fpscr = fpscr_before;

        let thumb = cpsr_before & Cpu::CPSR_THUMB != 0;
        let pc = regs_before[Cpu::PC];
        let ptr: ConstVoidPtr = Ptr::from_bits(pc);
        let instruction = mem.get_bytes_fallible(ptr, 4).map(|bytes| {
            if thumb {
                u32::from(u16::from_le_bytes([bytes[0], bytes[1]])) << 16
                    | u32::from(u16::from_le_bytes([bytes[2], bytes[3]]))
            } else {
                u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
            }
        });

        // An instruction that both reads and writes the same memory can't be
        // executed twice, so the interpreter has to sit that one out.
        let tested_result = match instruction {
            Some(instruction) if !thumb && interpreter::reads_and_writes_same_memory(instruction) => {
                Err("(not comparable in lockstep)")
            }
            _ => self.tested.step(mem),
        };
        let reference_result = self.reference.run_or_step(mem, None);

        match tested_result {
            Err(description) => {
                let count = self.unimplemented.entry(description).or_insert(0);
                *count += 1;
                if *count == 1 {
                    echo!(
                        "touchHLE::cpu: lockstep: interpreter lacks {}: {:08x?} at {:#x} ({})",
                        description,
                        instruction,
                        pc,
                        if thumb { "Thumb" } else { "ARM" }
                    );
                }
            }
            Ok(tested_result) => {
                self.compared += 1;
                let same_kind = match (&tested_result, &reference_result) {
                    (CpuState::Normal, CpuState::Normal) => true,
                    (CpuState::Svc(a), CpuState::Svc(b)) => a == b,
                    // Where exactly the registers are left after an error is
                    // not worth matching.
                    (CpuState::Error(a), CpuState::Error(b)) => {
                        if std::mem::discriminant(a) == std::mem::discriminant(b) {
                            return reference_result;
                        }
                        false
                    }
                    _ => false,
                };
                let regs_after = *self.reference.regs();
                let cpsr_after = self.reference.cpsr();
                let d_after = self.reference.ext_regs();
                let fpscr_after = self.reference.fpscr();
                // Only the comparison flags of FPSCR are modelled.
                let float_mismatch = self.tested.state.d != d_after
                    || (self.tested.state.fpscr ^ fpscr_after) & 0xF0000000 != 0;
                if float_mismatch {
                    for reg in 0..32 {
                        if self.tested.state.d[reg] != d_after[reg] {
                            echo!(
                                "  d{}: before {:016x}, interpreter {:016x}, dynarmic {:016x}",
                                reg,
                                d_before[reg],
                                self.tested.state.d[reg],
                                d_after[reg]
                            );
                        }
                    }
                    echo!(
                        "  fpscr: before {:08x}, interpreter {:08x}, dynarmic {:08x}",
                        fpscr_before,
                        self.tested.state.fpscr,
                        fpscr_after
                    );
                }
                if !same_kind
                    || float_mismatch
                    || self.tested.state.regs != regs_after
                    || (self.tested.state.cpsr ^ cpsr_after) & Self::CPSR_COMPARE_MASK != 0
                {
                    echo!(
                        "touchHLE::cpu: lockstep MISMATCH after {} steps, instruction {:08x?} at {:#x} ({})",
                        self.steps,
                        instruction,
                        pc,
                        if thumb { "Thumb" } else { "ARM" }
                    );
                    echo!("  result: interpreter {:?}, dynarmic {:?}", tested_result, reference_result);
                    echo!("  before:      {:08x?} cpsr {:08x}", regs_before, cpsr_before);
                    echo!("  interpreter: {:08x?} cpsr {:08x}", self.tested.state.regs, self.tested.state.cpsr);
                    echo!("  dynarmic:    {:08x?} cpsr {:08x}", regs_after, cpsr_after);
                    panic!("Lockstep mismatch, see log");
                }
            }
        }

        if self.steps % 20_000_000 == 0 {
            self.report();
        }
        reference_result
    }

    fn report(&self) {
        let mut missing: Vec<_> = self.unimplemented.iter().collect();
        missing.sort_by_key(|&(_, &count)| std::cmp::Reverse(count));
        echo!(
            "touchHLE::cpu: lockstep: {} steps, {} compared OK, not handled by interpreter: {:?}",
            self.steps,
            self.compared,
            missing
        );
    }
}
