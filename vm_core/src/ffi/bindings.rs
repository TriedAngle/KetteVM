//! The guest-facing FFI surface and the call trampoline.

use crate::ffi::{Abi, CType, FfiError, TypeId};
use crate::{
    Args, CBuffer, CFunction, CallInfo, DenseString, Errors, ForeignLibrary, Handle, HandleScope,
    HandleSlice, Heap, HostCtx, Map, MapInit, MapKind, Object, PropertyDescriptor, SetupError, Smi,
    Tagged, ThreadState, VM, Value, VmCore, VmError,
};

/// A guest-visible C type descriptor: an `Object` whose first slot is the
/// `TypeId` as a Smi.
pub fn type_object<'s>(
    heap: &mut Heap,
    scope: &'s HandleScope<'_>,
    type_id: TypeId,
) -> Handle<'s, Object> {
    let map = heap.known().c_type_map.get_handle(heap, scope);
    let slots = scope.stage(&[Smi::new(type_id.as_i64()).into_tagged()]);
    scope.handle(heap.new_object(scope, map, slots))
}

fn type_id_of(heap: &Heap, value: Tagged<Value>) -> Option<TypeId> {
    let object = value.as_heap_object()?;
    if !object.map(heap).ptr_eq(heap.known().c_type_map.get(heap)) {
        return None;
    }
    let slot = object.slots(heap).at(heap, 0);
    Some(TypeId::from_i64(slot.to_i64()?))
}

fn is_nullish(heap: &Heap, value: Tagged<Value>) -> bool {
    value.ptr_eq(heap.known().null.get(heap).erase())
        || value.ptr_eq(heap.known().undefined.get(heap).erase())
}

fn raise_ffi<'a>(
    vm: &'a VM,
    heap: &'a mut Heap,
    state: &'a ThreadState,
    error: FfiError,
) -> Tagged<'a, Value> {
    Errors::raise_with(vm, heap, state, VmError::FfiError, &error.to_string())
}

fn string_of<'a>(heap: &'a Heap, value: Tagged<'a, Value>) -> Option<String> {
    value
        .get_as::<DenseString>(heap)
        .map(|string| string.to_rust_string(heap))
}

/// `FFI.library(path)` -> `ForeignLibrary`; `null` means the current process.
fn ffi_library<'a>(ctx: HostCtx<'a>, _call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let ffi = &vm.runtime::<VmCore>().ffi;
    let path = {
        let value = args.get_handle(heap, 0).as_tagged(heap);
        if is_nullish(heap, value) {
            None
        } else {
            match string_of(heap, value) {
                Some(path) => Some(path),
                None => {
                    return raise_ffi(
                        vm,
                        heap,
                        state,
                        FfiError::InvalidArgument("expected a library path string"),
                    );
                }
            }
        }
    };
    state.handle_scope(
        |scope| match ffi.open_library(vm, heap, &scope, path.as_deref()) {
            Ok(library) => library.as_tagged(heap).erase(),
            Err(error) => raise_ffi(vm, heap, state, error),
        },
    )
}

/// `FFI.cstring(text)` -> owned NUL-terminated `CPointer`.
fn ffi_cstring<'a>(ctx: HostCtx<'a>, _call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let ffi = &vm.runtime::<VmCore>().ffi;
    let text = {
        let value = args.get_handle(heap, 0).as_tagged(heap);
        match string_of(heap, value) {
            Some(text) => text,
            None => {
                return raise_ffi(
                    vm,
                    heap,
                    state,
                    FfiError::InvalidArgument("expected a string"),
                );
            }
        }
    };
    state.handle_scope(|scope| match ffi.cstring(heap, &scope, &text) {
        Ok(pointer) => pointer.as_tagged(heap).erase(),
        Err(error) => raise_ffi(vm, heap, state, error),
    })
}

