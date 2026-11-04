mod builder;
mod opcodes;
mod program;
mod validate;

pub use builder::{
    BuildError, ConstIdx, Feedback, FnBuilder, FunctionMeta, Label, REGISTER_FILE_START, Reg,
    RegList, RtArg, TempScope, TryBlock,
};
pub use opcodes::{OPERAND_SIZES_NARROW, OPERAND_SIZES_WIDE, Opcode};
pub use program::{
    CallableKind, Constant, FrontendError, FrontendErrorKind, Function, FunctionId,
    GlobalDeclarations, GlobalFunctionDecl, HandlerEntry, Program, ScopeLayout, ScopeLocal,
    SourceMode,
};
pub use validate::{ValidationError, validate, validate_function};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum PropertyFlags {
    /// the data value is not writable
    ReadOnly = 1 << 0,
    /// the property is not enumerable
    DontEnum = 1 << 1,
    /// the property is not configurable
    DontDelete = 1 << 2,
    /// acc holds an `AccessorPair` (get/set) instead of a data value
    Accessor = 1 << 3,
}

impl PropertyFlags {
    /// The operand encoding of this attribute.
    pub const fn bits(self) -> u32 {
        self as u32
    }
}

impl core::ops::BitOr for PropertyFlags {
    type Output = u32;
    fn bitor(self, rhs: Self) -> u32 {
        self.bits() | rhs.bits()
    }
}

impl core::ops::BitOr<PropertyFlags> for u32 {
    type Output = u32;
    fn bitor(self, rhs: PropertyFlags) -> Self::Output {
        self | rhs.bits()
    }
}

/// `Store*PropertyToSuper` semantics-flag operand: ES stores shadow
/// inherited data properties on `this`; the alternative write-through
/// variant writes at the holder (Self-style semantics).
pub const SUPER_STORE_WRITE_THROUGH: u32 = 1;

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum Operand {
    Register,          // scalable, signed (negative indices are parameters)
    RegisterListStart, // first register of a start+count range; scalable, signed
    RegisterCount,     // UImmediate (scales with the stream like the rest)
    Immediate,         // scalable, signed
    UImmediate,        // scalable
    Index,             // scalable
}

impl Operand {
    pub const fn size_in_stream(self, scale: Scale) -> usize {
        // Every operand widens with the stream. Keeping `RegisterCount`
        // scalable lets the interpreter use a uniform `base + I * STRIDE`
        // operand stride (and `STRIDE * operands().len()` instruction
        // sizing) in both encodings, at the cost of one extra byte per
        // call in the wide form.
        scale as usize
    }

    pub const fn is_signed(self) -> bool {
        use Operand::*;
        matches!(self, Register | RegisterListStart | Immediate)
    }
}

/// A lazy cursor over one instruction's operands: values are read
/// straight from the code stream on access — positional, idempotent,
/// never materialized. Per-operand offset, size and signedness come
/// from packed per-opcode tables (see the `OPERAND_*` statics).
///
/// The `code` view is anchored to a heap borrow and therefore valid
/// only until the next safepoint (any allocation): fast paths read
/// their operands immediately after [`decode`]; across a safepoint,
/// re-derive the view from the rooted code register (the position
/// words — `pc`, `base` — are plain integers and always safe).
#[derive(Clone, Copy)]
pub struct Operands<'c> {
    code: &'c [u8],
    /// Byte offset of the first operand.
    base: u32,
    row: &'static [u8; 5],
}

impl<'c> Operands<'c> {
    /// Read operand `i` from the stream.
    #[inline(always)]
    fn read(&self, i: usize) -> u32 {
        let e = self.row[i] as usize;
        debug_assert!(e != 0, "no operand {i}");
        let off = self.base as usize + (e & 0x0f) - 1;
        debug_assert!(
            off + if e & 0x10 != 0 { 2 } else { 1 } <= self.code.len(),
            "operand read past the end of the code stream"
        );
        // Safety: the stream was validated at materialization, and the
        // row encodes this operand's exact offset and size.
        unsafe {
            if e & 0x10 != 0 {
                let w = u16::from_le_bytes([
                    *self.code.get_unchecked(off),
                    *self.code.get_unchecked(off + 1),
                ]) as u32;
                if e & 0x20 != 0 {
                    (w as i16) as i32 as u32
                } else {
                    w
                }
            } else {
                let b = *self.code.get_unchecked(off) as u32;
                if e & 0x20 != 0 {
                    (b as i8) as i32 as u32
                } else {
                    b
                }
            }
        }
    }

    #[inline]
    pub fn reg(&self, i: usize) -> i32 {
        self.read(i) as i32
    }

    #[inline]
    pub fn reg_list(&self, i: usize) -> i32 {
        self.read(i) as i32
    }

