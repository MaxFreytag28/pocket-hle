/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! Thumb and Thumb-2 instructions for the interpreter.
//!
//! Thumb instructions are 16 bits wide. Thumb-2 adds 32-bit instructions made
//! of two 16-bit halves (`hw1`, `hw2`), and the IT instruction, which makes
//! up to four following instructions conditional.

use super::{
    add_with_carry, field, read16, read32, read8, shift_imm, shift_reg, write16, write32, write8,
    Exec, Flow, InterpreterCpu, Stop, FLAG_C, FLAG_Q, FLAG_T, FLAG_V, LR, PC,
};
use crate::mem::Mem;

const SP: usize = 13;

/// `ThumbExpandImm_C()`: returns the value and the carry out.
fn expand_imm(imm12: u32, carry_in: bool) -> (u32, bool) {
    if imm12 >> 10 == 0 {
        let imm8 = imm12 & 0xFF;
        let value = match (imm12 >> 8) & 3 {
            0b00 => imm8,
            0b01 => (imm8 << 16) | imm8,
            0b10 => (imm8 << 24) | (imm8 << 8),
            _ => (imm8 << 24) | (imm8 << 16) | (imm8 << 8) | imm8,
        };
        (value, carry_in)
    } else {
        let unrotated = 0x80 | (imm12 & 0x7F);
        let value = unrotated.rotate_right(imm12 >> 7);
        (value, value >> 31 != 0)
    }
}

/// Sign-extend the low `bits` bits of `value`.
fn sign_extend(value: u32, bits: u32) -> u32 {
    (((value << (32 - bits)) as i32) >> (32 - bits)) as u32
}

impl InterpreterCpu {
    fn it_state(&self) -> u32 {
        ((self.state.cpsr >> 8) & 0xFC) | ((self.state.cpsr >> 25) & 3)
    }
    fn set_it_state(&mut self, it: u32) {
        self.state.cpsr =
            (self.state.cpsr & !0x0600FC00) | ((it & 0xFC) << 8) | ((it & 3) << 25);
    }

    /// Read a register as a Thumb-mode instruction at `pc` sees it.
    fn reg_thumb(&self, reg: usize, pc: u32) -> u32 {
        if reg == PC {
            pc.wrapping_add(4)
        } else {
            self.state.regs[reg]
        }
    }

    /// `BranchWritePC()` in Thumb mode.
    fn branch_thumb(&mut self, target: u32) {
        self.state.regs[PC] = target & !1;
    }

    /// Execute the Thumb instruction at `pc`. Returns how it ended and its
    /// length in bytes.
    pub(super) fn exec_thumb(&mut self, mem: &mut Mem, pc: u32) -> Exec<(Flow, u32)> {
        let hw1 = u32::from(read16(mem, pc)?);
        let is_32bit = hw1 >> 11 >= 0b11101;
        let length = if is_32bit { 4 } else { 2 };

        // Instructions in an IT block are conditional, and each one advances
        // the IT state.
        let it = self.it_state();
        let in_it_block = it != 0;
        if in_it_block {
            let condition_passed = self.condition_passed(it >> 4);
            self.set_it_state(if it & 7 == 0 {
                0
            } else {
                (it & 0xE0) | ((it << 1) & 0x1F)
            });
            if !condition_passed {
                return Ok((Flow::Next, length));
            }
        }

        let flow = if is_32bit {
            let hw2 = u32::from(read16(mem, pc.wrapping_add(2))?);
            self.exec_thumb32(mem, pc, hw1, hw2)?
        } else {
            self.exec_thumb16(mem, pc, hw1, !in_it_block)?
        };
        Ok((flow, length))
    }

