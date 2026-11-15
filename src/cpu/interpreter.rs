/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! Portable interpreter CPU backend.
//!
//! This is a plain fetch-decode-execute interpreter for the user-mode subset
//! of ARMv6/ARMv7-A that apps use. It has no dependencies and generates no
//! code, so it works on hosts where a recompiler can't be used.
//!
//! Instructions that are not implemented are never guessed at: [InterpreterCpu::step]
//! reports them with a short description. The lockstep mode in [super] uses
//! that to list what an app needs, and to check every implemented instruction
//! against dynarmic.
//!
//! Resources:
//! - The ARM Architecture Reference Manual, ARMv7-A and ARMv7-R edition
//!   (instruction encodings and pseudocode).

mod thumb;
mod vfp;

use super::{CpuError, CpuState};
use crate::mem::{ConstVoidPtr, Mem, Ptr};

const FLAG_N: u32 = 1 << 31;
const FLAG_Z: u32 = 1 << 30;
const FLAG_C: u32 = 1 << 29;
const FLAG_V: u32 = 1 << 28;
const FLAG_Q: u32 = 1 << 27;
const FLAG_T: u32 = 1 << 5;

const PC: usize = 15;
const LR: usize = 14;

/// Everything that makes up the state of the CPU. This is also what is saved
/// and restored when switching threads.
pub struct State {
    /// r0-r15. The value stored for r15 is the address of the next
    /// instruction to execute, without any pipeline offset.
    pub regs: [u32; 16],
    pub cpsr: u32,
    /// VFP/NEON registers d0-d31. The single-precision registers s0-s31 are
    /// the halves of d0-d15.
    pub d: [u64; 32],
    pub fpscr: u32,
}
impl State {
    pub fn new() -> Self {
        State {
            regs: [0; 16],
            cpsr: super::Cpu::CPSR_USER_MODE,
            d: [0; 32],
            fpscr: 0,
        }
    }
}

pub struct InterpreterCpu {
    pub state: State,
}

/// How an instruction ended, if it did not fail.
enum Flow {
    /// Continue with the following instruction.
    Next,
    /// The instruction wrote to the PC itself.
    Branched,
    Svc(u32),
    Undefined,
    Breakpoint,
}

