/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! VFP (floating-point) instructions for the interpreter.
//!
//! The encodings are shared between ARM and Thumb mode: in Thumb mode the
//! same 32 bits appear with `0b1110` where ARM has the condition field.
//!
//! Arithmetic is done with the host's IEEE 754 operations. Things that are
//! not modelled: the cumulative exception flags in FPSCR, trapped exceptions,
//! the flush-to-zero and default-NaN modes, rounding modes other than
//! round-to-nearest, and the deprecated short vector mode (refused if FPSCR
//! selects it).

use super::{field, read32, write32, Exec, Flow, InterpreterCpu, Stop, PC};
use crate::mem::Mem;

/// Result of a VFP comparison as the NZCV bits of FPSCR.
fn compare<T: PartialOrd>(a: T, b: T) -> u32 {
    match a.partial_cmp(&b) {
        Some(std::cmp::Ordering::Equal) => 0b0110,
        Some(std::cmp::Ordering::Less) => 0b1000,
        Some(std::cmp::Ordering::Greater) => 0b0010,
        None => 0b0011, // unordered
    }
}

/// `VFPExpandImm()` for single precision.
fn expand_imm32(imm8: u32) -> u32 {
    let sign = (imm8 >> 7) & 1;
    let b6 = (imm8 >> 6) & 1;
    let exponent = ((b6 ^ 1) << 7) | (if b6 != 0 { 0b11111 << 2 } else { 0 }) | ((imm8 >> 4) & 3);
    (sign << 31) | (exponent << 23) | ((imm8 & 0xF) << 19)
}
/// `VFPExpandImm()` for double precision.
fn expand_imm64(imm8: u32) -> u64 {
    let imm8 = u64::from(imm8);
    let sign = (imm8 >> 7) & 1;
    let b6 = (imm8 >> 6) & 1;
    let exponent =
        ((b6 ^ 1) << 10) | (if b6 != 0 { 0b11111111 << 2 } else { 0 }) | ((imm8 >> 4) & 3);
    (sign << 63) | (exponent << 52) | ((imm8 & 0xF) << 48)
}

impl InterpreterCpu {
    fn s(&self, n: usize) -> u32 {
        let d = self.state.d[n / 2];
        if n & 1 == 0 {
            d as u32
        } else {
            (d >> 32) as u32
        }
    }
    fn set_s(&mut self, n: usize, value: u32) {
        let d = &mut self.state.d[n / 2];
        if n & 1 == 0 {
            *d = (*d & !0xFFFF_FFFF) | u64::from(value);
        } else {
            *d = (*d & 0xFFFF_FFFF) | (u64::from(value) << 32);
        }
    }
    fn sf(&self, n: usize) -> f32 {
        f32::from_bits(self.s(n))
    }
    fn set_sf(&mut self, n: usize, value: f32) {
        self.set_s(n, value.to_bits())
    }
    fn df(&self, n: usize) -> f64 {
        f64::from_bits(self.state.d[n])
    }
    fn set_df(&mut self, n: usize, value: f64) {
        self.state.d[n] = value.to_bits()
    }

    /// Execute a VFP instruction (coprocessors 10 and 11). `pc_base` is the
    /// value of the PC as the instruction sees it.
    pub(super) fn exec_vfp(&mut self, mem: &mut Mem, pc_base: u32, i: u32) -> Exec<Flow> {
        if (i >> 24) & 0xF == 0xE {
            if i & 0x10 == 0 {
                self.exec_vfp_data_processing(i)
            } else {
                self.exec_vfp_transfer(i)
            }
        } else if (i >> 25) & 7 == 0b110 {
            self.exec_vfp_load_store(mem, pc_base, i)
        } else {
            Err(Stop::Unimplemented("VFP instruction (unknown class)"))
        }
    }