    /// 16-bit instructions. Most of the data-processing ones set the flags,
    /// except inside an IT block, hence `set_flags`.
    fn exec_thumb16(&mut self, mem: &mut Mem, pc: u32, i: u32, set_flags: bool) -> Exec<Flow> {
        let carry_in = self.flag(FLAG_C);
        let rd = (i & 7) as usize;
        let rm3 = ((i >> 3) & 7) as usize;
        match i >> 10 {
            0b000000..=0b000101 => {
                // LSL, LSR, ASR (immediate); LSL #0 is MOV
                let (result, carry) =
                    shift_imm(self.state.regs[rm3], i >> 11, (i >> 6) & 0x1F, carry_in);
                self.state.regs[rd] = result;
                if set_flags {
                    self.set_nz(result);
                    self.set_flag(FLAG_C, carry);
                }
            }
            0b000110 | 0b000111 => {
                // ADD, SUB (register or 3-bit immediate)
                let operand = if i & (1 << 10) != 0 {
                    (i >> 6) & 7
                } else {
                    self.state.regs[((i >> 6) & 7) as usize]
                };
                let (result, carry, overflow) = if i & (1 << 9) != 0 {
                    add_with_carry(self.state.regs[rm3], !operand, true)
                } else {
                    add_with_carry(self.state.regs[rm3], operand, false)
                };
                self.state.regs[rd] = result;
                if set_flags {
                    self.set_nz(result);
                    self.set_flag(FLAG_C, carry);
                    self.set_flag(FLAG_V, overflow);
                }
            }
            0b001000..=0b001111 => {
                // MOV, CMP, ADD, SUB (8-bit immediate)
                let rdn = ((i >> 8) & 7) as usize;
                let imm8 = i & 0xFF;
                let value = self.state.regs[rdn];
                match (i >> 11) & 3 {
                    0b00 => {
                        self.state.regs[rdn] = imm8;
                        if set_flags {
                            self.set_nz(imm8);
                        }
                    }
                    op => {
                        let (result, carry, overflow) = if op == 0b10 {
                            add_with_carry(value, imm8, false)
                        } else {
                            add_with_carry(value, !imm8, true)
                        };
                        // CMP always sets the flags.
                        if op == 0b01 || set_flags {
                            self.set_nz(result);
                            self.set_flag(FLAG_C, carry);
                            self.set_flag(FLAG_V, overflow);
                        }
                        if op != 0b01 {
                            self.state.regs[rdn] = result;
                        }
                    }
                }
            }
            0b010000 => {
                // Data processing on low registers
                let a = self.state.regs[rd];
                let b = self.state.regs[rm3];
                let op = (i >> 6) & 0xF;
                // (result, carry, overflow, write result?)
                let (result, carry, overflow, write) = match op {
                    0x0 => (a & b, None, None, true),  // AND
                    0x1 => (a ^ b, None, None, true),  // EOR
                    0x2 | 0x3 | 0x4 | 0x7 => {
                        // LSL, LSR, ASR, ROR (register)
                        let shift_type = match op {
                            0x2 => 0,
                            0x3 => 1,
                            0x4 => 2,
                            _ => 3,
                        };
                        let (r, c) = shift_reg(a, shift_type, b, carry_in);
                        (r, Some(c), None, true)
                    }
                    0x5 => {
                        // ADC
                        let (r, c, v) = add_with_carry(a, b, carry_in);
                        (r, Some(c), Some(v), true)
                    }
                    0x6 => {
                        // SBC
                        let (r, c, v) = add_with_carry(a, !b, carry_in);
                        (r, Some(c), Some(v), true)
                    }
                    0x8 => (a & b, None, None, false), // TST
                    0x9 => {
                        // RSB from zero (NEG)
                        let (r, c, v) = add_with_carry(!b, 0, true);
                        (r, Some(c), Some(v), true)
                    }
                    0xA => {
                        // CMP
                        let (r, c, v) = add_with_carry(a, !b, true);
                        (r, Some(c), Some(v), false)
                    }
                    0xB => {
                        // CMN
                        let (r, c, v) = add_with_carry(a, b, false);
                        (r, Some(c), Some(v), false)
                    }
                    0xC => (a | b, None, None, true),              // ORR
                    0xD => (a.wrapping_mul(b), None, None, true),  // MUL
                    0xE => (a & !b, None, None, true),             // BIC
                    0xF => (!b, None, None, true),                 // MVN
                    _ => unreachable!(),
                };
                if set_flags || !write {
                    self.set_nz(result);
                    if let Some(carry) = carry {
                        self.set_flag(FLAG_C, carry);
                    }
                    if let Some(overflow) = overflow {
                        self.set_flag(FLAG_V, overflow);
                    }
                }
                if write {
                    self.state.regs[rd] = result;
                }
            }
            0b010001 => {
                // Operations that can use the high registers, and BX/BLX
                let rm = ((i >> 3) & 0xF) as usize;
                let rdn = (((i >> 4) & 8) | (i & 7)) as usize;
                let rm_value = self.reg_thumb(rm, pc);
                match (i >> 8) & 3 {
                    0b00 => {
                        // ADD
                        let result = self.reg_thumb(rdn, pc).wrapping_add(rm_value);
                        if rdn == PC {
                            self.branch_thumb(result);
                            return Ok(Flow::Branched);
                        }
                        self.state.regs[rdn] = result;
                    }
                    0b01 => {
                        // CMP
                        let (result, carry, overflow) =
                            add_with_carry(self.reg_thumb(rdn, pc), !rm_value, true);
                        self.set_nz(result);
                        self.set_flag(FLAG_C, carry);
                        self.set_flag(FLAG_V, overflow);
                    }
                    0b10 => {
                        // MOV
                        if rdn == PC {
                            self.branch_thumb(rm_value);
                            return Ok(Flow::Branched);
                        }
                        self.state.regs[rdn] = rm_value;
                    }
                    _ => {
                        // BX, BLX
                        if i & (1 << 7) != 0 {
                            self.state.regs[LR] = pc.wrapping_add(2) | 1;
                        }
                        self.bx_write_pc(rm_value);
                        return Ok(Flow::Branched);
                    }
                }
            }
            0b010010 | 0b010011 => {
                // LDR (literal)
                let rt = ((i >> 8) & 7) as usize;
                let addr = (pc.wrapping_add(4) & !3).wrapping_add((i & 0xFF) * 4);
                self.state.regs[rt] = read32(mem, addr)?;
            }
            0b010100..=0b010111 => {
                // Loads and stores with a register offset
                let rn = self.state.regs[rm3];
                let addr = rn.wrapping_add(self.state.regs[((i >> 6) & 7) as usize]);
                let rt = rd;
                match (i >> 9) & 7 {
                    0b000 => write32(mem, addr, self.state.regs[rt])?,
                    0b001 => write16(mem, addr, self.state.regs[rt] as u16)?,
                    0b010 => write8(mem, addr, self.state.regs[rt] as u8)?,
                    0b011 => self.state.regs[rt] = read8(mem, addr)? as i8 as u32,
                    0b100 => self.state.regs[rt] = read32(mem, addr)?,
                    0b101 => self.state.regs[rt] = read16(mem, addr)?.into(),
                    0b110 => self.state.regs[rt] = read8(mem, addr)?.into(),
                    _ => self.state.regs[rt] = read16(mem, addr)? as i16 as u32,
                }
            }
            0b011000..=0b011111 => {
                // STR, LDR, STRB, LDRB (5-bit immediate offset)
                let imm5 = (i >> 6) & 0x1F;
                let byte = i & (1 << 12) != 0;
                let load = i & (1 << 11) != 0;
                let base = self.state.regs[rm3];
                let addr = base.wrapping_add(if byte { imm5 } else { imm5 * 4 });
                match (byte, load) {
                    (false, false) => write32(mem, addr, self.state.regs[rd])?,
                    (false, true) => self.state.regs[rd] = read32(mem, addr)?,
                    (true, false) => write8(mem, addr, self.state.regs[rd] as u8)?,
                    (true, true) => self.state.regs[rd] = read8(mem, addr)?.into(),
                }
            }
            0b100000..=0b100011 => {
                // STRH, LDRH (immediate offset)
                let addr = self.state.regs[rm3].wrapping_add(((i >> 6) & 0x1F) * 2);
                if i & (1 << 11) != 0 {
                    self.state.regs[rd] = read16(mem, addr)?.into();
                } else {
                    write16(mem, addr, self.state.regs[rd] as u16)?;
                }
            }
            0b100100..=0b100111 => {
                // STR, LDR (SP-relative)
                let rt = ((i >> 8) & 7) as usize;
                let addr = self.state.regs[SP].wrapping_add((i & 0xFF) * 4);
                if i & (1 << 11) != 0 {
                    self.state.regs[rt] = read32(mem, addr)?;
                } else {
                    write32(mem, addr, self.state.regs[rt])?;
                }
            }
            0b101000..=0b101011 => {
                // ADR; ADD (SP plus immediate)
                let rt = ((i >> 8) & 7) as usize;
                let base = if i & (1 << 11) != 0 {
                    self.state.regs[SP]
                } else {
                    pc.wrapping_add(4) & !3
                };
                self.state.regs[rt] = base.wrapping_add((i & 0xFF) * 4);
            }
            0b101100..=0b101111 => return self.exec_thumb16_misc(mem, pc, i),
            0b110000..=0b110011 => {
                // STMIA, LDMIA
                let rn = ((i >> 8) & 7) as usize;
                let list = i & 0xFF;
                let load = i & (1 << 11) != 0;
                let mut addr = self.state.regs[rn];
                for reg in 0..8 {
                    if list & (1 << reg) == 0 {
                        continue;
                    }
                    if load {
                        self.state.regs[reg] = read32(mem, addr)?;
                    } else {
                        write32(mem, addr, self.state.regs[reg])?;
                    }
                    addr = addr.wrapping_add(4);
                }
                if !(load && list & (1 << rn) != 0) {
                    self.state.regs[rn] = addr;
                }
            }
            0b110100..=0b110111 => {
                let cond = (i >> 8) & 0xF;
                match cond {
                    0xE => return Ok(Flow::Undefined),
                    0xF => return Ok(Flow::Svc(i & 0xFF)),
                    _ => {
                        // B (conditional)
                        if self.condition_passed(cond) {
                            let offset = sign_extend((i & 0xFF) << 1, 9);
                            self.branch_thumb(pc.wrapping_add(4).wrapping_add(offset));
                            return Ok(Flow::Branched);
                        }
                    }
                }
            }
            0b111000 | 0b111001 => {
                // B
                let offset = sign_extend((i & 0x7FF) << 1, 12);
                self.branch_thumb(pc.wrapping_add(4).wrapping_add(offset));
                return Ok(Flow::Branched);
            }
            _ => unreachable!("32-bit instruction in exec_thumb16"),
        }
        Ok(Flow::Next)
    }