/// `FFI.fn([params], result)` -> function `CType`.
fn ffi_fn<'a>(ctx: HostCtx<'a>, _call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let ffi = &vm.runtime::<VmCore>().ffi;

    let result = match type_id_of(heap, args.get_handle(heap, 1).as_tagged(heap)) {
        Some(id) => id,
        None => {
            return raise_ffi(
                vm,
                heap,
                state,
                FfiError::InvalidArgument("expected a C type as the result"),
            );
        }
    };

    let mut params = Vec::new();
    {
        let value = args.get_handle(heap, 0).as_tagged(heap);
        if !is_nullish(heap, value) {
            let Some(array) = value.as_heap_object() else {
                return raise_ffi(
                    vm,
                    heap,
                    state,
                    FfiError::InvalidArgument("expected an array of C types"),
                );
            };
            for index in 0..array.length() {
                let Some(element) = array.element_value(heap, index) else {
                    return raise_ffi(vm, heap, state, FfiError::NotATypeObject);
                };
                let Some(id) = type_id_of(heap, element) else {
                    return raise_ffi(vm, heap, state, FfiError::NotATypeObject);
                };
                params.push(id);
            }
        }
    }

    let signature = match ffi.types_mut().function(params, result, false, Abi::C) {
        Ok(id) => id,
        Err(error) => return raise_ffi(vm, heap, state, error.into()),
    };
    state.handle_scope(|scope| type_object(heap, &scope, signature).as_tagged(heap).erase())
}

/// `FFI.pointer(type)` -> pointer `CType`.
fn ffi_pointer<'a>(ctx: HostCtx<'a>, _call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let ffi = &vm.runtime::<VmCore>().ffi;
    let Some(pointee) = type_id_of(heap, args.get_handle(heap, 0).as_tagged(heap)) else {
        return raise_ffi(vm, heap, state, FfiError::NotATypeObject);
    };
    let pointer = ffi.types_mut().pointer_to(pointee);
    state.handle_scope(|scope| type_object(heap, &scope, pointer).as_tagged(heap).erase())
}

fn library_receiver<'a>(heap: &'a Heap, call: &CallInfo<'_>) -> Option<Tagged<'a, ForeignLibrary>> {
    call.receiver.as_tagged(heap).get_as::<ForeignLibrary>(heap)
}

/// `library.fn(name, signature)` -> `CFunction`.
fn library_fn<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let core = vm.runtime::<VmCore>();
    let ffi = &core.ffi;
    let trampoline = core.ffi_call;

    let signature = match type_id_of(heap, args.get_handle(heap, 1).as_tagged(heap)) {
        Some(id) => id,
        None => return raise_ffi(vm, heap, state, FfiError::NotATypeObject),
    };
    let name = {
        let value = args.get_handle(heap, 0).as_tagged(heap);
        let Some(name) = string_of(heap, value) else {
            return raise_ffi(
                vm,
                heap,
                state,
                FfiError::InvalidArgument("expected a symbol name string"),
            );
        };
        name
    };

    let pointer_type = ffi.types_mut().pointer_to(signature);
    state.handle_scope(|scope| {
        let Some(library) = library_receiver(heap, &call) else {
            return raise_ffi(vm, heap, state, FfiError::InvalidLibrary);
        };
        let library = scope.handle(library);
        let target = match ffi.library_symbol(heap, &scope, &library, &name, pointer_type) {
            Ok(target) => target,
            Err(error) => return raise_ffi(vm, heap, state, error),
        };
        match ffi.function(heap, &scope, trampoline, signature, &target, &library) {
            Ok(function) => function.as_tagged(heap).erase(),
            Err(error) => raise_ffi(vm, heap, state, error),
        }
    })
}

/// `library.symbol(name, pointer type)` -> `CPointer`.
fn library_symbol<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let ffi = &vm.runtime::<VmCore>().ffi;
    let ty = match type_id_of(heap, args.get_handle(heap, 1).as_tagged(heap)) {
        Some(id) => id,
        None => return raise_ffi(vm, heap, state, FfiError::NotATypeObject),
    };
    let name = {
        let value = args.get_handle(heap, 0).as_tagged(heap);
        let Some(name) = string_of(heap, value) else {
            return raise_ffi(
                vm,
                heap,
                state,
                FfiError::InvalidArgument("expected a symbol name string"),
            );
        };
        name
    };
    state.handle_scope(|scope| {
        let Some(library) = library_receiver(heap, &call) else {
            return raise_ffi(vm, heap, state, FfiError::InvalidLibrary);
        };
        let library = scope.handle(library);
        match ffi.library_symbol(heap, &scope, &library, &name, ty) {
            Ok(pointer) => pointer.as_tagged(heap).erase(),
            Err(error) => raise_ffi(vm, heap, state, error),
        }
    })
}