    fn exec_vfp_data_processing(&mut self, i: u32) -> Exec<Flow> {
        if self.state.fpscr & 0x00370000 != 0 {
            return Err(Stop::Unimplemented("VFP short vector mode"));
        }
        if self.state.fpscr & 0x00C00000 != 0 {
            return Err(Stop::Unimplemented("VFP non-default rounding mode"));
        }
        let double = i & (1 << 8) != 0;
        let (d_bit, n_bit, m_bit) = (
            ((i >> 22) & 1) as usize,
            ((i >> 7) & 1) as usize,
            ((i >> 5) & 1) as usize,
        );
        let (vd, vn, vm) = (field(i, 12), field(i, 16), field(i, 0));
        // Register numbers when interpreted as single or double precision.
        let (sd, sn, sm) = ((vd << 1) | d_bit, (vn << 1) | n_bit, (vm << 1) | m_bit);
        let (dd, dn, dm) = ((d_bit << 4) | vd, (n_bit << 4) | vn, (m_bit << 4) | vm);
        let op = i & (1 << 6) != 0;

        // The three-operand arithmetic instructions.
        macro_rules! arithmetic {
            (|$d:ident, $n:ident, $m:ident| $result:expr) => {{
                if double {
                    let ($d, $n, $m) = (self.df(dd), self.df(dn), self.df(dm));
                    let _ = $d;
                    self.set_df(dd, $result);
                } else {
                    let ($d, $n, $m) = (self.sf(sd), self.sf(sn), self.sf(sm));
                    let _ = $d;
                    self.set_sf(sd, $result);
                }
            }};
        }

        match ((i >> 23) & 1, (i >> 20) & 3) {
            // VMLA, VMLS
            (0, 0) if !op => arithmetic!(|d, n, m| d + n * m),
            (0, 0) => arithmetic!(|d, n, m| d + -(n * m)),
            // VNMLS, VNMLA
            (0, 1) if !op => arithmetic!(|d, n, m| -d + n * m),
            (0, 1) => arithmetic!(|d, n, m| -d + -(n * m)),
            // VMUL, VNMUL
            (0, 2) if !op => arithmetic!(|d, n, m| n * m),
            (0, 2) => arithmetic!(|d, n, m| -(n * m)),
            // VADD, VSUB
            (0, 3) if !op => arithmetic!(|d, n, m| n + m),
            (0, 3) => arithmetic!(|d, n, m| n - m),
            // VDIV
            (1, 0) if !op => arithmetic!(|d, n, m| n / m),
            (1, 3) if !op => {
                // VMOV (immediate)
                let imm8 = ((i >> 12) & 0xF0) | (i & 0xF);
                if double {
                    self.state.d[dd] = expand_imm64(imm8);
                } else {
                    self.set_s(sd, expand_imm32(imm8));
                }
            }
            (1, 3) => {
                let bit7 = i & (1 << 7) != 0;
                match (field(i, 16), bit7) {
                    (0, false) => {
                        // VMOV (register)
                        if double {
                            self.state.d[dd] = self.state.d[dm];
                        } else {
                            self.set_s(sd, self.s(sm));
                        }
                    }
                    (0, true) => {
                        // VABS
                        if double {
                            self.state.d[dd] = self.state.d[dm] & !(1 << 63);
                        } else {
                            self.set_s(sd, self.s(sm) & !(1 << 31));
                        }
                    }
                    (1, false) => {
                        // VNEG
                        if double {
                            self.state.d[dd] = self.state.d[dm] ^ (1 << 63);
                        } else {
                            self.set_s(sd, self.s(sm) ^ (1 << 31));
                        }
                    }
                    (1, true) => {
                        // VSQRT
                        if double {
                            self.set_df(dd, self.df(dm).sqrt());
                        } else {
                            self.set_sf(sd, self.sf(sm).sqrt());
                        }
                    }
                    (opc2 @ (4 | 5), _) => {
                        // VCMP, VCMPE, with a register or with zero
                        let with_zero = opc2 == 5;
                        let nzcv = if double {
                            compare(self.df(dd), if with_zero { 0.0 } else { self.df(dm) })
                        } else {
                            compare(self.sf(sd), if with_zero { 0.0 } else { self.sf(sm) })
                        };
                        self.state.fpscr = (self.state.fpscr & 0x0FFFFFFF) | (nzcv << 28);
                    }
                    (7, true) => {
                        // VCVT between single and double precision
                        if double {
                            self.set_sf(sd, self.df(dm) as f32);
                        } else {
                            self.set_df(dd, self.sf(sm).into());
                        }
                    }
                    (8, signed) => {
                        // VCVT from integer; the integer is always in a
                        // single-precision register
                        let int = self.s(sm);
                        if double {
                            let value = if signed {
                                f64::from(int as i32)
                            } else {
                                f64::from(int)
                            };
                            self.set_df(dd, value);
                        } else {
                            let value = if signed { int as i32 as f32 } else { int as f32 };
                            self.set_sf(sd, value);
                        }
                    }
                    (opc2 @ (12 | 13), round_to_zero) => {
                        // VCVT, VCVTR to integer; the result always goes to
                        // a single-precision register. Rust's float-to-int
                        // casts saturate and turn NaN into 0, like ARM's.
                        let value = if double {
                            self.df(dm)
                        } else {
                            self.sf(sm).into()
                        };
                        let value = if round_to_zero {
                            value
                        } else {
                            value.round_ties_even()
                        };
                        let int = if opc2 == 13 {
                            value as i32 as u32
                        } else {
                            value as u32
                        };
                        self.set_s(sd, int);
                    }
                    _ => return Err(Stop::Unimplemented("VFP conversion or other operation")),
                }
            }
            _ => return Err(Stop::Unimplemented("VFP data-processing instruction")),
        }
        Ok(Flow::Next)
    }