    /// The miscellaneous 16-bit instructions (`1011 xxxx ...`).
    fn exec_thumb16_misc(&mut self, mem: &mut Mem, pc: u32, i: u32) -> Exec<Flow> {
        let rd = (i & 7) as usize;
        let rm = ((i >> 3) & 7) as usize;
        match (i >> 8) & 0xF {
            0b0000 => {
                // ADD, SUB (SP and immediate)
                let imm = (i & 0x7F) * 4;
                let sp = self.state.regs[SP];
                self.state.regs[SP] = if i & (1 << 7) != 0 {
                    sp.wrapping_sub(imm)
                } else {
                    sp.wrapping_add(imm)
                };
            }
            0b0001 | 0b0011 | 0b1001 | 0b1011 => {
                // CBZ, CBNZ
                let nonzero = i & (1 << 11) != 0;
                if (self.state.regs[rd] != 0) == nonzero {
                    let offset = ((i >> 3) & 0x40) | ((i >> 2) & 0x3E);
                    self.branch_thumb(pc.wrapping_add(4).wrapping_add(offset));
                    return Ok(Flow::Branched);
                }
            }
            0b0010 => {
                // SXTH, SXTB, UXTH, UXTB
                let value = self.state.regs[rm];
                self.state.regs[rd] = match (i >> 6) & 3 {
                    0b00 => value as i16 as u32,
                    0b01 => value as i8 as u32,
                    0b10 => value & 0xFFFF,
                    _ => value & 0xFF,
                };
            }
            0b0100 | 0b0101 => {
                // PUSH
                let list = (i & 0xFF) | ((i & 0x100) << 6);
                let count = list.count_ones();
                let mut addr = self.state.regs[SP].wrapping_sub(4 * count);
                let new_sp = addr;
                for reg in 0..15 {
                    if list & (1 << reg) != 0 {
                        write32(mem, addr, self.state.regs[reg])?;
                        addr = addr.wrapping_add(4);
                    }
                }
                self.state.regs[SP] = new_sp;
            }
            0b1100 | 0b1101 => {
                // POP
                let list = (i & 0xFF) | ((i & 0x100) << 7);
                let mut addr = self.state.regs[SP];
                let mut branch_target = None;
                for reg in 0..16 {
                    if list & (1 << reg) == 0 {
                        continue;
                    }
                    let value = read32(mem, addr)?;
                    if reg == PC {
                        branch_target = Some(value);
                    } else {
                        self.state.regs[reg] = value;
                    }
                    addr = addr.wrapping_add(4);
                }
                self.state.regs[SP] = addr;
                if let Some(target) = branch_target {
                    self.bx_write_pc(target);
                    return Ok(Flow::Branched);
                }
            }
            0b1010 => {
                // REV, REV16, REVSH
                let value = self.state.regs[rm];
                self.state.regs[rd] = match (i >> 6) & 3 {
                    0b00 => value.swap_bytes(),
                    0b01 => ((value >> 8) & 0x00FF00FF) | ((value << 8) & 0xFF00FF00),
                    0b11 => (value as u16).swap_bytes() as i16 as u32,
                    _ => return Ok(Flow::Undefined),
                };
            }
            0b1110 => return Ok(Flow::Breakpoint),
            0b1111 => {
                // IT, or hints (NOP, YIELD, ...) when the mask is zero
                if i & 0xF != 0 {
                    self.set_it_state(i & 0xFF);
                }
            }
            _ => return Err(Stop::Unimplemented("Thumb miscellaneous instruction")),
        }
        Ok(Flow::Next)
    }