/// `library.close()`.
fn library_close<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, _args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let ffi = &vm.runtime::<VmCore>().ffi;
    state.handle_scope(|scope| {
        let Some(library) = library_receiver(heap, &call) else {
            return raise_ffi(vm, heap, state, FfiError::InvalidLibrary);
        };
        let library = scope.handle(library);
        match ffi.close_library(heap, &library) {
            Ok(()) => heap.known().undefined.get(heap).erase(),
            Err(error) => raise_ffi(vm, heap, state, error),
        }
    })
}

/// `library.path()` -> string.
fn library_path<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, _args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    state.handle_scope(|scope| {
        let Some(library) = library_receiver(heap, &call) else {
            return raise_ffi(vm, heap, state, FfiError::InvalidLibrary);
        };
        let library = scope.handle(library);
        let from = library.as_tagged(heap).path(heap).to_rust_string(heap);
        vm.interner()
            .intern_str(heap, &scope, &from)
            .as_tagged(heap)
            .erase()
    })
}

/// `FFI.struct([["name", type], ...])` -> struct `CType`.
fn ffi_struct<'a>(ctx: HostCtx<'a>, _call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let ffi = &vm.runtime::<VmCore>().ffi;
    let Some(fields) = args.get_handle(heap, 0).as_tagged(heap).as_heap_object() else {
        return raise_ffi(
            vm,
            heap,
            state,
            FfiError::InvalidArgument("expected an array of field pairs"),
        );
    };
    let mut collected = Vec::with_capacity(fields.length());
    for index in 0..fields.length() {
        let Some(pair) = fields.element_value(heap, index) else {
            return raise_ffi(vm, heap, state, FfiError::NotATypeObject);
        };
        let Some(pair) = pair.as_heap_object() else {
            return raise_ffi(
                vm,
                heap,
                state,
                FfiError::InvalidArgument("expected a [name, type] pair"),
            );
        };
        let name = pair
            .element_value(heap, 0)
            .and_then(|value| value.get_as::<DenseString>(heap))
            .map(|name| name.to_rust_string(heap));
        let Some(ty) = pair
            .element_value(heap, 1)
            .and_then(|value| type_id_of(heap, value))
        else {
            return raise_ffi(vm, heap, state, FfiError::NotATypeObject);
        };
        collected.push((name, ty));
    }
    let type_id = match ffi.types_mut().define_struct(None, collected) {
        Ok(id) => id,
        Err(error) => return raise_ffi(vm, heap, state, error.into()),
    };
    state.handle_scope(|scope| type_object(heap, &scope, type_id).as_tagged(heap).erase())
}

fn type_receiver(heap: &Heap, call: &CallInfo<'_>) -> Option<TypeId> {
    type_id_of(heap, call.receiver.as_tagged(heap))
}

/// `type.size()` / `type.align()`.
fn c_type_size<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, _args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let ffi = &vm.runtime::<VmCore>().ffi;
    let Some(type_id) = type_receiver(heap, &call) else {
        return raise_ffi(vm, heap, state, FfiError::NotATypeObject);
    };
    match ffi.types().layout(type_id) {
        Some(layout) => Smi::new(layout.size() as i64).into_tagged(),
        None => raise_ffi(vm, heap, state, FfiError::NotAnObject(type_id)),
    }
}

fn c_type_align<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, _args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let ffi = &vm.runtime::<VmCore>().ffi;
    let Some(type_id) = type_receiver(heap, &call) else {
        return raise_ffi(vm, heap, state, FfiError::NotATypeObject);
    };
    match ffi.types().layout(type_id) {
        Some(layout) => Smi::new(layout.align() as i64).into_tagged(),
        None => raise_ffi(vm, heap, state, FfiError::NotAnObject(type_id)),
    }
}

