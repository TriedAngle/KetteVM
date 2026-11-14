//! libffi lowering of a C function type.

use crate::ffi::{FfiError, FfiResult, TypeKind};
use crate::{Handle, Heap, Tagged, TypeInfo};
use libffi::middle::{Cif, Type};

/// A prepared libffi signature.
///
/// Built once per interned function type and leaked until the VM shuts down;
/// the owning `TypeInfo` stores the pointer.
pub struct CallPlan {
    pub cif: Cif,
}

// Safety: an `ffi_cif` is built once and is read-only during `ffi_call`;
// libffi does not mutate a prepared cif while calls are in flight.
unsafe impl Send for CallPlan {}
unsafe impl Sync for CallPlan {}

impl CallPlan {
    pub fn new(
        heap: &Heap,
        params: &[Handle<TypeInfo>],
        result: &Handle<TypeInfo>,
        variadic: bool,
    ) -> FfiResult<Self> {
        let mut lowered = Vec::with_capacity(params.len());
        for param in params {
            lowered.push(lower(heap, param.as_tagged(heap))?);
        }
        let result_ty = result.as_tagged(heap);
        let lowered_result = if result_ty.kind() == TypeKind::Void {
            Type::void()
        } else {
            lower(heap, result_ty)?
        };
        let fixed = params.len();
        let cif = if variadic {
            Cif::new_variadic(lowered, fixed, lowered_result)
        } else {
            Cif::new(lowered, lowered_result)
        };
        Ok(Self { cif })
    }
}

fn lower(heap: &Heap, ty: Tagged<TypeInfo>) -> FfiResult<Type> {
    Ok(match ty.kind() {
        TypeKind::Bool | TypeKind::U8 | TypeKind::Char => Type::u8(),
        TypeKind::I8 => Type::i8(),
        TypeKind::U16 => Type::u16(),
        TypeKind::I16 => Type::i16(),
        TypeKind::U32 => Type::u32(),
        TypeKind::I32 => Type::i32(),
        TypeKind::U64 => Type::u64(),
        TypeKind::I64 => Type::i64(),
        TypeKind::F32 => Type::f32(),
        TypeKind::F64 => Type::f64(),
        TypeKind::Pointer => Type::pointer(),
        TypeKind::Struct => {
            let fields = ty.fields(heap).ok_or(FfiError::UnsupportedType(ty.id()))?;
            let mut elements = Vec::with_capacity(fields.len() / 3);
            let mut index = 0;
            while index + 2 < fields.len() {
                let field = fields
                    .at(heap, index + 1)
                    .get_as::<TypeInfo>(heap)
                    .ok_or(FfiError::UnsupportedType(ty.id()))?;
                elements.push(lower(heap, field)?);
                index += 3;
            }
            Type::structure(elements)
        }
        // libffi has no array node: a fixed array is a struct of elements.
        TypeKind::Array if ty.len() > 0 => {
            let element = ty.element(heap).ok_or(FfiError::UnsupportedType(ty.id()))?;
            let lowered = lower(heap, element)?;
            let elements = (0..ty.len()).map(|_| lowered.clone()).collect::<Vec<_>>();
            Type::structure(elements)
        }
        // libffi has no union node, and a zero-length array has no layout.
        TypeKind::Void | TypeKind::Union | TypeKind::Array | TypeKind::Function => {
            return Err(FfiError::UnsupportedType(ty.id()));
        }
    })
}
