//! FFI operation errors.
//!
//! These are host-side errors from the pointer and call layers. The native
//! layer converts them into guest exceptions; keeping them separate lets the
//! FFI be tested without a VM.

use crate::ffi::{TypeError, TypeId};
use core::fmt;

pub type FfiResult<T> = Result<T, FfiError>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FfiError {
    /// The pointer is live but its address is `NULL`.
    NullPointer,
    /// The pointer was killed (its library was closed, storage was freed, ...).
    DeadPointer,
    /// Freeing a pointer whose storage this VM does not own.
    NotOwned,
    /// The pointer's library was closed.
    LibraryClosed,
    /// The object was not produced by a load operation.
    InvalidLibrary,
    /// A dynamic loader could not open a library.
    LoadFailed(String),
    /// A symbol name is not present in the library.
    SymbolNotFound {
        name: String,
        message: String,
    },
    /// A library symbol was requested with a non-pointer type.
    NotAPointer(TypeId),
    /// The value is not callable as a C function.
    NotCallable,
    /// A type used as a call signature is not a function type.
    NotAFunction(TypeId),
    /// The call uses a type the current implementation does not pass yet.
    UnsupportedType(TypeId),
    /// Wrong number of arguments for the C signature.
    ArgCount {
        expected: usize,
        found: usize,
    },
    /// A guest value cannot be converted to the C parameter type.
    InvalidArgument(&'static str),
    /// A pointer argument points to a different C type.
    MismatchedType {
        expected: TypeId,
        found: TypeId,
    },
    /// An integer result does not fit the guest representation.
    IntegerOutOfRange,
    /// A string passed to C contains an interior NUL byte.
    EmbeddedNul,
    /// A value is not a guest C type descriptor.
    NotATypeObject,
    /// A struct, union, or array has no such field.
    UnknownField(String),
    /// An index is outside an array type's length.
    IndexOutOfBounds,
    /// Pointer arithmetic overflowed the address space.
    AddressOverflow,
    /// The value is not an array view.
    NotAnArray(TypeId),
    /// External allocation failed.
    OutOfMemory {
        size: usize,
        align: usize,
    },
    /// A C type is not a complete object type (e.g. a function type).
    NotAnObject(TypeId),
    Type(TypeError),
}

impl From<core::num::TryFromIntError> for FfiError {
    fn from(_: core::num::TryFromIntError) -> Self {
        Self::IntegerOutOfRange
    }
}

impl From<TypeError> for FfiError {
    fn from(error: TypeError) -> Self {
        Self::Type(error)
    }
}

impl fmt::Display for FfiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FfiError::NullPointer => write!(f, "null C pointer"),
            FfiError::DeadPointer => write!(f, "dead C pointer"),
            FfiError::NotOwned => write!(f, "C pointer storage is not owned by the VM"),
            FfiError::LibraryClosed => write!(f, "the owning C library was closed"),
            FfiError::InvalidLibrary => write!(f, "object is not a foreign library"),
            FfiError::LoadFailed(message) => write!(f, "failed to load library: {message}"),
            FfiError::SymbolNotFound { name, message } => {
                write!(f, "symbol '{name}' not found: {message}")
            }
            FfiError::NotAPointer(id) => write!(f, "C type {id:?} is not a pointer type"),
            FfiError::NotCallable => write!(f, "value is not a C function"),
            FfiError::NotAFunction(id) => write!(f, "C type {id:?} is not a function type"),
            FfiError::UnsupportedType(id) => write!(f, "C type {id:?} is not supported here yet"),
            FfiError::ArgCount { expected, found } => {
                write!(
                    f,
                    "wrong argument count: expected {expected}, found {found}"
                )
            }
            FfiError::InvalidArgument(reason) => write!(f, "invalid C argument: {reason}"),
            FfiError::MismatchedType { expected, found } => {
                write!(
                    f,
                    "C pointer type mismatch: expected {expected:?}, found {found:?}"
                )
            }
            FfiError::IntegerOutOfRange => {
                write!(f, "C integer result does not fit the guest range")
            }
            FfiError::EmbeddedNul => write!(f, "string contains an interior NUL byte"),
            FfiError::NotATypeObject => write!(f, "value is not a C type"),
            FfiError::UnknownField(name) => write!(f, "unknown C field '{name}'"),
            FfiError::IndexOutOfBounds => write!(f, "C array index out of bounds"),
            FfiError::AddressOverflow => write!(f, "C pointer arithmetic overflowed"),
            FfiError::NotAnArray(id) => write!(f, "C type {id:?} is not an array"),
            FfiError::OutOfMemory { size, align } => {
                write!(f, "C allocation failed ({size} bytes, align {align})")
            }
            FfiError::NotAnObject(id) => write!(f, "C type {id:?} is not an object type"),
            FfiError::Type(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for FfiError {}