/// `type.new(...values)` -> `CBuffer` with each field initialized in order.
fn c_type_new<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let ffi = &vm.runtime::<VmCore>().ffi;
    let Some(type_id) = type_receiver(heap, &call) else {
        return raise_ffi(vm, heap, state, FfiError::NotATypeObject);
    };
    let fields: Vec<Option<String>> = match ffi.types().get(type_id) {
        CType::Struct(structure) | CType::Union(structure) => structure
            .fields
            .iter()
            .map(|field| field.name.clone())
            .collect(),
        CType::Array(_) => Vec::new(),
        _ => {
            return raise_ffi(
                vm,
                heap,
                state,
                FfiError::InvalidArgument("new() needs a struct, union, or array type"),
            );
        }
    };
    state.handle_scope(|scope| {
        let buffer = match ffi.buffer(heap, &scope, type_id) {
            Ok(buffer) => buffer,
            Err(error) => return raise_ffi(vm, heap, state, error),
        };
        for (index, name) in fields.iter().enumerate() {
            if index >= args.len() {
                break;
            }
            let Some(name) = name else { continue };
            let value = args.get_handle(heap, index).as_tagged(heap);
            if let Err(error) = ffi.buffer_set(heap, &buffer, name, value) {
                return raise_ffi(vm, heap, state, error);
            }
        }
        buffer.as_tagged(heap).erase()
    })
}

fn buffer_receiver<'a>(heap: &'a Heap, call: &CallInfo<'_>) -> Option<Tagged<'a, CBuffer>> {
    call.receiver.as_tagged(heap).get_as::<CBuffer>(heap)
}

fn field_name(heap: &Heap, value: Tagged<Value>) -> Option<String> {
    value
        .get_as::<DenseString>(heap)
        .map(|name| name.to_rust_string(heap))
}

/// `buffer.get("field")`.
fn c_buffer_get<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let ffi = &vm.runtime::<VmCore>().ffi;
    state.handle_scope(|scope| {
        let key = args.get_handle(heap, 0).as_tagged(heap);
        let Some(buffer) = buffer_receiver(heap, &call) else {
            return raise_ffi(
                vm,
                heap,
                state,
                FfiError::InvalidArgument("expected a C buffer"),
            );
        };
        let buffer = scope.handle(buffer);
        let raw = if let Some(index) = key.to_i64() {
            match ffi.buffer_get_index_raw(heap, &buffer, index as usize) {
                Ok(raw) => raw,
                Err(error) => return raise_ffi(vm, heap, state, error),
            }
        } else if let Some(name) = field_name(heap, key) {
            match ffi.buffer_get_raw(heap, &buffer, &name) {
                Ok(raw) => raw,
                Err(error) => return raise_ffi(vm, heap, state, error),
            }
        } else {
            return raise_ffi(
                vm,
                heap,
                state,
                FfiError::InvalidArgument("expected a C field name or array index"),
            );
        };
        ffi.read_raw(heap, &scope, &raw, None)
    })
}

/// `buffer.set("field", value)`.
fn c_buffer_set<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let ffi = &vm.runtime::<VmCore>().ffi;
    state.handle_scope(|scope| {
        let key = args.get_handle(heap, 0).as_tagged(heap);
        let Some(buffer) = buffer_receiver(heap, &call) else {
            return raise_ffi(
                vm,
                heap,
                state,
                FfiError::InvalidArgument("expected a C buffer"),
            );
        };
        let buffer = scope.handle(buffer);
        let value = args.get_handle(heap, 1).as_tagged(heap);
        let result = if let Some(index) = key.to_i64() {
            ffi.buffer_set_index(heap, &buffer, index as usize, value)
        } else if let Some(name) = field_name(heap, key) {
            ffi.buffer_set(heap, &buffer, &name, value)
        } else {
            return raise_ffi(
                vm,
                heap,
                state,
                FfiError::InvalidArgument("expected a C field name or array index"),
            );
        };
        match result {
            Ok(()) => heap.known().undefined.get(heap).erase(),
            Err(error) => raise_ffi(vm, heap, state, error),
        }
    })
}