    /// The data-processing operations shared by the "modified immediate" and
    /// "shifted register" forms. `a` is the first operand, `b` the second.
    /// Sets the flags if asked and returns the result.
    fn thumb2_data_processing(
        &mut self,
        op: u32,
        set_flags: bool,
        a: u32,
        b: u32,
        shifter_carry: bool,
    ) -> Exec<u32> {
        let carry_in = self.flag(FLAG_C);
        let (result, arithmetic) = match op {
            0b0000 => (a & b, None),
            0b0001 => (a & !b, None), // BIC
            0b0010 => (a | b, None),  // ORR
            0b0011 => (a | !b, None), // ORN
            0b0100 => (a ^ b, None),
            0b1000 => {
                let (r, c, v) = add_with_carry(a, b, false);
                (r, Some((c, v)))
            }
            0b1010 => {
                let (r, c, v) = add_with_carry(a, b, carry_in);
                (r, Some((c, v)))
            }
            0b1011 => {
                let (r, c, v) = add_with_carry(a, !b, carry_in);
                (r, Some((c, v)))
            }
            0b1101 => {
                let (r, c, v) = add_with_carry(a, !b, true);
                (r, Some((c, v)))
            }
            0b1110 => {
                let (r, c, v) = add_with_carry(!a, b, true);
                (r, Some((c, v)))
            }
            _ => return Err(Stop::Unimplemented("Thumb-2 data-processing operation")),
        };
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
        Ok(result)
    }

    /// Finish a Thumb-2 data-processing instruction: handle the forms where
    /// Rd = 15 means "only set the flags" (TST, TEQ, CMN, CMP).
    fn thumb2_write_dp_result(&mut self, op: u32, set_flags: bool, rd: usize, result: u32) -> Exec<()> {
        if rd == PC {
            if set_flags && matches!(op, 0b0000 | 0b0100 | 0b1000 | 0b1101) {
                return Ok(());
            }
            return Err(Stop::Unimplemented("Thumb-2 data processing with Rd = PC"));
        }
        self.state.regs[rd] = result;
        Ok(())
    }