    /// Transfers between one ARM core register and VFP.
    fn exec_vfp_transfer(&mut self, i: u32) -> Exec<Flow> {
        let rt = field(i, 12);
        let to_core = i & (1 << 20) != 0;
        if rt == PC && i & 0x0FFF0FFF != 0x0EF10A10 {
            return Err(Stop::Unimplemented("VFP transfer with PC"));
        }
        if i & 0x0FE00F7F == 0x0E000A10 {
            // VMOV between a core register and a single-precision register
            let sn = (field(i, 16) << 1) | ((i >> 7) & 1) as usize;
            if to_core {
                self.state.regs[rt] = self.s(sn);
            } else {
                self.set_s(sn, self.state.regs[rt]);
            }
        } else if i & 0x0FFF0FFF == 0x0EF10A10 {
            // VMRS from FPSCR. With Rt = 15, only the flags are transferred,
            // to the APSR.
            if rt == PC {
                self.state.cpsr = (self.state.cpsr & 0x0FFFFFFF) | (self.state.fpscr & 0xF0000000);
            } else {
                self.state.regs[rt] = self.state.fpscr;
            }
        } else if i & 0x0FFF0FFF == 0x0EE10A10 {
            // VMSR to FPSCR
            self.state.fpscr = self.state.regs[rt];
        } else if i & 0x0FD00F7F == 0x0E000B10 || i & 0x0FD00F7F == 0x0E100B10 {
            // VMOV between a core register and one half of a double register
            let dn = (((i >> 7) & 1) as usize) << 4 | field(i, 16);
            let high = i & (1 << 21) != 0;
            let d = self.state.d[dn];
            if to_core {
                self.state.regs[rt] = if high { (d >> 32) as u32 } else { d as u32 };
            } else {
                let value = u64::from(self.state.regs[rt]);
                self.state.d[dn] = if high {
                    (d & 0xFFFF_FFFF) | (value << 32)
                } else {
                    (d & !0xFFFF_FFFF) | value
                };
            }
        } else {
            return Err(Stop::Unimplemented("VFP/NEON register transfer"));
        }
        Ok(Flow::Next)
    }