/// `buffer.free()`.
fn c_buffer_free<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, _args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let ffi = &vm.runtime::<VmCore>().ffi;
    let Some(buffer) = buffer_receiver(heap, &call) else {
        return raise_ffi(
            vm,
            heap,
            state,
            FfiError::InvalidArgument("expected a C buffer"),
        );
    };
    ffi.untrack(buffer.storage(heap).address());
    match buffer.storage(heap).free_external(heap) {
        Ok(()) => heap.known().undefined.get(heap).erase(),
        Err(error) => raise_ffi(vm, heap, state, error),
    }
}

/// `FFI.array(type, count)` -> array `CType`.
fn ffi_array<'a>(ctx: HostCtx<'a>, _call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let ffi = &vm.runtime::<VmCore>().ffi;
    let Some(element) = type_id_of(heap, args.get_handle(heap, 0).as_tagged(heap)) else {
        return raise_ffi(vm, heap, state, FfiError::NotATypeObject);
    };
    let Some(count) = args.get_handle(heap, 1).as_tagged(heap).to_i64() else {
        return raise_ffi(
            vm,
            heap,
            state,
            FfiError::InvalidArgument("expected an array length"),
        );
    };
    if count < 0 {
        return raise_ffi(
            vm,
            heap,
            state,
            FfiError::InvalidArgument("negative array length"),
        );
    }
    let type_id = match ffi.types_mut().array_of(element, count as usize) {
        Ok(id) => id,
        Err(error) => return raise_ffi(vm, heap, state, error.into()),
    };
    state.handle_scope(|scope| type_object(heap, &scope, type_id).as_tagged(heap).erase())
}

/// `pointer.add(elements)` -> pointer advanced by `elements * sizeof(pointee)`.
fn c_pointer_add<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let ffi = &vm.runtime::<VmCore>().ffi;
    let Some(elements) = args.get_handle(heap, 0).as_tagged(heap).to_i64() else {
        return raise_ffi(
            vm,
            heap,
            state,
            FfiError::InvalidArgument("expected an integer offset"),
        );
    };
    state.handle_scope(|scope| {
        let Some(pointer) = call
            .receiver
            .as_tagged(heap)
            .get_as::<crate::CPointer>(heap)
        else {
            return raise_ffi(
                vm,
                heap,
                state,
                FfiError::InvalidArgument("expected a C pointer"),
            );
        };
        let pointer = scope.handle(pointer);
        match ffi.pointer_offset(heap, &scope, &pointer, elements) {
            Ok(pointer) => pointer.as_tagged(heap).erase(),
            Err(error) => raise_ffi(vm, heap, state, error),
        }
    })
}

/// `pointer.get(index)` -> value at that index, typed by the pointee.
fn c_pointer_get<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let ffi = &vm.runtime::<VmCore>().ffi;
    state.handle_scope(|scope| {
        let key = args.get_handle(heap, 0).as_tagged(heap);
        let Some(index) = key.to_i64() else {
            return raise_ffi(
                vm,
                heap,
                state,
                FfiError::InvalidArgument("expected an integer index"),
            );
        };
        let Some(pointer) = call
            .receiver
            .as_tagged(heap)
            .get_as::<crate::CPointer>(heap)
        else {
            return raise_ffi(
                vm,
                heap,
                state,
                FfiError::InvalidArgument("expected a C pointer"),
            );
        };
        let pointer = scope.handle(pointer);
        let raw = match ffi.pointer_read_raw(heap, &pointer, index as usize) {
            Ok(raw) => raw,
            Err(error) => return raise_ffi(vm, heap, state, error),
        };
        ffi.read_raw(heap, &scope, &raw, None)
    })
}