    fn exec_thumb32(&mut self, mem: &mut Mem, pc: u32, hw1: u32, hw2: u32) -> Exec<Flow> {
        let rn = field(hw1, 0);
        match (hw1 >> 11) & 3 {
            0b01 => {
                if hw1 & (1 << 10) != 0 {
                    // Coprocessor instructions: same encoding as ARM
                    return self.exec_thumb32_coprocessor(mem, pc, hw1, hw2);
                }
                if hw1 & (1 << 9) != 0 {
                    // Data processing (shifted register)
                    let op = (hw1 >> 5) & 0xF;
                    let set_flags = hw1 & (1 << 4) != 0;
                    let rd = field(hw2, 8);
                    let shift_amount = ((hw2 >> 10) & 0x1C) | ((hw2 >> 6) & 3);
                    let (b, shifter_carry) = shift_imm(
                        self.state.regs[field(hw2, 0)],
                        (hw2 >> 4) & 3,
                        shift_amount,
                        self.flag(FLAG_C),
                    );
                    // ORR and ORN with Rn = 15 are MOV and MVN
                    let a = if rn == PC && matches!(op, 0b0010 | 0b0011) {
                        0
                    } else if rn == PC {
                        return Err(Stop::Unimplemented("Thumb-2 data processing with Rn = PC"));
                    } else {
                        self.state.regs[rn]
                    };
                    let result = self.thumb2_data_processing(op, set_flags, a, b, shifter_carry)?;
                    self.thumb2_write_dp_result(op, set_flags, rd, result)?;
                    return Ok(Flow::Next);
                }
                if hw1 & (1 << 6) == 0 {
                    self.exec_thumb32_load_store_multiple(mem, hw1, hw2)
                } else {
                    self.exec_thumb32_dual_exclusive_table(mem, pc, hw1, hw2)
                }
            }
            0b10 => {
                if hw2 & (1 << 15) != 0 {
                    return self.exec_thumb32_branch_etc(pc, hw1, hw2);
                }
                let rd = field(hw2, 8);
                let imm12 = ((hw1 & 0x400) << 1) | ((hw2 >> 4) & 0x700) | (hw2 & 0xFF);
                if hw1 & (1 << 9) == 0 {
                    // Data processing (modified immediate)
                    let op = (hw1 >> 5) & 0xF;
                    let set_flags = hw1 & (1 << 4) != 0;
                    let (b, shifter_carry) = expand_imm(imm12, self.flag(FLAG_C));
                    let a = if rn == PC && matches!(op, 0b0010 | 0b0011) {
                        0
                    } else if rn == PC {
                        return Err(Stop::Unimplemented("Thumb-2 data processing with Rn = PC"));
                    } else {
                        self.state.regs[rn]
                    };
                    let result = self.thumb2_data_processing(op, set_flags, a, b, shifter_carry)?;
                    self.thumb2_write_dp_result(op, set_flags, rd, result)?;
                    return Ok(Flow::Next);
                }
                // Data processing (plain binary immediate)
                if rd == PC {
                    return Err(Stop::Unimplemented("Thumb-2 immediate operation with Rd = PC"));
                }
                let imm16 = (rn as u32) << 12 | imm12;
                let lsb = ((hw2 >> 10) & 0x1C) | ((hw2 >> 6) & 3);
                match (hw1 >> 4) & 0x1F {
                    0b00000 | 0b01010 => {
                        // ADDW, SUBW; with Rn = 15 these are ADR
                        let base = if rn == PC {
                            pc.wrapping_add(4) & !3
                        } else {
                            self.state.regs[rn]
                        };
                        self.state.regs[rd] = if hw1 & (1 << 7) != 0 {
                            base.wrapping_sub(imm12)
                        } else {
                            base.wrapping_add(imm12)
                        };
                    }
                    0b00100 => self.state.regs[rd] = imm16, // MOVW
                    0b01100 => {
                        // MOVT
                        self.state.regs[rd] = (self.state.regs[rd] & 0xFFFF) | (imm16 << 16);
                    }
                    op @ (0b10100 | 0b11100) => {
                        // SBFX, UBFX
                        let width = (hw2 & 0x1F) + 1;
                        if lsb + width > 32 {
                            return Err(Stop::Unimplemented("bitfield extract out of range"));
                        }
                        let shifted = self.state.regs[rn] << (32 - lsb - width);
                        self.state.regs[rd] = if op == 0b10100 {
                            ((shifted as i32) >> (32 - width)) as u32
                        } else {
                            shifted >> (32 - width)
                        };
                    }
                    0b10110 => {
                        // BFI; with Rn = 15 this is BFC
                        let msb = hw2 & 0x1F;
                        if msb < lsb {
                            return Err(Stop::Unimplemented("BFI with msb < lsb"));
                        }
                        let mask = (u32::MAX >> (31 - (msb - lsb))) << lsb;
                        let insert = if rn == PC {
                            0
                        } else {
                            self.state.regs[rn] << lsb
                        };
                        self.state.regs[rd] = (self.state.regs[rd] & !mask) | (insert & mask);
                    }
                    _ => return Err(Stop::Unimplemented("Thumb-2 saturation or other immediate operation")),
                }
                Ok(Flow::Next)
            }
            0b11 => {
                if hw1 & (1 << 10) != 0 {
                    return Err(Stop::Unimplemented("Thumb-2 NEON or unconditional coprocessor instruction"));
                }
                if hw1 & (1 << 9) == 0 {
                    return self.exec_thumb32_load_store_single(mem, pc, hw1, hw2);
                }
                match (hw1 >> 7) & 3 {
                    0b00 | 0b01 => self.exec_thumb32_register_ops(hw1, hw2),
                    0b10 => self.exec_thumb32_multiply(hw1, hw2),
                    _ => self.exec_thumb32_long_multiply_divide(hw1, hw2),
                }
            }
            _ => unreachable!("16-bit instruction in exec_thumb32"),
        }
    }

    fn exec_thumb32_coprocessor(&mut self, mem: &mut Mem, pc: u32, hw1: u32, hw2: u32) -> Exec<Flow> {
        match (hw2 >> 8) & 0xF {
            10 | 11 => {
                let arm_form = 0xE0000000 | ((hw1 & 0x0FFF) << 16) | hw2;
                self.exec_vfp(mem, pc.wrapping_add(4), arm_form)
            }
            _ => Err(Stop::Unimplemented("coprocessor instruction")),
        }
    }

