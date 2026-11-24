#![cfg_attr(
    feature = "fast",
    feature(explicit_tail_calls, rust_preserve_none_cc, fn_align)
)]
#![allow(incomplete_features)]
#![allow(unused_imports, unused_macros, unused_unsafe, unused_variables)]

use bytecode::{Opcode, TestTypeKind};
use core::hint::black_box;
use std::marker::PhantomData;
use vm_core::{
    Acc, CallTarget, CallableInfoObject, Callee, Compare, ContextInit, ContextObject, Convert, Ctx,
    Errors, FeedbackVector, FixedArray, HandleSlice, Heap, NativeIndex, Object, Ops, Regs,
    ScopeInfo, Smi, StringRef, Tagged, Value, VmError, bootstrap::WellKnown, ic::ElementHit,
    ic::GlobalLexicalHit, ic::InlineCache, ic::LoadHit, ic::MonoProbe, raise, raise_runtime,
};

pub mod handler;
#[cfg(all(not(feature = "fast"), debug_assertions))]
pub mod loop_call;
#[cfg(not(feature = "fast"))]
pub mod loop_match;
#[cfg(feature = "fast")]
pub mod register_probe;
#[cfg(feature = "fast")]
pub mod tail_call;

#[cfg(all(not(feature = "fast"), debug_assertions))]
pub use loop_call::LoopCallInterpreter;
#[cfg(all(not(feature = "fast"), not(debug_assertions)))]
pub use loop_match::LoopMatchInterpreter;
#[cfg(feature = "fast")]
pub use tail_call::{DispatchTable, Handler, TailCallInterpreter};
#[cfg(feature = "fast")]
use tail_call::{TableArg, throw_dispatch};
pub use vm_core::{EntryFn, Interpreter};

#[cfg(feature = "fast")]
pub type DefaultInterpreter = tail_call::TailCallInterpreter;
#[cfg(all(not(feature = "fast"), debug_assertions))]
pub type DefaultInterpreter = loop_call::LoopCallInterpreter;
#[cfg(all(not(feature = "fast"), not(debug_assertions)))]
pub type DefaultInterpreter = loop_match::LoopMatchInterpreter;
mod slow;
mod utils;

/// A pinned FP-register argument. Rides the float register pool (d0
/// under preserve-none), so it costs no integer argument registers;
/// forwarding it through `become` is free. Numeric slow paths hand
/// their payload through it instead of parking it in a Ctx cell.
#[derive(Clone, Copy)]
pub struct FloatReg(f64);

impl FloatReg {
    #[inline(always)]
    pub fn new(v: f64) -> FloatReg {
        FloatReg(v)
    }

    #[inline(always)]
    pub fn get(self) -> f64 {
        black_box(self.0)
    }
}

#[derive(Clone, Copy)]
pub struct RootsArg<'s>(&'s WellKnown);

impl<'s> RootsArg<'s> {
    #[inline(always)]
    pub fn new(known: &'s WellKnown) -> RootsArg<'s> {
        RootsArg(known)
    }

    #[inline(always)]
    pub fn known(&self, _heap: &Heap) -> &'s WellKnown {
        self.0
    }
}

impl core::ops::Deref for RootsArg<'_> {
    type Target = WellKnown;

    #[inline(always)]
    fn deref(&self) -> &WellKnown {
        self.0
    }
}

#[cfg(all(feature = "star_fusion", feature = "fast"))]
const fn star_lookahead(op: Opcode) -> bool {
    matches!(
        op,
        Opcode::Load
            | Opcode::LoadSmi
            | Opcode::LoadConstant
            | Opcode::LoadZero
            | Opcode::LoadUndefined
            | Opcode::LoadNull
            | Opcode::LoadTrue
            | Opcode::LoadFalse
            | Opcode::LoadHole
            | Opcode::LoadGlobal
            | Opcode::LoadNamedProperty
            | Opcode::LoadKeyedProperty
            | Opcode::LoadElementImm
            | Opcode::LoadNewTarget
            | Opcode::LoadContextSlot
            | Opcode::Add
            | Opcode::Sub
            | Opcode::Mul
            | Opcode::Div
            | Opcode::Mod
            | Opcode::Exp
            | Opcode::BitwiseOr
            | Opcode::BitwiseXor
            | Opcode::BitwiseAnd
            | Opcode::ShiftLeft
            | Opcode::ShiftRight
            | Opcode::ShiftRightLogical
            | Opcode::AddImmediate
            | Opcode::SubImmediate
            | Opcode::MulImmediate
            | Opcode::DivImmediate
            | Opcode::ModImmediate
            | Opcode::ExpImmediate
            | Opcode::BitwiseOrImmediate
            | Opcode::BitwiseXorImmediate
            | Opcode::BitwiseAndImmediate
            | Opcode::ShiftLeftImmediate
            | Opcode::ShiftRightImmediate
            | Opcode::ShiftRightLogicalImmediate
            | Opcode::AddLoc
            | Opcode::SubLoc
    )
}

/// Placeholder receiver type for `#[handler]` fns.
/// The proc macro rewrites access to it.
#[doc(hidden)]
pub struct Frame;