    #[inline]
    pub fn reg_count(&self, i: usize) -> usize {
        self.read(i) as usize
    }

    #[inline]
    pub fn imm(&self, i: usize) -> i32 {
        self.read(i) as i32
    }

    #[inline]
    pub fn uimm(&self, i: usize) -> u32 {
        self.read(i)
    }

    #[inline]
    pub fn idx(&self, i: usize) -> usize {
        self.read(i) as usize
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Scale {
    Byte1 = 1, // default
    Byte2 = 2, // wide
}

pub fn emit(code: &mut Vec<u8>, op: Opcode, operands: &[u32]) {
    let kinds = op.operands();
    assert_eq!(kinds.len(), operands.len(), "operand count mismatch");

    let wide = kinds.iter().zip(operands).any(|(kind, value)| {
        if kind.is_signed() {
            let v = *value as i32;
            v < i8::MIN as i32 || v > i8::MAX as i32
        } else {
            *value > u8::MAX as u32
        }
    });
    let scale = if wide { Scale::Byte2 } else { Scale::Byte1 };
    if wide {
        code.push(Opcode::Wide as u8);
    }
    code.push(op as u8);

    for (kind, value) in kinds.iter().zip(operands) {
        let size = kind.size_in_stream(scale);
        let fits = if kind.is_signed() {
            let v = *value as i32;
            match size {
                1 => v >= i8::MIN as i32 && v <= i8::MAX as i32,
                2 => v >= i16::MIN as i32 && v <= i16::MAX as i32,
                _ => unreachable!("signed operands are at most 2 bytes"),
            }
        } else {
            (*value as u64) < (1u64 << (size * 8))
        };
        assert!(fits, "operand {value} does not fit in {size} byte(s)");
        code.extend_from_slice(&value.to_le_bytes()[..size]);
    }
}

/// Decode the instruction at `pc` into a lazy operand cursor. The stream
/// is trusted — programs are validated at materialization (`validate`) —
/// so release builds read unchecked; debug builds re-verify via
/// [`try_decode`]. The returned view is anchored to `code`'s borrow and
/// valid only until the next safepoint (see [`Operands`]).
pub fn decode<'c>(code: &'c [u8], pc: usize) -> (Opcode, Operands<'c>, usize) {
    debug_assert!(
        try_decode(code, pc).is_some(),
        "invalid or truncated instruction at pc {pc}"
    );
    // Safety: validated stream (see above).
    let byte = unsafe { *code.get_unchecked(pc) };
    let (op, base, wide) = if byte == Opcode::Wide as u8 {
        (
            // Safety: validated stream.
            unsafe { Opcode::from_byte_unchecked(*code.get_unchecked(pc + 1)) },
            pc + 2,
            true,
        )
    } else {
        // Safety: validated stream.
        (unsafe { Opcode::from_byte_unchecked(byte) }, pc + 1, false)
    };
    let idx = op as usize;
    // Safety: opcode discriminants are dense over the decode tables
    let row: &'static [u8; 5] = unsafe {
        (if wide {
            opcodes::OPERAND_ROWS_WIDE
        } else {
            opcodes::OPERAND_ROWS_NARROW
        })
        .get_unchecked(idx)
    };
    // Safety: dense discriminant, see above
    let size = unsafe {
        *(if wide {
            opcodes::OPERAND_SIZES_WIDE
        } else {
            opcodes::OPERAND_SIZES_NARROW
        })
        .get_unchecked(idx)
    } as usize;
    (
        op,
        Operands {
            code,
            base: base as u32,
            row,
        },
        base + size,
    )
}

/// Fallible [`decode`]: `None` on an unknown opcode byte or a stream that
/// ends inside an instruction. The validator's checked twin.
pub fn try_decode<'c>(code: &'c [u8], mut pc: usize) -> Option<(Opcode, Operands<'c>, usize)> {
    let mut byte = *code.get(pc)?;
    pc += 1;
    let wide = byte == Opcode::Wide as u8;
    if wide {
        byte = *code.get(pc)?;
        pc += 1;
    }
    let op = Opcode::from_byte(byte)?;
    let idx = op as usize;
    let row = &(if wide {
        opcodes::OPERAND_ROWS_WIDE
    } else {
        opcodes::OPERAND_ROWS_NARROW
    })[idx];
    let size = (if wide {
        opcodes::OPERAND_SIZES_WIDE
    } else {
        opcodes::OPERAND_SIZES_NARROW
    })[idx] as usize;
    if pc + size > code.len() {
        return None;
    }
    Some((
        op,
        Operands {
            code,
            base: pc as u32,
            row,
        },
        pc + size,
    ))
}

pub fn jump_target(pc: usize, offset: i32) -> usize {
    pc.wrapping_add_signed(offset as isize)
}