    /// STMIA, LDMIA, STMDB, LDMDB, which includes the wide PUSH and POP.
    fn exec_thumb32_load_store_multiple(&mut self, mem: &mut Mem, hw1: u32, hw2: u32) -> Exec<Flow> {
        let rn = field(hw1, 0);
        let writeback = hw1 & (1 << 5) != 0;
        let load = hw1 & (1 << 4) != 0;
        let list = hw2;
        let count = list.count_ones();
        let base = self.state.regs[rn];
        let (mut addr, new_base) = match (hw1 >> 7) & 3 {
            0b01 => (base, base.wrapping_add(4 * count)),
            0b10 => (base.wrapping_sub(4 * count), base.wrapping_sub(4 * count)),
            _ => return Err(Stop::Unimplemented("SRS/RFE")),
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
                write32(mem, addr, self.state.regs[reg])?;
            }
            addr = addr.wrapping_add(4);
        }
        if writeback && !(load && list & (1 << rn) != 0) {
            self.state.regs[rn] = new_base;
        }
        if let Some(target) = branch_target {
            self.bx_write_pc(target);
            return Ok(Flow::Branched);
        }
        Ok(Flow::Next)
    }

    /// LDRD, STRD, the exclusive loads and stores, and the table branches.
    fn exec_thumb32_dual_exclusive_table(
        &mut self,
        mem: &mut Mem,
        pc: u32,
        hw1: u32,
        hw2: u32,
    ) -> Exec<Flow> {
        let rn = field(hw1, 0);
        let pre_index = hw1 & (1 << 8) != 0;
        let add = hw1 & (1 << 7) != 0;
        let writeback = hw1 & (1 << 5) != 0;
        let load = hw1 & (1 << 4) != 0;
        let rt = field(hw2, 12);

        if !pre_index && !writeback {
            let base = self.reg_thumb(rn, pc);
            if !add {
                // LDREX, STREX. See the ARM versions about why the store
                // always succeeds.
                let addr = base.wrapping_add((hw2 & 0xFF) * 4);
                if load {
                    self.state.regs[rt] = read32(mem, addr)?;
                } else {
                    write32(mem, addr, self.state.regs[rt])?;
                    self.state.regs[field(hw2, 8)] = 0;
                }
                return Ok(Flow::Next);
            }
            match (load, (hw2 >> 4) & 0xF) {
                (true, 0b0000) | (true, 0b0001) => {
                    // TBB, TBH
                    let index = self.state.regs[field(hw2, 0)];
                    let entry = if hw2 & (1 << 4) != 0 {
                        u32::from(read16(mem, base.wrapping_add(index.wrapping_mul(2)))?)
                    } else {
                        u32::from(read8(mem, base.wrapping_add(index))?)
                    };
                    self.branch_thumb(pc.wrapping_add(4).wrapping_add(entry * 2));
                    return Ok(Flow::Branched);
                }
                (true, 0b0100) => self.state.regs[rt] = read8(mem, base)?.into(),
                (true, 0b0101) => self.state.regs[rt] = read16(mem, base)?.into(),
                (true, 0b0111) => {
                    self.state.regs[rt] = read32(mem, base)?;
                    self.state.regs[field(hw2, 8)] = read32(mem, base.wrapping_add(4))?;
                }
                (false, 0b0100) => {
                    write8(mem, base, self.state.regs[rt] as u8)?;
                    self.state.regs[field(hw2, 0)] = 0;
                }
                (false, 0b0101) => {
                    write16(mem, base, self.state.regs[rt] as u16)?;
                    self.state.regs[field(hw2, 0)] = 0;
                }
                (false, 0b0111) => {
                    write32(mem, base, self.state.regs[rt])?;
                    write32(mem, base.wrapping_add(4), self.state.regs[field(hw2, 8)])?;
                    self.state.regs[field(hw2, 0)] = 0;
                }
                _ => return Ok(Flow::Undefined),
            }
            return Ok(Flow::Next);
        }

        // LDRD, STRD (immediate or literal)
        let rt2 = field(hw2, 8);
        let offset = (hw2 & 0xFF) * 4;
        let base = if rn == PC {
            pc.wrapping_add(4) & !3
        } else {
            self.state.regs[rn]
        };
        let offset_addr = if add {
            base.wrapping_add(offset)
        } else {
            base.wrapping_sub(offset)
        };
        let addr = if pre_index { offset_addr } else { base };
        if load {
            let low = read32(mem, addr)?;
            let high = read32(mem, addr.wrapping_add(4))?;
            self.state.regs[rt] = low;
            self.state.regs[rt2] = high;
        } else {
            write32(mem, addr, self.state.regs[rt])?;
            write32(mem, addr.wrapping_add(4), self.state.regs[rt2])?;
        }
        if writeback && rn != PC && !(load && (rt == rn || rt2 == rn)) {
            self.state.regs[rn] = offset_addr;
        }
        Ok(Flow::Next)
    }

    /// Branches, and the instructions that share their encoding space: MSR,
    /// MRS, hints and barriers.
    fn exec_thumb32_branch_etc(&mut self, pc: u32, hw1: u32, hw2: u32) -> Exec<Flow> {
        let s = (hw1 >> 10) & 1;
        let (j1, j2) = ((hw2 >> 13) & 1, (hw2 >> 11) & 1);
        let wide_offset = |low_bits: u32| -> u32 {
            let (i1, i2) = ((j1 ^ s) ^ 1, (j2 ^ s) ^ 1);
            sign_extend(
                (s << 24) | (i1 << 23) | (i2 << 22) | ((hw1 & 0x3FF) << 12) | low_bits,
                25,
            )
        };
        let pc4 = pc.wrapping_add(4);
        match ((hw2 >> 14) & 1, (hw2 >> 12) & 1) {
            (0, 0) => {
                let cond = (hw1 >> 6) & 0xF;
                if cond < 0xE {
                    // B (conditional, wide)
                    if self.condition_passed(cond) {
                        let offset = sign_extend(
                            (s << 20)
                                | (j2 << 19)
                                | (j1 << 18)
                                | ((hw1 & 0x3F) << 12)
                                | ((hw2 & 0x7FF) << 1),
                            21,
                        );
                        self.branch_thumb(pc4.wrapping_add(offset));
                        return Ok(Flow::Branched);
                    }
                    return Ok(Flow::Next);
                }
                if hw1 & 0xFFE0 == 0xF380 && hw2 & 0xD0FF == 0x8000 {
                    // MSR (register) to APSR; bit 4 of hw1 selects the SPSR
                    if hw1 & (1 << 4) != 0 {
                        return Err(Stop::Unimplemented("MSR to SPSR"));
                    }
                    let value = self.state.regs[field(hw1, 0)];
                    self.write_apsr((hw2 >> 8) & 0xF, value);
                    return Ok(Flow::Next);
                }
                if hw1 == 0xF3EF && hw2 & 0xF0FF == 0x8000 {
                    // MRS from APSR
                    self.state.regs[field(hw2, 8)] = self.state.cpsr & 0xF80F0000;
                    return Ok(Flow::Next);
                }
                if hw1 == 0xF3AF && hw2 & 0xFF00 == 0x8000 {
                    // NOP and the other hints
                    return Ok(Flow::Next);
                }
                if hw1 == 0xF3BF && hw2 & 0xFF00 == 0x8F00 {
                    // CLREX, DSB, DMB, ISB
                    return Ok(Flow::Next);
                }
                Err(Stop::Unimplemented("Thumb-2 system instruction"))
            }
            (0, 1) => {
                // B (wide)
                self.branch_thumb(pc4.wrapping_add(wide_offset((hw2 & 0x7FF) << 1)));
                Ok(Flow::Branched)
            }
            (1, 1) => {
                // BL
                self.state.regs[LR] = pc4 | 1;
                self.branch_thumb(pc4.wrapping_add(wide_offset((hw2 & 0x7FF) << 1)));
                Ok(Flow::Branched)
            }
            _ => {
                // BLX: switches to ARM
                self.state.regs[LR] = pc4 | 1;
                let target = (pc4 & !3).wrapping_add(wide_offset((hw2 & 0x7FE) << 1));
                self.state.regs[PC] = target & !3;
                self.state.cpsr &= !FLAG_T;
                Ok(Flow::Branched)
            }
        }
    }

    /// Loads and stores of a single byte, halfword or word.
    fn exec_thumb32_load_store_single(
        &mut self,
        mem: &mut Mem,
        pc: u32,
        hw1: u32,
        hw2: u32,
    ) -> Exec<Flow> {
        let rn = field(hw1, 0);
        let rt = field(hw2, 12);
        let signed = hw1 & (1 << 8) != 0;
        let size = (hw1 >> 5) & 3;
        let load = hw1 & (1 << 4) != 0;
        if size == 3 || (!load && (signed || rn == PC)) {
            return Ok(Flow::Undefined);
        }

        // (address to access, value to write back to Rn if any)
        let (addr, writeback) = if rn == PC {
            // Literal
            let base = pc.wrapping_add(4) & !3;
            let imm12 = hw2 & 0xFFF;
            let addr = if hw1 & (1 << 7) != 0 {
                base.wrapping_add(imm12)
            } else {
                base.wrapping_sub(imm12)
            };
            (addr, None)
        } else if hw1 & (1 << 7) != 0 {
            // 12-bit positive offset
            (self.state.regs[rn].wrapping_add(hw2 & 0xFFF), None)
        } else if hw2 & (1 << 11) != 0 {
            // 8-bit offset with optional indexing and writeback
            let pre_index = hw2 & (1 << 10) != 0;
            let imm8 = hw2 & 0xFF;
            let base = self.state.regs[rn];
            let offset_addr = if hw2 & (1 << 9) != 0 {
                base.wrapping_add(imm8)
            } else {
                base.wrapping_sub(imm8)
            };
            (
                if pre_index { offset_addr } else { base },
                (hw2 & (1 << 8) != 0).then_some(offset_addr),
            )
        } else if hw2 & 0x0FC0 == 0 {
            // Register offset, shifted left by 0-3
            let offset = self.state.regs[field(hw2, 0)] << ((hw2 >> 4) & 3);
            (self.state.regs[rn].wrapping_add(offset), None)
        } else {
            return Ok(Flow::Undefined);
        };

        if load {
            if rt == PC && size != 2 {
                // PLD, PLI: hints
                return Ok(Flow::Next);
            }
            let value = match (size, signed) {
                (0, false) => read8(mem, addr)?.into(),
                (0, true) => read8(mem, addr)? as i8 as u32,
                (1, false) => read16(mem, addr)?.into(),
                (1, true) => read16(mem, addr)? as i16 as u32,
                _ => read32(mem, addr)?,
            };
            if let Some(new_base) = writeback {
                self.state.regs[rn] = new_base;
            }
            if rt == PC {
                self.bx_write_pc(value);
                return Ok(Flow::Branched);
            }
            self.state.regs[rt] = value;
        } else {
            let value = self.state.regs[rt];
            match size {
                0 => write8(mem, addr, value as u8)?,
                1 => write16(mem, addr, value as u16)?,
                _ => write32(mem, addr, value)?,
            }
            if let Some(new_base) = writeback {
                self.state.regs[rn] = new_base;
            }
        }
        Ok(Flow::Next)
    }

    /// Shifts by a register, sign/zero extension, byte reversal, CLZ.
    fn exec_thumb32_register_ops(&mut self, hw1: u32, hw2: u32) -> Exec<Flow> {
        let rn = field(hw1, 0);
        let rd = field(hw2, 8);
        let rm = field(hw2, 0);
        if hw2 & 0xF000 != 0xF000 || rd == PC {
            return Ok(Flow::Undefined);
        }
        if hw1 & (1 << 7) == 0 {
            if hw2 & 0x00F0 == 0 {
                // LSL, LSR, ASR, ROR (register)
                let (result, carry) = shift_reg(
                    self.state.regs[rn],
                    (hw1 >> 5) & 3,
                    self.state.regs[rm],
                    self.flag(FLAG_C),
                );
                self.state.regs[rd] = result;
                if hw1 & (1 << 4) != 0 {
                    self.set_nz(result);
                    self.set_flag(FLAG_C, carry);
                }
                return Ok(Flow::Next);
            }
            if hw2 & 0x0080 != 0 {
                // SXTH, UXTH, SXTB, UXTB, and the adding variants
                let rotated = self.state.regs[rm].rotate_right(((hw2 >> 4) & 3) * 8);
                let extended = match (hw1 >> 4) & 7 {
                    0b000 => rotated as i16 as u32,
                    0b001 => rotated & 0xFFFF,
                    0b100 => rotated as i8 as u32,
                    0b101 => rotated & 0xFF,
                    _ => return Err(Stop::Unimplemented("SXTB16/UXTB16")),
                };
                self.state.regs[rd] = if rn == PC {
                    extended
                } else {
                    self.state.regs[rn].wrapping_add(extended)
                };
                return Ok(Flow::Next);
            }
            return Ok(Flow::Undefined);
        }
        if hw2 & 0x00C0 == 0x0080 {
            let value = self.state.regs[rm];
            self.state.regs[rd] = match ((hw1 >> 4) & 3, (hw2 >> 4) & 3) {
                (0b01, 0b00) => value.swap_bytes(),
                (0b01, 0b01) => ((value >> 8) & 0x00FF00FF) | ((value << 8) & 0xFF00FF00),
                (0b01, 0b10) => value.reverse_bits(),
                (0b01, 0b11) => (value as u16).swap_bytes() as i16 as u32,
                (0b11, 0b00) => value.leading_zeros(),
                _ => return Err(Stop::Unimplemented("Thumb-2 saturating arithmetic")),
            };
            return Ok(Flow::Next);
        }
        Err(Stop::Unimplemented("Thumb-2 parallel arithmetic"))
    }

    fn exec_thumb32_multiply(&mut self, hw1: u32, hw2: u32) -> Exec<Flow> {
        let regs = &mut self.state.regs;
        let (rn, ra, rd, rm) = (field(hw1, 0), field(hw2, 12), field(hw2, 8), field(hw2, 0));
        let accumulate = ra != PC;
        let half = |value: u32, high: bool| -> i64 {
            (if high { (value >> 16) as i16 } else { value as i16 }).into()
        };
        let (n_high, m_high) = (hw2 & (1 << 5) != 0, hw2 & (1 << 4) != 0);
        let mut overflowed = false;
        match ((hw1 >> 4) & 7, (hw2 >> 4) & 0xF) {
            (0b000, 0b0000) => {
                // MUL, MLA
                let product = regs[rn].wrapping_mul(regs[rm]);
                regs[rd] = if accumulate {
                    product.wrapping_add(regs[ra])
                } else {
                    product
                };
            }
            (0b000, 0b0001) => regs[rd] = regs[ra].wrapping_sub(regs[rn].wrapping_mul(regs[rm])), // MLS
            (0b001, 0b0000..=0b0011) => {
                // SMULxy, SMLAxy
                let product = half(regs[rn], n_high) * half(regs[rm], m_high);
                if accumulate {
                    let result = product + i64::from(regs[ra] as i32);
                    overflowed = result != i64::from(result as i32);
                    regs[rd] = result as u32;
                } else {
                    regs[rd] = product as u32;
                }
            }
            (0b011, 0b0000 | 0b0001) => {
                // SMULWy, SMLAWy
                let product = (i64::from(regs[rn] as i32) * half(regs[rm], m_high)) >> 16;
                if accumulate {
                    let result = product + i64::from(regs[ra] as i32);
                    overflowed = result != i64::from(result as i32);
                    regs[rd] = result as u32;
                } else {
                    regs[rd] = product as u32;
                }
            }
            (0b101, round @ (0b0000 | 0b0001)) => {
                // SMMUL, SMMLA (optionally rounding)
                let mut result = i64::from(regs[rn] as i32) * i64::from(regs[rm] as i32);
                if accumulate {
                    result += i64::from(regs[ra] as i32) << 32;
                }
                if round == 1 {
                    result += 0x80000000;
                }
                regs[rd] = (result >> 32) as u32;
            }
            _ => return Err(Stop::Unimplemented("Thumb-2 multiply variant")),
        }
        if overflowed {
            self.state.cpsr |= FLAG_Q;
        }
        Ok(Flow::Next)
    }

    fn exec_thumb32_long_multiply_divide(&mut self, hw1: u32, hw2: u32) -> Exec<Flow> {
        let regs = &mut self.state.regs;
        let (rn, rdlo, rdhi, rm) = (field(hw1, 0), field(hw2, 12), field(hw2, 8), field(hw2, 0));
        let accumulator = (u64::from(regs[rdhi]) << 32) | u64::from(regs[rdlo]);
        let signed_product = (i64::from(regs[rn] as i32) * i64::from(regs[rm] as i32)) as u64;
        let unsigned_product = u64::from(regs[rn]) * u64::from(regs[rm]);
        let result = match ((hw1 >> 4) & 7, (hw2 >> 4) & 0xF) {
            (0b000, 0b0000) => signed_product,                               // SMULL
            (0b010, 0b0000) => unsigned_product,                             // UMULL
            (0b100, 0b0000) => signed_product.wrapping_add(accumulator),     // SMLAL
            (0b110, 0b0000) => unsigned_product.wrapping_add(accumulator),   // UMLAL
            (0b110, 0b0110) => {
                // UMAAL
                unsigned_product + u64::from(regs[rdhi]) + u64::from(regs[rdlo])
            }
            (0b001, 0b1111) => {
                // SDIV (the destination is in the RdHi position)
                let (n, m) = (regs[rn] as i32, regs[rm] as i32);
                regs[rdhi] = if m == 0 { 0 } else { n.wrapping_div(m) as u32 };
                return Ok(Flow::Next);
            }
            (0b011, 0b1111) => {
                // UDIV
                regs[rdhi] = regs[rn].checked_div(regs[rm]).unwrap_or(0);
                return Ok(Flow::Next);
            }
            _ => return Err(Stop::Unimplemented("Thumb-2 long multiply variant")),
        };
        regs[rdhi] = (result >> 32) as u32;
        regs[rdlo] = result as u32;
        Ok(Flow::Next)
    }
}