/// `pointer.set(index, value)`.
fn c_pointer_set<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let ffi = &vm.runtime::<VmCore>().ffi;
    state.handle_scope(|scope| {
        let key = args.get_handle(heap, 0).as_tagged(heap);
        let Some(index) = key.to_i64() else {
            return raise_ffi(
                vm,
                heap,
                state,
                FfiError::InvalidArgument("expected an integer index"),
            );
        };
        let Some(pointer) = call
            .receiver
            .as_tagged(heap)
            .get_as::<crate::CPointer>(heap)
        else {
            return raise_ffi(
                vm,
                heap,
                state,
                FfiError::InvalidArgument("expected a C pointer"),
            );
        };
        let pointer = scope.handle(pointer);
        let value = args.get_handle(heap, 1).as_tagged(heap);
        match ffi.pointer_write(heap, &pointer, index as usize, value) {
            Ok(()) => heap.known().undefined.get(heap).erase(),
            Err(error) => raise_ffi(vm, heap, state, error),
        }
    })
}

/// `FFI.scope_begin()` - open a manual allocation scope.
fn ffi_scope_begin<'a>(ctx: HostCtx<'a>, _call: CallInfo<'_>, _args: Args) -> Tagged<'a, Value> {
    let HostCtx { vm, heap, .. } = ctx;
    vm.runtime::<VmCore>().ffi.scope_begin();
    heap.known().undefined.get(heap).erase()
}

/// `FFI.scope_end()` - free everything allocated since `scope_begin`.
fn ffi_scope_end<'a>(ctx: HostCtx<'a>, _call: CallInfo<'_>, _args: Args) -> Tagged<'a, Value> {
    let HostCtx { vm, heap, .. } = ctx;
    vm.runtime::<VmCore>().ffi.scope_end();
    heap.known().undefined.get(heap).erase()
}

/// `buffer.pointer` -> `CPointer` to the buffer's storage.
fn c_buffer_pointer<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, _args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let ffi = &vm.runtime::<VmCore>().ffi;
    state.handle_scope(|scope| {
        let Some(buffer) = buffer_receiver(heap, &call) else {
            return raise_ffi(
                vm,
                heap,
                state,
                FfiError::InvalidArgument("expected a C buffer"),
            );
        };
        let ty = buffer.c_type();
        let storage = scope.handle(buffer.storage(heap));
        // An array decays to a pointer to its first element, like C.
        let element = match ffi.types().get(ty) {
            CType::Array(array) => Some(array.element),
            _ => None,
        };
        let pointer_type = match element {
            Some(element) => ffi.types_mut().pointer_to(element),
            None => ffi.types_mut().pointer_to(ty),
        };
        match ffi.cast_pointer(heap, &scope, &storage, pointer_type) {
            Ok(pointer) => pointer.as_tagged(heap).erase(),
            Err(error) => raise_ffi(vm, heap, state, error),
        }
    })
}

/// `pointer.cast(type)` -> retyped pointer.
fn c_pointer_cast<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let ffi = &vm.runtime::<VmCore>().ffi;
    let Some(ty) = type_id_of(heap, args.get_handle(heap, 0).as_tagged(heap)) else {
        return raise_ffi(vm, heap, state, FfiError::NotATypeObject);
    };
    state.handle_scope(|scope| {
        let Some(pointer) = call
            .receiver
            .as_tagged(heap)
            .get_as::<crate::CPointer>(heap)
        else {
            return raise_ffi(
                vm,
                heap,
                state,
                FfiError::InvalidArgument("expected a C pointer"),
            );
        };
        let pointer = scope.handle(pointer);
        match ffi.cast_pointer(heap, &scope, &pointer, ty) {
            Ok(cast) => cast.as_tagged(heap).erase(),
            Err(error) => raise_ffi(vm, heap, state, error),
        }
    })
}

/// The single native every `CFunction` maps to.
fn ffi_trampoline<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let Some(callee) = call.callee else {
        return raise_ffi(vm, heap, state, FfiError::NotCallable);
    };
    let ffi = &vm.runtime::<VmCore>().ffi;
    let raw = match ffi.call_raw(heap, &callee, args) {
        Ok(raw) => raw,
        Err(error) => return raise_ffi(vm, heap, state, error),
    };
    state.handle_scope(|scope| {
        let library = {
            let function = callee.as_tagged(heap).get_as::<CFunction>(heap);
            function
                .and_then(|function| function.library(heap))
                .map(|library| scope.handle(library))
        };
        ffi.read_raw(heap, &scope, &raw, library.as_ref())
    })
}