/// Why an instruction could not be completed.
enum Stop {
    /// Access to unmapped memory (in practice, the null page).
    Memory,
    /// The interpreter doesn't know this instruction (yet).
    Unimplemented(&'static str),
}
type Exec<T> = Result<T, Stop>;

fn read8(mem: &Mem, addr: u32) -> Exec<u8> {
    let ptr: ConstVoidPtr = Ptr::from_bits(addr);
    mem.get_bytes_fallible(ptr, 1)
        .map(|b| b[0])
        .ok_or(Stop::Memory)
}
fn read16(mem: &Mem, addr: u32) -> Exec<u16> {
    let ptr: ConstVoidPtr = Ptr::from_bits(addr);
    mem.get_bytes_fallible(ptr, 2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .ok_or(Stop::Memory)
}
fn read32(mem: &Mem, addr: u32) -> Exec<u32> {
    let ptr: ConstVoidPtr = Ptr::from_bits(addr);
    mem.get_bytes_fallible(ptr, 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .ok_or(Stop::Memory)
}
fn write8(mem: &mut Mem, addr: u32, value: u8) -> Exec<()> {
    let ptr: ConstVoidPtr = Ptr::from_bits(addr);
    mem.get_bytes_fallible_mut(ptr, 1)
        .map(|b| b[0] = value)
        .ok_or(Stop::Memory)
}
fn write16(mem: &mut Mem, addr: u32, value: u16) -> Exec<()> {
    let ptr: ConstVoidPtr = Ptr::from_bits(addr);
    mem.get_bytes_fallible_mut(ptr, 2)
        .map(|b| b.copy_from_slice(&value.to_le_bytes()))
        .ok_or(Stop::Memory)
}
fn write32(mem: &mut Mem, addr: u32, value: u32) -> Exec<()> {
    let ptr: ConstVoidPtr = Ptr::from_bits(addr);
    mem.get_bytes_fallible_mut(ptr, 4)
        .map(|b| b.copy_from_slice(&value.to_le_bytes()))
        .ok_or(Stop::Memory)
}

/// Extract a register number from an instruction.
fn field(instruction: u32, shift: u32) -> usize {
    ((instruction >> shift) & 0xF) as usize
}

/// `AddWithCarry()` from the manual: returns the result, carry and overflow.
fn add_with_carry(a: u32, b: u32, carry_in: bool) -> (u32, bool, bool) {
    let unsigned = u64::from(a) + u64::from(b) + u64::from(carry_in);
    let signed = i64::from(a as i32) + i64::from(b as i32) + i64::from(carry_in);
    let result = unsigned as u32;
    (
        result,
        unsigned != u64::from(result),
        signed != i64::from(result as i32),
    )
}

/// `Shift_C()` for an immediate shift amount, as encoded in instructions
/// (where an amount of zero selects special cases).
fn shift_imm(value: u32, shift_type: u32, amount: u32, carry_in: bool) -> (u32, bool) {
    match (shift_type, amount) {
        (0, 0) => (value, carry_in),
        (0, n) => (value << n, (value >> (32 - n)) & 1 != 0),
        // LSR #32
        (1, 0) => (0, value >> 31 != 0),
        (1, n) => (value >> n, (value >> (n - 1)) & 1 != 0),
        // ASR #32
        (2, 0) => (((value as i32) >> 31) as u32, value >> 31 != 0),
        (2, n) => (((value as i32) >> n) as u32, (value >> (n - 1)) & 1 != 0),
        // RRX
        (3, 0) => ((u32::from(carry_in) << 31) | (value >> 1), value & 1 != 0),
        (3, n) => (value.rotate_right(n), (value >> (n - 1)) & 1 != 0),
        _ => unreachable!(),
    }
}

/// `Shift_C()` for a shift amount taken from a register.
fn shift_reg(value: u32, shift_type: u32, amount: u32, carry_in: bool) -> (u32, bool) {
    let amount = amount & 0xFF;
    if amount == 0 {
        return (value, carry_in);
    }
    match shift_type {
        0 => match amount {
            1..=31 => (value << amount, (value >> (32 - amount)) & 1 != 0),
            32 => (0, value & 1 != 0),
            _ => (0, false),
        },
        1 => match amount {
            1..=31 => (value >> amount, (value >> (amount - 1)) & 1 != 0),
            32 => (0, value >> 31 != 0),
            _ => (0, false),
        },
        2 => match amount {
            1..=31 => (
                ((value as i32) >> amount) as u32,
                (value >> (amount - 1)) & 1 != 0,
            ),
            _ => (((value as i32) >> 31) as u32, value >> 31 != 0),
        },
        3 => {
            let result = value.rotate_right(amount & 31);
            (result, result >> 31 != 0)
        }
        _ => unreachable!(),
    }
}

/// Saturate a 64-bit value to the signed 32-bit range.
fn signed_sat(value: i64) -> (u32, bool) {
    if value > i64::from(i32::MAX) {
        (i32::MAX as u32, true)
    } else if value < i64::from(i32::MIN) {
        (i32::MIN as u32, true)
    } else {
        (value as u32, false)
    }
}

/// Used by the lockstep mode: is this an ARM instruction that reads memory
/// and then overwrites the same location, i.e. one that can't be executed
/// twice in a row with the same result?
pub fn reads_and_writes_same_memory(instruction: u32) -> bool {
    // SWP, SWPB
    instruction >> 28 != 0xF && instruction & 0x0FB00FF0 == 0x01000090
}

impl InterpreterCpu {
    pub fn new() -> Self {
        InterpreterCpu {
            state: State::new(),
        }
    }

    /// See [super::Cpu::run_or_step].
    #[must_use]
    pub fn run_or_step(&mut self, mem: &mut Mem, ticks: Option<&mut u64>) -> CpuState {
        match ticks {
            None => self.step_or_panic(mem),
            Some(ticks) => {
                while *ticks > 0 {
                    *ticks -= 1;
                    let state = self.step_or_panic(mem);
                    if !matches!(state, CpuState::Normal) {
                        return state;
                    }
                }
                CpuState::Normal
            }
        }
    }

    fn step_or_panic(&mut self, mem: &mut Mem) -> CpuState {
        match self.step(mem) {
            Ok(state) => state,
            Err(description) => panic!(
                "The interpreter CPU backend does not implement this instruction yet: {} at {:#x}",
                description, self.state.regs[PC]
            ),
        }
    }

    /// Execute a single instruction. If the instruction is not implemented, a
    /// description of it is returned and the state is left untouched.
    pub fn step(&mut self, mem: &mut Mem) -> Result<CpuState, &'static str> {
        let pc = self.state.regs[PC];
        let regs_before = self.state.regs;
        let cpsr_before = self.state.cpsr;
        // (how the instruction ended, its length in bytes)
        let result = if self.state.cpsr & FLAG_T != 0 {
            self.exec_thumb(mem, pc)
        } else {
            read32(mem, pc).and_then(|instruction| Ok((self.exec_arm(mem, pc, instruction)?, 4)))
        };
        match result {
            Ok((Flow::Next, length)) => {
                self.state.regs[PC] = pc.wrapping_add(length);
                Ok(CpuState::Normal)
            }
            Ok((Flow::Branched, _)) => Ok(CpuState::Normal),
            Ok((Flow::Svc(svc), length)) => {
                self.state.regs[PC] = pc.wrapping_add(length);
                Ok(CpuState::Svc(svc))
            }
            Ok((Flow::Undefined, _)) => Ok(CpuState::Error(CpuError::UndefinedInstruction)),
            Ok((Flow::Breakpoint, _)) => Ok(CpuState::Error(CpuError::Breakpoint)),
            Err(Stop::Memory) => {
                self.state.regs[PC] = pc;
                Ok(CpuState::Error(CpuError::MemoryError))
            }
            Err(Stop::Unimplemented(description)) => {
                self.state.regs = regs_before;
                self.state.cpsr = cpsr_before;
                Err(description)
            }
        }
    }

    fn flag(&self, flag: u32) -> bool {
        self.state.cpsr & flag != 0
    }
    fn set_flag(&mut self, flag: u32, value: bool) {
        if value {
            self.state.cpsr |= flag;
        } else {
            self.state.cpsr &= !flag;
        }
    }
    fn set_nz(&mut self, result: u32) {
        self.set_flag(FLAG_N, result >> 31 != 0);
        self.set_flag(FLAG_Z, result == 0);
    }

    fn condition_passed(&self, cond: u32) -> bool {
        let (n, z, c, v) = (
            self.flag(FLAG_N),
            self.flag(FLAG_Z),
            self.flag(FLAG_C),
            self.flag(FLAG_V),
        );
        let base = match cond >> 1 {
            0 => z,
            1 => c,
            2 => n,
            3 => v,
            4 => c && !z,
            5 => n == v,
            6 => n == v && !z,
            _ => return true,
        };
        base != (cond & 1 != 0)
    }

    /// Read a register as an ARM-mode instruction at `pc` sees it.
    fn reg_arm(&self, reg: usize, pc: u32) -> u32 {
        if reg == PC {
            pc.wrapping_add(8)
        } else {
            self.state.regs[reg]
        }
    }

    /// `BXWritePC()`: branch, selecting the instruction set from bit 0.
    fn bx_write_pc(&mut self, target: u32) {
        if target & 1 != 0 {
            self.state.cpsr |= FLAG_T;
            self.state.regs[PC] = target & !1;
        } else {
            self.state.cpsr &= !FLAG_T;
            self.state.regs[PC] = target & !3;
        }
    }

    fn exec_arm(&mut self, mem: &mut Mem, pc: u32, i: u32) -> Exec<Flow> {
        let cond = i >> 28;
        if cond == 0xF {
            return self.exec_arm_unconditional(pc, i);
        }
        if !self.condition_passed(cond) {
            return Ok(Flow::Next);
        }
        match (i >> 25) & 7 {
            0b000 => {
                if i & 0x0FFFFFF0 == 0x012FFF10 {
                    // BX
                    let target = self.reg_arm(field(i, 0), pc);
                    self.bx_write_pc(target);
                    Ok(Flow::Branched)
                } else if i & 0x0FFFFFF0 == 0x012FFF30 {
                    // BLX (register)
                    let target = self.reg_arm(field(i, 0), pc);
                    self.state.regs[LR] = pc.wrapping_add(4);
                    self.bx_write_pc(target);
                    Ok(Flow::Branched)
                } else if i & 0x0FFF0FF0 == 0x016F0F10 {
                    // CLZ
                    self.state.regs[field(i, 12)] = self.state.regs[field(i, 0)].leading_zeros();
                    Ok(Flow::Next)
                } else if i & 0x0FF000F0 == 0x01200070 {
                    Ok(Flow::Breakpoint)
                } else if i & 0x90 == 0x90 {
                    if i & 0x60 == 0 {
                        self.exec_arm_multiply_etc(mem, i)
                    } else {
                        self.exec_arm_extra_load_store(mem, pc, i)
                    }
                } else if i & 0x01900000 == 0x01000000 {
                    self.exec_arm_misc(i)
                } else {
                    self.exec_arm_data_processing(pc, i)
                }
            }
            0b001 => {
                if i & 0x0FF00000 == 0x03000000 {
                    // MOVW
                    self.state.regs[field(i, 12)] = ((i >> 4) & 0xF000) | (i & 0xFFF);
                    Ok(Flow::Next)
                } else if i & 0x0FF00000 == 0x03400000 {
                    // MOVT
                    let rd = field(i, 12);
                    let imm16 = ((i >> 4) & 0xF000) | (i & 0xFFF);
                    self.state.regs[rd] = (self.state.regs[rd] & 0xFFFF) | (imm16 << 16);
                    Ok(Flow::Next)
                } else if i & 0x0FB00000 == 0x03200000 {
                    // MSR (immediate), or hints (NOP etc) when the mask is 0
                    if i & (1 << 22) != 0 {
                        return Err(Stop::Unimplemented("MSR to SPSR"));
                    }
                    let imm = (i & 0xFF).rotate_right(((i >> 8) & 0xF) * 2);
                    self.write_apsr((i >> 16) & 0xF, imm);
                    Ok(Flow::Next)
                } else {
                    self.exec_arm_data_processing(pc, i)
                }
            }
            0b010 => self.exec_arm_load_store(mem, pc, i),
            0b011 => {
                if i & 0x10 == 0 {
                    self.exec_arm_load_store(mem, pc, i)
                } else {
                    self.exec_arm_media(i)
                }
            }
            0b100 => self.exec_arm_load_store_multiple(mem, pc, i),
            0b101 => {
                // B, BL
                let offset = (((i & 0x00FFFFFF) << 8) as i32 >> 6) as u32;
                if i & (1 << 24) != 0 {
                    self.state.regs[LR] = pc.wrapping_add(4);
                }
                self.state.regs[PC] = pc.wrapping_add(8).wrapping_add(offset);
                Ok(Flow::Branched)
            }
            0b110 => self.exec_arm_coprocessor(mem, pc, i),
            0b111 => {
                if i & (1 << 24) != 0 {
                    Ok(Flow::Svc(i & 0x00FFFFFF))
                } else {
                    self.exec_arm_coprocessor(mem, pc, i)
                }
            }
            _ => unreachable!(),
        }
    }

    fn exec_arm_unconditional(&mut self, pc: u32, i: u32) -> Exec<Flow> {
        if i & 0xFE000000 == 0xFA000000 {
            // BLX (immediate): always switches to Thumb
            let offset = (((i & 0x00FFFFFF) << 8) as i32 >> 6) as u32 | ((i >> 23) & 2);
            self.state.regs[LR] = pc.wrapping_add(4);
            self.state.regs[PC] = pc.wrapping_add(8).wrapping_add(offset);
            self.state.cpsr |= FLAG_T;
            Ok(Flow::Branched)
        } else if i & 0xFD30F000 == 0xF510F000 || i & 0xFF30F010 == 0xF710F000 {
            // PLD, PLDW (immediate/literal and register): hints
            Ok(Flow::Next)
        } else if i == 0xF57FF01F || i & 0xFFFFFFC0 == 0xF57FF040 {
            // CLREX; DSB, DMB, ISB
            Ok(Flow::Next)
        } else {
            Err(Stop::Unimplemented("ARM unconditional instruction"))
        }
    }

    fn write_apsr(&mut self, mask: u32, value: u32) {
        let mut bits = 0;
        if mask & 0b1000 != 0 {
            bits |= 0xF8000000; // NZCVQ
        }
        if mask & 0b0100 != 0 {
            bits |= 0x000F0000; // GE
        }
        // The other fields can't be written in user mode.
        self.state.cpsr = (self.state.cpsr & !bits) | (value & bits);
    }

    fn exec_arm_data_processing(&mut self, pc: u32, i: u32) -> Exec<Flow> {
        let opcode = (i >> 21) & 0xF;
        let set_flags = i & (1 << 20) != 0;
        let rn = field(i, 16);
        let rd = field(i, 12);
        let carry_in = self.flag(FLAG_C);

        let (operand2, shifter_carry) = if i & (1 << 25) != 0 {
            let rotation = ((i >> 8) & 0xF) * 2;
            let imm = (i & 0xFF).rotate_right(rotation);
            (imm, if rotation == 0 { carry_in } else { imm >> 31 != 0 })
        } else {
            let rm = self.reg_arm(field(i, 0), pc);
            let shift_type = (i >> 5) & 3;
            if i & 0x10 == 0 {
                shift_imm(rm, shift_type, (i >> 7) & 0x1F, carry_in)
            } else {
                shift_reg(rm, shift_type, self.state.regs[field(i, 8)], carry_in)
            }
        };
        let operand1 = self.reg_arm(rn, pc);

        // (result, Some((carry, overflow)) for arithmetic operations)
        let (result, arithmetic) = match opcode {
            0x0 | 0x8 => (operand1 & operand2, None),  // AND, TST
            0x1 | 0x9 => (operand1 ^ operand2, None),  // EOR, TEQ
            0x2 | 0xA => {
                // SUB, CMP
                let (r, c, v) = add_with_carry(operand1, !operand2, true);
                (r, Some((c, v)))
            }
            0x3 => {
                // RSB
                let (r, c, v) = add_with_carry(!operand1, operand2, true);
                (r, Some((c, v)))
            }
            0x4 | 0xB => {
                // ADD, CMN
                let (r, c, v) = add_with_carry(operand1, operand2, false);
                (r, Some((c, v)))
            }
            0x5 => {
                // ADC
                let (r, c, v) = add_with_carry(operand1, operand2, carry_in);
                (r, Some((c, v)))
            }
            0x6 => {
                // SBC
                let (r, c, v) = add_with_carry(operand1, !operand2, carry_in);
                (r, Some((c, v)))
            }
            0x7 => {
                // RSC
                let (r, c, v) = add_with_carry(!operand1, operand2, carry_in);
                (r, Some((c, v)))
            }
            0xC => (operand1 | operand2, None),  // ORR
            0xD => (operand2, None),             // MOV
            0xE => (operand1 & !operand2, None), // BIC
            0xF => (!operand2, None),            // MVN
            _ => unreachable!(),
        };

        let is_comparison = (0x8..=0xB).contains(&opcode);
        if !is_comparison && rd == PC {
            if set_flags {
                return Err(Stop::Unimplemented("exception return"));
            }
            // ALUWritePC(): interworking branch
            self.bx_write_pc(result);
            return Ok(Flow::Branched);
        }
        if set_flags {
            self.set_nz(result);
            match arithmetic {
                Some((carry, overflow)) => {
                    self.set_flag(FLAG_C, carry);
                    self.set_flag(FLAG_V, overflow);
                }
                None => self.set_flag(FLAG_C, shifter_carry),
            }
        }
        if !is_comparison {
            self.state.regs[rd] = result;
        }
        Ok(Flow::Next)
    }

    /// Multiplies, SWP and the exclusive load/store instructions: everything
    /// with bits 7:4 = 0b1001 in the first group.
    fn exec_arm_multiply_etc(&mut self, mem: &mut Mem, i: u32) -> Exec<Flow> {
        let set_flags = i & (1 << 20) != 0;
        let regs = &mut self.state.regs;
        match (i >> 21) & 0xF {
            op @ (0b0000 | 0b0001 | 0b0011) => {
                // MUL, MLA, MLS
                let (rd, ra, rm, rn) = (field(i, 16), field(i, 12), field(i, 8), field(i, 0));
                let product = regs[rn].wrapping_mul(regs[rm]);
                let result = match op {
                    0b0000 => product,
                    0b0001 => product.wrapping_add(regs[ra]),
                    _ => regs[ra].wrapping_sub(product),
                };
                regs[rd] = result;
                if set_flags && op != 0b0011 {
                    self.set_nz(result);
                }
            }
            0b0010 => {
                // UMAAL
                let (rdhi, rdlo, rm, rn) = (field(i, 16), field(i, 12), field(i, 8), field(i, 0));
                let result = u64::from(regs[rn]) * u64::from(regs[rm])
                    + u64::from(regs[rdhi])
                    + u64::from(regs[rdlo]);
                regs[rdhi] = (result >> 32) as u32;
                regs[rdlo] = result as u32;
            }
            op @ 0b0100..=0b0111 => {
                // UMULL, UMLAL, SMULL, SMLAL
                let (rdhi, rdlo, rm, rn) = (field(i, 16), field(i, 12), field(i, 8), field(i, 0));
                let product = if op & 0b10 != 0 {
                    (i64::from(regs[rn] as i32) * i64::from(regs[rm] as i32)) as u64
                } else {
                    u64::from(regs[rn]) * u64::from(regs[rm])
                };
                let result = if op & 1 != 0 {
                    product.wrapping_add((u64::from(regs[rdhi]) << 32) | u64::from(regs[rdlo]))
                } else {
                    product
                };
                regs[rdhi] = (result >> 32) as u32;
                regs[rdlo] = result as u32;
                if set_flags {
                    self.set_flag(FLAG_N, result >> 63 != 0);
                    self.set_flag(FLAG_Z, result == 0);
                }
            }
            0b1000 | 0b1010 => {
                // SWP, SWPB
                let (rn, rt, rt2) = (field(i, 16), field(i, 12), field(i, 0));
                let addr = regs[rn];
                if i & (1 << 22) != 0 {
                    let old = read8(mem, addr)?;
                    write8(mem, addr, regs[rt2] as u8)?;
                    regs[rt] = old.into();
                } else {
                    let old = read32(mem, addr)?;
                    write32(mem, addr, regs[rt2])?;
                    regs[rt] = old;
                }
            }
            0b1100..=0b1111 => {
                // LDREX, STREX and their byte/halfword/doubleword variants.
                // touchHLE runs all guest threads on a single host thread and
                // never preempts in the middle of such a sequence, so the
                // store always succeeds.
                let rn = field(i, 16);
                let addr = regs[rn];
                let size = (i >> 21) & 3;
                if i & (1 << 20) != 0 {
                    let rt = field(i, 12);
                    match size {
                        0b00 => regs[rt] = read32(mem, addr)?,
                        0b01 => {
                            regs[rt] = read32(mem, addr)?;
                            regs[rt + 1] = read32(mem, addr.wrapping_add(4))?;
                        }
                        0b10 => regs[rt] = read8(mem, addr)?.into(),
                        _ => regs[rt] = read16(mem, addr)?.into(),
                    }
                } else {
                    let (rd, rt) = (field(i, 12), field(i, 0));
                    match size {
                        0b00 => write32(mem, addr, regs[rt])?,
                        0b01 => {
                            write32(mem, addr, regs[rt])?;
                            write32(mem, addr.wrapping_add(4), regs[rt + 1])?;
                        }
                        0b10 => write8(mem, addr, regs[rt] as u8)?,
                        _ => write16(mem, addr, regs[rt] as u16)?,
                    }
                    regs[rd] = 0;
                }
            }
            _ => return Ok(Flow::Undefined),
        }
        Ok(Flow::Next)
    }

    /// Miscellaneous instructions: the data-processing encodings for the
    /// comparison opcodes with S = 0.
    fn exec_arm_misc(&mut self, i: u32) -> Exec<Flow> {
        if i & 0x0FBF0FFF == 0x010F0000 {
            // MRS
            if i & (1 << 22) != 0 {
                return Err(Stop::Unimplemented("MRS from SPSR"));
            }
            self.state.regs[field(i, 12)] = self.state.cpsr & 0xF80F0000;
            return Ok(Flow::Next);
        }
        if i & 0x0FB0FFF0 == 0x0120F000 {
            // MSR (register)
            if i & (1 << 22) != 0 {
                return Err(Stop::Unimplemented("MSR to SPSR"));
            }
            let value = self.state.regs[field(i, 0)];
            self.write_apsr((i >> 16) & 0xF, value);
            return Ok(Flow::Next);
        }
        let op = (i >> 21) & 3;
        let regs = &mut self.state.regs;
        if i & 0xF0 == 0x50 {
            // QADD, QSUB, QDADD, QDSUB
            let (rn, rd, rm) = (field(i, 16), field(i, 12), field(i, 0));
            let a = i64::from(regs[rm] as i32);
            let mut b = i64::from(regs[rn] as i32);
            let mut saturated = false;
            if op & 0b10 != 0 {
                let (doubled, sat) = signed_sat(b * 2);
                b = i64::from(doubled as i32);
                saturated |= sat;
            }
            let (result, sat) = signed_sat(if op & 1 != 0 { a - b } else { a + b });
            saturated |= sat;
            regs[rd] = result;
            if saturated {
                self.state.cpsr |= FLAG_Q;
            }
            return Ok(Flow::Next);
        }
        if i & 0x90 == 0x80 {
            // Signed 16-bit multiplies
            let (rd, ra, rm, rn) = (field(i, 16), field(i, 12), field(i, 8), field(i, 0));
            let half = |value: u32, high: bool| -> i64 {
                (if high { (value >> 16) as i16 } else { value as i16 }).into()
            };
            let x_high = i & (1 << 5) != 0;
            let y_high = i & (1 << 6) != 0;
            let mut overflowed = false;
            match op {
                0b00 => {
                    // SMLAxy
                    let product = half(regs[rn], x_high) * half(regs[rm], y_high);
                    let result = product + i64::from(regs[ra] as i32);
                    overflowed = result != i64::from(result as i32);
                    regs[rd] = result as u32;
                }
                0b01 => {
                    // SMLAWy, SMULWy
                    let product = (i64::from(regs[rn] as i32) * half(regs[rm], y_high)) >> 16;
                    if x_high {
                        regs[rd] = product as u32;
                    } else {
                        let result = product + i64::from(regs[ra] as i32);
                        overflowed = result != i64::from(result as i32);
                        regs[rd] = result as u32;
                    }
                }
                0b10 => {
                    // SMLALxy (rd is RdHi, ra is RdLo)
                    let product = half(regs[rn], x_high) * half(regs[rm], y_high);
                    let accumulator = ((u64::from(regs[rd]) << 32) | u64::from(regs[ra])) as i64;
                    let result = accumulator.wrapping_add(product) as u64;
                    regs[rd] = (result >> 32) as u32;
                    regs[ra] = result as u32;
                }
                _ => {
                    // SMULxy
                    regs[rd] = (half(regs[rn], x_high) * half(regs[rm], y_high)) as u32;
                }
            }
            if overflowed {
                self.state.cpsr |= FLAG_Q;
            }
            return Ok(Flow::Next);
        }
        Err(Stop::Unimplemented("ARM miscellaneous instruction"))
    }

    /// LDR, STR, LDRB, STRB with an immediate or (scaled) register offset.
    fn exec_arm_load_store(&mut self, mem: &mut Mem, pc: u32, i: u32) -> Exec<Flow> {
        let pre_index = i & (1 << 24) != 0;
        let add = i & (1 << 23) != 0;
        let byte = i & (1 << 22) != 0;
        let writeback = !pre_index || i & (1 << 21) != 0;
        let load = i & (1 << 20) != 0;
        let rn = field(i, 16);
        let rt = field(i, 12);

        let offset = if i & (1 << 25) == 0 {
            i & 0xFFF
        } else {
            let rm = self.state.regs[field(i, 0)];
            shift_imm(rm, (i >> 5) & 3, (i >> 7) & 0x1F, self.flag(FLAG_C)).0
        };
        let base = self.reg_arm(rn, pc);
        let offset_addr = if add {
            base.wrapping_add(offset)
        } else {
            base.wrapping_sub(offset)
        };
        let addr = if pre_index { offset_addr } else { base };

        if load {
            let value = if byte {
                read8(mem, addr)?.into()
            } else {
                read32(mem, addr)?
            };
            if writeback {
                self.state.regs[rn] = offset_addr;
            }
            if rt == PC {
                // LoadWritePC()
                self.bx_write_pc(value);
                return Ok(Flow::Branched);
            }
            self.state.regs[rt] = value;
        } else {
            let value = self.reg_arm(rt, pc);
            if byte {
                write8(mem, addr, value as u8)?;
            } else {
                write32(mem, addr, value)?;
            }
            if writeback {
                self.state.regs[rn] = offset_addr;
            }
        }
        Ok(Flow::Next)
    }

    /// LDRH, STRH, LDRSB, LDRSH, LDRD, STRD.
    fn exec_arm_extra_load_store(&mut self, mem: &mut Mem, pc: u32, i: u32) -> Exec<Flow> {
        let pre_index = i & (1 << 24) != 0;
        let add = i & (1 << 23) != 0;
        let writeback = !pre_index || i & (1 << 21) != 0;
        let load = i & (1 << 20) != 0;
        let rn = field(i, 16);
        let rt = field(i, 12);

        let offset = if i & (1 << 22) != 0 {
            ((i >> 4) & 0xF0) | (i & 0xF)
        } else {
            self.state.regs[field(i, 0)]
        };
        let base = self.reg_arm(rn, pc);
        let offset_addr = if add {
            base.wrapping_add(offset)
        } else {
            base.wrapping_sub(offset)
        };
        let addr = if pre_index { offset_addr } else { base };

        match ((i >> 5) & 3, load) {
            (0b01, false) => write16(mem, addr, self.state.regs[rt] as u16)?,
            (0b01, true) => self.state.regs[rt] = read16(mem, addr)?.into(),
            (0b10, true) => self.state.regs[rt] = read8(mem, addr)? as i8 as u32,
            (0b11, true) => self.state.regs[rt] = read16(mem, addr)? as i16 as u32,
            (0b10, false) => {
                // LDRD
                let low = read32(mem, addr)?;
                let high = read32(mem, addr.wrapping_add(4))?;
                self.state.regs[rt] = low;
                self.state.regs[rt + 1] = high;
            }
            (0b11, false) => {
                // STRD
                write32(mem, addr, self.state.regs[rt])?;
                write32(mem, addr.wrapping_add(4), self.state.regs[rt + 1])?;
            }
            _ => unreachable!(),
        }
        // For a load, the loaded value wins if the base register is also the
        // destination.
        let base_was_loaded = load && rt == rn || (i >> 5) & 3 == 0b10 && !load && rt + 1 == rn;
        if writeback && !base_was_loaded {
            self.state.regs[rn] = offset_addr;
        }
        Ok(Flow::Next)
    }

    /// LDM, STM (all addressing modes), which includes PUSH and POP.
    fn exec_arm_load_store_multiple(&mut self, mem: &mut Mem, pc: u32, i: u32) -> Exec<Flow> {
        let before = i & (1 << 24) != 0;
        let increment = i & (1 << 23) != 0;
        if i & (1 << 22) != 0 {
            return Err(Stop::Unimplemented("LDM/STM of user registers"));
        }
        let writeback = i & (1 << 21) != 0;
        let load = i & (1 << 20) != 0;
        let rn = field(i, 16);
        let list = i & 0xFFFF;
        let count = list.count_ones();

        let base = self.state.regs[rn];
        // The registers always go to/from ascending addresses.
        let mut addr = match (increment, before) {
            (true, false) => base,
            (true, true) => base.wrapping_add(4),
            (false, false) => base.wrapping_sub(4 * count).wrapping_add(4),
            (false, true) => base.wrapping_sub(4 * count),
        };
        let new_base = if increment {
            base.wrapping_add(4 * count)
        } else {
            base.wrapping_sub(4 * count)
        };

        let mut branch_target = None;
        for reg in 0..16 {
            if list & (1 << reg) == 0 {
                continue;
            }
            if load {
                let value = read32(mem, addr)?;
                if reg == PC {
                    branch_target = Some(value);
                } else {
                    self.state.regs[reg] = value;
                }
            } else {
                write32(mem, addr, self.reg_arm(reg, pc))?;
            }
            addr = addr.wrapping_add(4);
        }
        if writeback && !(load && list & (1 << rn) != 0) {
            self.state.regs[rn] = new_base;
        }
        if let Some(target) = branch_target {
            // LoadWritePC()
            self.bx_write_pc(target);
            return Ok(Flow::Branched);
        }
        Ok(Flow::Next)
    }

    /// The "media" instructions: byte reversal, sign/zero extension, bitfields.
    fn exec_arm_media(&mut self, i: u32) -> Exec<Flow> {
        if i & 0x0FF000F0 == 0x07F000F0 {
            // UDF: permanently undefined
            return Ok(Flow::Undefined);
        }
        let regs = &mut self.state.regs;
        let rd = field(i, 12);
        if i & 0x0F8000F0 == 0x06800070 {
            // SXTB, SXTH, UXTB, UXTH, and the variants that also add (when
            // Rn isn't 15)
            let rotated = regs[field(i, 0)].rotate_right(((i >> 10) & 3) * 8);
            let extended = match (i >> 20) & 7 {
                0b010 => rotated as i8 as u32,
                0b011 => rotated as i16 as u32,
                0b110 => rotated & 0xFF,
                0b111 => rotated & 0xFFFF,
                _ => return Err(Stop::Unimplemented("SXTB16/UXTB16")),
            };
            let rn = field(i, 16);
            regs[rd] = if rn == PC {
                extended
            } else {
                regs[rn].wrapping_add(extended)
            };
            return Ok(Flow::Next);
        }
        let rm = regs[field(i, 0)];
        match i & 0x0FFF0FF0 {
            0x06BF0F30 => regs[rd] = rm.swap_bytes(), // REV
            0x06BF0FB0 => regs[rd] = ((rm >> 8) & 0x00FF00FF) | ((rm << 8) & 0xFF00FF00), // REV16
            0x06FF0FB0 => regs[rd] = (rm as u16).swap_bytes() as i16 as u32, // REVSH
            0x06FF0F30 => regs[rd] = rm.reverse_bits(), // RBIT
            _ => {
                let lsb = (i >> 7) & 0x1F;
                match i & 0x0FE00070 {
                    0x07C00010 => {
                        // BFC, BFI
                        let msb = (i >> 16) & 0x1F;
                        if msb < lsb {
                            return Err(Stop::Unimplemented("BFI with msb < lsb"));
                        }
                        let width = msb - lsb + 1;
                        let mask = (u32::MAX >> (32 - width)) << lsb;
                        let rn = field(i, 0);
                        let insert = if rn == PC { 0 } else { regs[rn] << lsb };
                        regs[rd] = (regs[rd] & !mask) | (insert & mask);
                    }
                    op @ (0x07E00050 | 0x07A00050) => {
                        // UBFX, SBFX
                        let width = ((i >> 16) & 0x1F) + 1;
                        if lsb + width > 32 {
                            return Err(Stop::Unimplemented("bitfield extract out of range"));
                        }
                        let value = regs[field(i, 0)];
                        let shifted = value << (32 - lsb - width);
                        regs[rd] = if op == 0x07A00050 {
                            ((shifted as i32) >> (32 - width)) as u32
                        } else {
                            shifted >> (32 - width)
                        };
                    }
                    _ => return Err(Stop::Unimplemented("ARM media instruction")),
                }
            }
        }
        Ok(Flow::Next)
    }

    fn exec_arm_coprocessor(&mut self, mem: &mut Mem, pc: u32, i: u32) -> Exec<Flow> {
        match (i >> 8) & 0xF {
            10 | 11 => self.exec_vfp(mem, pc.wrapping_add(8), i),
            15 if i & 0x0FFF0FFF == 0x0E070FBA => {
                // mcr p15, 0, <Rt>, c7, c10, 5: the pre-ARMv7 encoding of a
                // memory barrier. Nothing to do.
                Ok(Flow::Next)
            }
            _ => Err(Stop::Unimplemented("coprocessor instruction")),
        }
    }
}
