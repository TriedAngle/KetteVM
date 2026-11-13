//! Per-signature call plans: the libffi lowering of a C function type.

use crate::ffi::{CType, FfiError, FfiResult, TypeId, TypeTable};
use libffi::middle::{Cif, Type};

/// A prepared libffi call signature, cached by function `TypeId`.
pub struct CallPlan {
    pub signature: TypeId,
    pub cif: Cif,
    pub params: Vec<TypeId>,
    pub result: TypeId,
}

// Safety: an `ffi_cif` is built once and is read-only during `ffi_call`;
// libffi does not mutate a prepared cif while calls are in flight.
unsafe impl Send for CallPlan {}
unsafe impl Sync for CallPlan {}

impl CallPlan {
    pub fn new(types: &TypeTable, signature: TypeId) -> FfiResult<Self> {
        let (params, result, variadic) = match types.get(signature) {
            CType::Function(function) => {
                (function.params.clone(), function.result, function.variadic)
            }
            _ => return Err(FfiError::NotAFunction(signature)),
        };

        let mut lowered = Vec::with_capacity(params.len());
        for &param in &params {
            lowered.push(lower(types, param)?);
        }
        let lowered_result = lower_result(types, result)?;
        let fixed = params.len();
        let cif = if variadic {
            Cif::new_variadic(lowered, fixed, lowered_result)
        } else {
            Cif::new(lowered, lowered_result)
        };

        Ok(Self {
            signature,
            cif,
            params,
            result,
        })
    }
}

fn lower(types: &TypeTable, id: TypeId) -> FfiResult<Type> {
    Ok(match types.get(id) {
        CType::Bool | CType::U8 | CType::Char => Type::u8(),
        CType::I8 => Type::i8(),
        CType::U16 => Type::u16(),
        CType::I16 => Type::i16(),
        CType::U32 => Type::u32(),
        CType::I32 => Type::i32(),
        CType::U64 => Type::u64(),
        CType::I64 => Type::i64(),
        CType::F32 => Type::f32(),
        CType::F64 => Type::f64(),
        CType::Pointer(_) => Type::pointer(),
        CType::Struct(structure) => {
            let mut elements = Vec::with_capacity(structure.fields.len());
            for field in &structure.fields {
                elements.push(lower(types, field.ty)?);
            }
            Type::structure(elements)
        }
        // libffi has no array node: a fixed array is a struct of elements.
        CType::Array(array) if array.len > 0 => {
            let element = lower(types, array.element)?;
            let elements = (0..array.len).map(|_| element.clone()).collect::<Vec<_>>();
            Type::structure(elements)
        }
        // libffi has no union node, and a zero-length array has no layout.
        CType::Void | CType::Union(_) | CType::Array(_) | CType::Function(_) => {
            return Err(FfiError::UnsupportedType(id));
        }
    })
}

fn lower_result(types: &TypeTable, id: TypeId) -> FfiResult<Type> {
    if id == TypeId::VOID {
        return Ok(Type::void());
    }
    lower(types, id)
}