/// Install the `FFI` namespace and register the FFI natives.
pub fn install(vm: &mut VM, state: &mut VmCore) -> Result<(), SetupError> {
    state.ffi_call = vm.register_native(ffi_trampoline);
    let library = vm.register_native(ffi_library);
    let cstring = vm.register_native(ffi_cstring);
    let function = vm.register_native(ffi_fn);
    let pointer = vm.register_native(ffi_pointer);
    let lib_fn = vm.register_native(library_fn);
    let lib_symbol = vm.register_native(library_symbol);
    let lib_close = vm.register_native(library_close);
    let lib_path = vm.register_native(library_path);
    let struct_type = vm.register_native(ffi_struct);
    let type_new = vm.register_native(c_type_new);
    let type_size = vm.register_native(c_type_size);
    let type_align = vm.register_native(c_type_align);
    let buffer_get = vm.register_native(c_buffer_get);
    let buffer_set = vm.register_native(c_buffer_set);
    let buffer_free = vm.register_native(c_buffer_free);
    let buffer_pointer = vm.register_native(c_buffer_pointer);
    let pointer_cast = vm.register_native(c_pointer_cast);
    let scope_begin = vm.register_native(ffi_scope_begin);
    let scope_end = vm.register_native(ffi_scope_end);
    let array_type = vm.register_native(ffi_array);
    let pointer_add = vm.register_native(c_pointer_add);
    let pointer_get = vm.register_native(c_pointer_get);
    let pointer_set = vm.register_native(c_pointer_set);

    let mut thread = vm.attach();
    thread.handle_scope(|thread, scope| {
        let object_prototype = {
            let heap = thread.heap();
            heap.known()
                .object_prototype
                .get_handle(heap, &scope)
                .erase()
        };

        let prototype_map = thread.heap().allocate_handle::<Map>(
            MapInit {
                kind: MapKind::OBJECT.union(MapKind::EXTENDABLE),
                value_slot_count: 0,
                descriptors: &[],
                prototype: object_prototype.clone(),
            },
            &scope,
        );
        let library_prototype = scope.handle(thread.heap().new_object(
            &scope,
            prototype_map,
            HandleSlice::EMPTY,
        ));
        Object::install_method(thread, &scope, library_prototype, "fn", lib_fn, 2);
        Object::install_method(thread, &scope, library_prototype, "symbol", lib_symbol, 2);
        Object::install_method(thread, &scope, library_prototype, "close", lib_close, 0);
        Object::install_method(thread, &scope, library_prototype, "path", lib_path, 0);
        {
            let heap = thread.heap();
            let map = heap.known().foreign_library_map.get(heap);
            map.prototype
                .set(heap, map.erase(), library_prototype.as_tagged(heap).erase());
        }

        let type_prototype_map = thread.heap().allocate_handle::<Map>(
            MapInit {
                kind: MapKind::OBJECT.union(MapKind::EXTENDABLE),
                value_slot_count: 0,
                descriptors: &[],
                prototype: object_prototype.clone(),
            },
            &scope,
        );
        let type_prototype = scope.handle(thread.heap().new_object(
            &scope,
            type_prototype_map,
            HandleSlice::EMPTY,
        ));
        Object::install_method(thread, &scope, type_prototype, "new", type_new, 0);
        Object::install_method(thread, &scope, type_prototype, "size", type_size, 0);
        Object::install_method(thread, &scope, type_prototype, "align", type_align, 0);
        {
            let heap = thread.heap();
            let map = heap.known().c_type_map.get(heap);
            map.prototype
                .set(heap, map.erase(), type_prototype.as_tagged(heap).erase());
        }

        let buffer_prototype_map = thread.heap().allocate_handle::<Map>(
            MapInit {
                kind: MapKind::OBJECT.union(MapKind::EXTENDABLE),
                value_slot_count: 0,
                descriptors: &[],
                prototype: object_prototype.clone(),
            },
            &scope,
        );
        let buffer_prototype = scope.handle(thread.heap().new_object(
            &scope,
            buffer_prototype_map,
            HandleSlice::EMPTY,
        ));
        Object::install_method(thread, &scope, buffer_prototype, "get", buffer_get, 1);
        Object::install_method(thread, &scope, buffer_prototype, "set", buffer_set, 2);
        Object::install_method(thread, &scope, buffer_prototype, "free", buffer_free, 0);
        Object::install_method(
            thread,
            &scope,
            buffer_prototype,
            "pointer",
            buffer_pointer,
            0,
        );
        {
            let heap = thread.heap();
            let map = heap.known().c_buffer_map.get(heap);
            map.prototype
                .set(heap, map.erase(), buffer_prototype.as_tagged(heap).erase());
        }

        let pointer_prototype_map = thread.heap().allocate_handle::<Map>(
            MapInit {
                kind: MapKind::OBJECT.union(MapKind::EXTENDABLE),
                value_slot_count: 0,
                descriptors: &[],
                prototype: object_prototype.clone(),
            },
            &scope,
        );
        let pointer_prototype = scope.handle(thread.heap().new_object(
            &scope,
            pointer_prototype_map,
            HandleSlice::EMPTY,
        ));
        Object::install_method(thread, &scope, pointer_prototype, "cast", pointer_cast, 1);
        Object::install_method(thread, &scope, pointer_prototype, "add", pointer_add, 1);
        Object::install_method(thread, &scope, pointer_prototype, "get", pointer_get, 1);
        Object::install_method(thread, &scope, pointer_prototype, "set", pointer_set, 2);
        {
            let heap = thread.heap();
            let map = heap.known().c_pointer_map.get(heap);
            map.prototype
                .set(heap, map.erase(), pointer_prototype.as_tagged(heap).erase());
        }

        let namespace_map = thread.heap().allocate_handle::<Map>(
            MapInit {
                kind: MapKind::OBJECT.union(MapKind::EXTENDABLE),
                value_slot_count: 0,
                descriptors: &[],
                prototype: object_prototype.clone(),
            },
            &scope,
        );
        let namespace = scope.handle(thread.heap().new_object(
            &scope,
            namespace_map,
            HandleSlice::EMPTY,
        ));
        Object::install_method(thread, &scope, namespace, "library", library, 1);
        Object::install_method(thread, &scope, namespace, "cstring", cstring, 1);
        Object::install_method(thread, &scope, namespace, "fn", function, 2);
        Object::install_method(thread, &scope, namespace, "pointer", pointer, 1);
        Object::install_method(thread, &scope, namespace, "struct", struct_type, 1);
        Object::install_method(thread, &scope, namespace, "array", array_type, 2);
        Object::install_method(thread, &scope, namespace, "scope_begin", scope_begin, 0);
        Object::install_method(thread, &scope, namespace, "scope_end", scope_end, 0);

        for (name, type_id) in [
            ("void", TypeId::VOID),
            ("bool", TypeId::BOOL),
            ("char", TypeId::CHAR),
            ("i8", TypeId::I8),
            ("u8", TypeId::U8),
            ("i16", TypeId::I16),
            ("u16", TypeId::U16),
            ("i32", TypeId::I32),
            ("u32", TypeId::U32),
            ("i64", TypeId::I64),
            ("u64", TypeId::U64),
            ("f32", TypeId::F32),
            ("f64", TypeId::F64),
        ] {
            let object = type_object(thread.heap(), &scope, type_id);
            let value = scope.handle(object.as_tagged(thread.heap()).erase());
            let key = thread.intern(&scope, name);
            let key = scope.handle(key.as_tagged(thread.heap()).erase().as_name());
            Object::define_own_property_ordinary(
                thread.heap(),
                &scope,
                namespace,
                key,
                PropertyDescriptor::data(value),
            )
            .expect("install FFI type constant");
        }

        let global = thread
            .heap()
            .known()
            .global_object
            .get_handle(thread.heap(), &scope);
        let value = scope.handle(namespace.as_tagged(thread.heap()).erase());
        let name = thread.intern(&scope, "FFI");
        let name = scope.handle(name.as_tagged(thread.heap()).erase().as_name());
        Object::define_own_property_ordinary(
            thread.heap(),
            &scope,
            global,
            name,
            PropertyDescriptor::data(value),
        )
        .expect("install FFI global");
    });
    Ok(())
}