    /// Loads and stores of VFP registers, and transfers involving two core
    /// registers.
    fn exec_vfp_load_store(&mut self, mem: &mut Mem, pc_base: u32, i: u32) -> Exec<Flow> {
        let double = i & (1 << 8) != 0;
        let to_core_or_load = i & (1 << 20) != 0;

        if i & 0x0FE00ED0 == 0x0C400A10 {
            // VMOV between two core registers and a double-precision register
            // or two consecutive single-precision registers
            let (rt, rt2) = (field(i, 12), field(i, 16));
            let m_bit = ((i >> 5) & 1) as usize;
            if rt == PC || rt2 == PC {
                return Err(Stop::Unimplemented("VFP transfer with PC"));
            }
            if double {
                let dm = (m_bit << 4) | field(i, 0);
                if to_core_or_load {
                    self.state.regs[rt] = self.state.d[dm] as u32;
                    self.state.regs[rt2] = (self.state.d[dm] >> 32) as u32;
                } else {
                    self.state.d[dm] =
                        u64::from(self.state.regs[rt]) | (u64::from(self.state.regs[rt2]) << 32);
                }
            } else {
                let sm = (field(i, 0) << 1) | m_bit;
                if sm == 31 {
                    return Err(Stop::Unimplemented("VMOV with s31, s32"));
                }
                if to_core_or_load {
                    self.state.regs[rt] = self.s(sm);
                    self.state.regs[rt2] = self.s(sm + 1);
                } else {
                    self.set_s(sm, self.state.regs[rt]);
                    self.set_s(sm + 1, self.state.regs[rt2]);
                }
            }
            return Ok(Flow::Next);
        }

        let pre_index = i & (1 << 24) != 0;
        let add = i & (1 << 23) != 0;
        let writeback = i & (1 << 21) != 0;
        let rn = field(i, 16);
        let imm8 = i & 0xFF;
        let d_bit = ((i >> 22) & 1) as usize;
        let vd = field(i, 12);
        let first = if double {
            (d_bit << 4) | vd
        } else {
            (vd << 1) | d_bit
        };

        if pre_index && !writeback {
            // VLDR, VSTR
            let base = if rn == PC {
                pc_base & !3
            } else {
                self.state.regs[rn]
            };
            let addr = if add {
                base.wrapping_add(imm8 * 4)
            } else {
                base.wrapping_sub(imm8 * 4)
            };
            self.vfp_transfer_registers(mem, addr, first, 1, double, to_core_or_load)?;
            return Ok(Flow::Next);
        }

        if (!pre_index && add) || (pre_index && !add && writeback) {
            // VLDM, VSTM (increment after, or decrement before with
            // writeback), which includes VPUSH and VPOP
            if rn == PC {
                return Err(Stop::Unimplemented("VLDM/VSTM with PC"));
            }
            let count = if double { imm8 / 2 } else { imm8 } as usize;
            if count == 0 || first + count > 32 {
                return Err(Stop::Unimplemented("VLDM/VSTM with a bad register list"));
            }
            let base = self.state.regs[rn];
            let addr = if add {
                base
            } else {
                base.wrapping_sub(imm8 * 4)
            };
            self.vfp_transfer_registers(mem, addr, first, count, double, to_core_or_load)?;
            if writeback {
                self.state.regs[rn] = if add {
                    base.wrapping_add(imm8 * 4)
                } else {
                    base.wrapping_sub(imm8 * 4)
                };
            }
            return Ok(Flow::Next);
        }

        Err(Stop::Unimplemented("VFP load/store instruction"))
    }

    /// Load or store `count` consecutive VFP registers at ascending addresses.
    fn vfp_transfer_registers(
        &mut self,
        mem: &mut Mem,
        mut addr: u32,
        first: usize,
        count: usize,
        double: bool,
        load: bool,
    ) -> Exec<()> {
        for reg in first..first + count {
            if double {
                if load {
                    let low = read32(mem, addr)?;
                    let high = read32(mem, addr.wrapping_add(4))?;
                    self.state.d[reg] = u64::from(low) | (u64::from(high) << 32);
                } else {
                    write32(mem, addr, self.state.d[reg] as u32)?;
                    write32(mem, addr.wrapping_add(4), (self.state.d[reg] >> 32) as u32)?;
                }
                addr = addr.wrapping_add(8);
            } else {
                if load {
                    let value = read32(mem, addr)?;
                    self.set_s(reg, value);
                } else {
                    write32(mem, addr, self.s(reg))?;
                }
                addr = addr.wrapping_add(4);
            }
        }
        Ok(())
    }
}
