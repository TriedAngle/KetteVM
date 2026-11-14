//! The guest-facing FFI surface and the call trampoline.

use crate::ffi::{FfiError, TypeKind, ValueRead};
use crate::{
    AccessorInfo, Args, CFunction, CPointer, CPointerFlags, CPointerInit, CValue, CallInfo,
    DenseString, Errors, FixedArray, ForeignLibrary, Handle, HandleSlice, Heap, HostCtx, Map,
    MapInit, MapKind, Object, PropertyDescriptor, SetupError, Smi, Tagged, ThreadState, TypeInfo,
    VM, Value, VmCore, VmError,
};

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
    let buffer_get = vm.register_native(c_value_get);
    let buffer_set = vm.register_native(c_value_set);
    let buffer_pointer = vm.register_native(c_value_pointer);
    let pointer_cast = vm.register_native(c_pointer_cast);
    let array_type = vm.register_native(ffi_array);
    let field_get = vm.register_native(c_value_field_get);
    let field_set = vm.register_native(c_value_field_set);
    let value_copy = vm.register_native(c_value_copy);
    let type_wrap = vm.register_native(c_type_wrap);
    let pointer_add = vm.register_native(c_pointer_add);
    let pointer_get = vm.register_native(c_pointer_get);
    let pointer_set = vm.register_native(c_pointer_set);

    let ffi = &state.ffi;
    let mut thread = vm.attach();
    thread.handle_scope(|thread, scope| {
        ffi.install(thread.heap(), &scope, field_get, field_set);

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
        Object::install_method(thread, &scope, type_prototype, "wrap", type_wrap, 1);
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
        Object::install_method(thread, &scope, buffer_prototype, "copy", value_copy, 0);
        Object::install_method(thread, &scope, buffer_prototype, "get", buffer_get, 1);
        Object::install_method(thread, &scope, buffer_prototype, "set", buffer_set, 2);
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
            let map = heap.known().c_value_map.get(heap);
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

        let heap: &mut Heap = thread.heap();
        let map = heap.known().c_pointer_map.get(heap);
        map.prototype
            .set(heap, map.erase(), pointer_prototype.as_tagged(heap).erase());

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

        for (name, kind) in [
            ("void", TypeKind::Void),
            ("bool", TypeKind::Bool),
            ("char", TypeKind::Char),
            ("i8", TypeKind::I8),
            ("u8", TypeKind::U8),
            ("i16", TypeKind::I16),
            ("u16", TypeKind::U16),
            ("i32", TypeKind::I32),
            ("u32", TypeKind::U32),
            ("i64", TypeKind::I64),
            ("u64", TypeKind::U64),
            ("f32", TypeKind::F32),
            ("f64", TypeKind::F64),
        ] {
            let value = {
                let ty = ffi.builtin(thread.heap(), kind);
                scope.handle(ty).erase()
            };
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

fn type_of<'a>(heap: &'a Heap, value: Tagged<'a, Value>) -> Option<Tagged<'a, TypeInfo>> {
    value.get_as::<TypeInfo>(heap)
}

fn type_receiver<'a>(heap: &'a Heap, call: &CallInfo<'_>) -> Option<Tagged<'a, TypeInfo>> {
    type_of(heap, call.receiver.as_tagged(heap))
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

/// `FFI.cstring(text)` -> VM-owned NUL-terminated `CPointer`.
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
    state.handle_scope(|scope| {
        let Some(result) = type_of(heap, args.get_handle(heap, 1).as_tagged(heap)) else {
            return raise_ffi(
                vm,
                heap,
                state,
                FfiError::InvalidArgument("expected a C type as the result"),
            );
        };
        let result = scope.handle(result);

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
                    let Some(ty) = type_of(heap, element) else {
                        return raise_ffi(vm, heap, state, FfiError::NotATypeObject);
                    };
                    params.push(scope.handle(ty));
                }
            }
        }

        match ffi.types().function(heap, &scope, &params, &result, false) {
            Ok(signature) => signature.as_tagged(heap).erase(),
            Err(error) => raise_ffi(vm, heap, state, error),
        }
    })
}

/// `FFI.pointer(type)` -> pointer `CType`.
fn ffi_pointer<'a>(ctx: HostCtx<'a>, _call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let ffi = &vm.runtime::<VmCore>().ffi;
    state.handle_scope(|scope| {
        let Some(pointee) = type_of(heap, args.get_handle(heap, 0).as_tagged(heap)) else {
            return raise_ffi(vm, heap, state, FfiError::NotATypeObject);
        };
        let pointee = scope.handle(pointee);
        let pointer = ffi.types().pointer_to(heap, &scope, &pointee);
        pointer.as_tagged(heap).erase()
    })
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
        let Some(signature) = type_of(heap, args.get_handle(heap, 1).as_tagged(heap)) else {
            return raise_ffi(vm, heap, state, FfiError::NotATypeObject);
        };
        let signature = scope.handle(signature);
        let Some(library) = library_receiver(heap, &call) else {
            return raise_ffi(vm, heap, state, FfiError::InvalidLibrary);
        };
        let library = scope.handle(library);
        let target = match ffi.library_symbol(heap, &scope, &library, &name, &signature) {
            Ok(target) => target,
            Err(error) => return raise_ffi(vm, heap, state, error),
        };
        match ffi.function(heap, &scope, trampoline, &signature, &target, &library) {
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
        let Some(ty) = type_of(heap, args.get_handle(heap, 1).as_tagged(heap)) else {
            return raise_ffi(vm, heap, state, FfiError::NotATypeObject);
        };
        let ty = scope.handle(ty);
        let Some(library) = library_receiver(heap, &call) else {
            return raise_ffi(vm, heap, state, FfiError::InvalidLibrary);
        };
        let library = scope.handle(library);
        match ffi.library_symbol(heap, &scope, &library, &name, &ty) {
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
    state.handle_scope(|scope| {
        let Some(fields) = args.get_handle(heap, 0).as_tagged(heap).as_heap_object() else {
            return raise_ffi(
                vm,
                heap,
                state,
                FfiError::InvalidArgument("expected an array of field pairs"),
            );
        };
        let mut collected: Vec<(Option<&str>, Handle<TypeInfo>)> =
            Vec::with_capacity(fields.length());
        let mut names = Vec::with_capacity(fields.length());
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
            names.push(name);
            let Some(ty) = pair
                .element_value(heap, 1)
                .and_then(|value| type_of(heap, value))
            else {
                return raise_ffi(vm, heap, state, FfiError::NotATypeObject);
            };
            collected.push((None, scope.handle(ty)));
        }
        for (index, name) in names.iter().enumerate() {
            collected[index].0 = name.as_deref();
        }
        match ffi.define_struct(vm, heap, &scope, None, &collected) {
            Ok(ty) => ty.as_tagged(heap).erase(),
            Err(error) => raise_ffi(vm, heap, state, error),
        }
    })
}

/// `type.size()` / `type.align()`.
fn c_type_size<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, _args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let Some(ty) = type_receiver(heap, &call) else {
        return raise_ffi(vm, heap, state, FfiError::NotATypeObject);
    };
    if !ty.is_object() {
        return raise_ffi(vm, heap, state, FfiError::NotAnObject(ty.id()));
    }
    Smi::new(ty.size() as i64).into_tagged()
}

fn c_type_align<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, _args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let Some(ty) = type_receiver(heap, &call) else {
        return raise_ffi(vm, heap, state, FfiError::NotATypeObject);
    };
    if !ty.is_object() {
        return raise_ffi(vm, heap, state, FfiError::NotAnObject(ty.id()));
    }
    Smi::new(ty.align() as i64).into_tagged()
}

/// `type.new(...values)` -> `CValue` with each field initialized in order.
fn c_type_new<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let ffi = &vm.runtime::<VmCore>().ffi;
    state.handle_scope(|scope| {
        let Some(ty) = type_receiver(heap, &call) else {
            return raise_ffi(vm, heap, state, FfiError::NotATypeObject);
        };
        let fields = match ty.kind() {
            TypeKind::Struct | TypeKind::Union => ty.field_names(heap),
            TypeKind::Array => Vec::new(),
            _ => {
                return raise_ffi(
                    vm,
                    heap,
                    state,
                    FfiError::InvalidArgument("new() needs a struct, union, or array type"),
                );
            }
        };
        let ty = scope.handle(ty);
        let value = match ffi.value(heap, &scope, &ty) {
            Ok(value) => value,
            Err(error) => return raise_ffi(vm, heap, state, error),
        };
        for (index, name) in fields.iter().enumerate() {
            if index >= args.len() {
                break;
            }
            let Some(name) = name else { continue };
            let guest = args.get_handle(heap, index).as_tagged(heap);
            if let Err(error) = ffi.value_set_field(heap, &value, name, guest) {
                return raise_ffi(vm, heap, state, error);
            }
        }
        value.as_tagged(heap).erase()
    })
}

fn value_receiver<'a>(heap: &'a Heap, call: &CallInfo<'_>) -> Option<Tagged<'a, CValue>> {
    call.receiver.as_tagged(heap).get_as::<CValue>(heap)
}

fn field_name(heap: &Heap, value: Tagged<Value>) -> Option<String> {
    value
        .get_as::<DenseString>(heap)
        .map(|name| name.to_rust_string(heap))
}

/// `value.get("field")`.
fn c_value_get<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let ffi = &vm.runtime::<VmCore>().ffi;
    state.handle_scope(|scope| {
        let key = args.get_handle(heap, 0).as_tagged(heap);
        let index = key.to_i64();
        let name = if index.is_none() {
            field_name(heap, key)
        } else {
            None
        };
        let Some(value) = value_receiver(heap, &call) else {
            return raise_ffi(
                vm,
                heap,
                state,
                FfiError::InvalidArgument("expected a C value"),
            );
        };
        let value = scope.handle(value);
        let read = if let Some(index) = index {
            ffi.value_get_index(heap, &scope, &value, index as usize)
        } else if let Some(name) = name {
            ffi.value_get_field(heap, &scope, &value, &name)
        } else {
            return raise_ffi(
                vm,
                heap,
                state,
                FfiError::InvalidArgument("expected a C field name or array index"),
            );
        };
        match read {
            Ok(ValueRead::View(view)) => view.as_tagged(heap).erase(),
            Ok(ValueRead::Raw(raw)) => ffi.read_raw(heap, &scope, &raw, None),
            Err(error) => raise_ffi(vm, heap, state, error),
        }
    })
}

/// `value.set("field", value)`.
fn c_value_set<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let ffi = &vm.runtime::<VmCore>().ffi;
    state.handle_scope(|scope| {
        let key = args.get_handle(heap, 0).as_tagged(heap);
        let index = key.to_i64();
        let name = if index.is_none() {
            field_name(heap, key)
        } else {
            None
        };
        let guest = args.get_handle(heap, 1).as_tagged(heap);
        let Some(value) = value_receiver(heap, &call) else {
            return raise_ffi(
                vm,
                heap,
                state,
                FfiError::InvalidArgument("expected a C value"),
            );
        };
        let value = scope.handle(value);
        let result = if let Some(index) = index {
            ffi.value_set_index(heap, &value, index as usize, guest)
        } else if let Some(name) = name {
            ffi.value_set_field(heap, &value, &name, guest)
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

/// `FFI.array(type, count)` -> array `CType`.
fn ffi_array<'a>(ctx: HostCtx<'a>, _call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let ffi = &vm.runtime::<VmCore>().ffi;
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
    state.handle_scope(|scope| {
        let Some(element) = type_of(heap, args.get_handle(heap, 0).as_tagged(heap)) else {
            return raise_ffi(vm, heap, state, FfiError::NotATypeObject);
        };
        let element = scope.handle(element);
        match ffi.types().array_of(heap, &scope, &element, count as usize) {
            Ok(ty) => ty.as_tagged(heap).erase(),
            Err(error) => raise_ffi(vm, heap, state, error.into()),
        }
    })
}

/// `pointer.add(elements)` -> pointer advanced by `elements * sizeof(pointee)`.
fn c_pointer_add<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let ffi = &vm.runtime::<VmCore>().ffi;
    state.handle_scope(|scope| {
        let Some(elements) = args.get_handle(heap, 0).as_tagged(heap).to_i64() else {
            return raise_ffi(
                vm,
                heap,
                state,
                FfiError::InvalidArgument("expected an integer offset"),
            );
        };
        let Some(pointer) = call.receiver.as_tagged(heap).get_as::<CPointer>(heap) else {
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
        let Some(index) = args.get_handle(heap, 0).as_tagged(heap).to_i64() else {
            return raise_ffi(
                vm,
                heap,
                state,
                FfiError::InvalidArgument("expected an integer index"),
            );
        };
        let Some(pointer) = call.receiver.as_tagged(heap).get_as::<CPointer>(heap) else {
            return raise_ffi(
                vm,
                heap,
                state,
                FfiError::InvalidArgument("expected a C pointer"),
            );
        };
        let pointer = scope.handle(pointer);
        let pointee = {
            let pointee = pointer.as_tagged(heap).pointee(heap);
            scope.handle(pointee)
        };
        if matches!(
            pointee.as_tagged(heap).kind(),
            TypeKind::Struct | TypeKind::Union | TypeKind::Array
        ) {
            let offset = index as usize * pointee.as_tagged(heap).size().max(1);
            let view = ffi.view_pointer(heap, &scope, &pointer, &pointee, offset);
            return view.as_tagged(heap).erase();
        }
        let raw = match ffi.pointer_read_raw(heap, &scope, &pointer, index as usize) {
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
        let Some(index) = args.get_handle(heap, 0).as_tagged(heap).to_i64() else {
            return raise_ffi(
                vm,
                heap,
                state,
                FfiError::InvalidArgument("expected an integer index"),
            );
        };
        let Some(pointer) = call.receiver.as_tagged(heap).get_as::<CPointer>(heap) else {
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

/// `value.pointer` -> `CPointer` to the value's storage.
fn c_value_pointer<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, _args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    state.handle_scope(|scope| {
        let Some(value) = value_receiver(heap, &call) else {
            return raise_ffi(
                vm,
                heap,
                state,
                FfiError::InvalidArgument("expected a C value"),
            );
        };
        let buffer = scope.handle(value);
        let ty = {
            let ty = buffer.as_tagged(heap).c_type(heap);
            scope.handle(ty)
        };
        // An array decays to a pointer to its first element, like C.
        let pointee = if ty.as_tagged(heap).kind() == TypeKind::Array {
            let element = ty
                .as_tagged(heap)
                .element(heap)
                .expect("array type without element");
            scope.handle(element)
        } else {
            ty.clone()
        };

        let (inline, external, offset) = {
            let source = buffer.as_tagged(heap);
            (
                source.inline_data(heap).map(|inline| scope.handle(inline)),
                source
                    .external_pointer(heap)
                    .map(|external| scope.handle(external)),
                source.offset(),
            )
        };
        let pointer = match (inline, external) {
            (Some(inline), _) => heap.allocate_handle::<CPointer>(
                CPointerInit {
                    pointee,
                    address: offset,
                    flags: CPointerFlags::VM_BACKED.union(CPointerFlags::BORROWED),
                    library: None,
                    owner: Some(inline),
                },
                &scope,
            ),
            (None, Some(external)) => {
                let (raw, flags, owner, library) = {
                    let source = external.as_tagged(heap);
                    (
                        source.raw_address(),
                        source.flags(),
                        source.owner(heap).map(|owner| scope.handle(owner)),
                        source.library(heap).map(|library| scope.handle(library)),
                    )
                };
                let pointer = heap.allocate_handle::<CPointer>(
                    CPointerInit {
                        pointee: pointee.clone(),
                        address: raw + offset,
                        flags,
                        library: None,
                        owner,
                    },
                    &scope,
                );
                if let Some(library) = library {
                    pointer
                        .as_tagged(heap)
                        .set_library(heap, library.as_tagged(heap));
                }
                pointer
            }
            (None, None) => {
                return raise_ffi(
                    vm,
                    heap,
                    state,
                    FfiError::InvalidArgument("C value has no storage"),
                );
            }
        };
        pointer.as_tagged(heap).erase()
    })
}

/// `pointer.cast(type)` -> retyped pointer.
fn c_pointer_cast<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let ffi = &vm.runtime::<VmCore>().ffi;
    state.handle_scope(|scope| {
        let Some(ty) = type_of(heap, args.get_handle(heap, 0).as_tagged(heap)) else {
            return raise_ffi(vm, heap, state, FfiError::NotATypeObject);
        };
        let Some(pointer) = call.receiver.as_tagged(heap).get_as::<CPointer>(heap) else {
            return raise_ffi(
                vm,
                heap,
                state,
                FfiError::InvalidArgument("expected a C pointer"),
            );
        };
        let pointer = scope.handle(pointer);
        let ty = scope.handle(ty);
        match ffi.cast_pointer(heap, &scope, &pointer, &ty) {
            Ok(pointer) => pointer.as_tagged(heap).erase(),
            Err(error) => raise_ffi(vm, heap, state, error),
        }
    })
}

/// The trampoline every `CFunction` maps to.
fn ffi_trampoline<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let Some(callee) = call.callee else {
        return raise_ffi(vm, heap, state, FfiError::NotCallable);
    };
    let ffi = &vm.runtime::<VmCore>().ffi;
    state.handle_scope(|scope| {
        let raw = match ffi.call_raw(heap, &scope, &callee, args) {
            Ok(raw) => raw,
            Err(error) => return raise_ffi(vm, heap, state, error),
        };
        let library = {
            let function = callee.as_tagged(heap).get_as::<CFunction>(heap);
            function
                .and_then(|function| function.library(heap))
                .map(|library| scope.handle(library))
        };
        ffi.read_raw(heap, &scope, &raw, library.as_ref())
    })
}

fn accessor_field<'a>(
    heap: &'a Heap,
    accessor: Tagged<'a, AccessorInfo>,
) -> Option<(Tagged<'a, TypeInfo>, usize)> {
    let data = accessor.data(heap)?.get_as::<FixedArray>(heap)?;
    let offset = data.at(heap, 0).to_i64()? as usize;
    let ty = data.at(heap, 1).get_as::<TypeInfo>(heap)?;
    Some((ty, offset))
}

/// Native getter behind a per-type field accessor.
fn c_value_field_get<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, _args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let ffi = &vm.runtime::<VmCore>().ffi;
    state.handle_scope(|scope| {
        let Some(accessor) = call
            .callee
            .as_ref()
            .and_then(|callee| callee.as_tagged(heap).get_as::<AccessorInfo>(heap))
        else {
            return raise_ffi(
                vm,
                heap,
                state,
                FfiError::InvalidArgument("missing field accessor"),
            );
        };
        let Some((field_ty, offset)) = accessor_field(heap, accessor) else {
            return raise_ffi(
                vm,
                heap,
                state,
                FfiError::InvalidArgument("malformed field accessor"),
            );
        };
        let field_ty = scope.handle(field_ty);
        let Some(value) = call.receiver.as_tagged(heap).get_as::<CValue>(heap) else {
            return raise_ffi(
                vm,
                heap,
                state,
                FfiError::InvalidArgument("expected a C value"),
            );
        };
        let value = scope.handle(value);
        match ffi.value_read(heap, &scope, &value, &field_ty, offset) {
            Ok(ValueRead::View(view)) => view.as_tagged(heap).erase(),
            Ok(ValueRead::Raw(raw)) => ffi.read_raw(heap, &scope, &raw, None),
            Err(error) => raise_ffi(vm, heap, state, error),
        }
    })
}

/// Native setter behind a per-type field accessor.
fn c_value_field_set<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let ffi = &vm.runtime::<VmCore>().ffi;
    state.handle_scope(|scope| {
        let Some(accessor) = call
            .callee
            .as_ref()
            .and_then(|callee| callee.as_tagged(heap).get_as::<AccessorInfo>(heap))
        else {
            return raise_ffi(
                vm,
                heap,
                state,
                FfiError::InvalidArgument("missing field accessor"),
            );
        };
        let Some((field_ty, offset)) = accessor_field(heap, accessor) else {
            return raise_ffi(
                vm,
                heap,
                state,
                FfiError::InvalidArgument("malformed field accessor"),
            );
        };
        let Some(value) = call.receiver.as_tagged(heap).get_as::<CValue>(heap) else {
            return raise_ffi(
                vm,
                heap,
                state,
                FfiError::InvalidArgument("expected a C value"),
            );
        };
        let value = scope.handle(value);
        let guest = args.get_handle(heap, 0).as_tagged(heap);
        match ffi.value_write(heap, &value, field_ty, offset, guest) {
            Ok(()) => heap.known().undefined.get(heap).erase(),
            Err(error) => raise_ffi(vm, heap, state, error),
        }
    })
}

/// `value.copy()` -> detached inline copy.
fn c_value_copy<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, _args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let ffi = &vm.runtime::<VmCore>().ffi;
    state.handle_scope(|scope| {
        let Some(value) = value_receiver(heap, &call) else {
            return raise_ffi(
                vm,
                heap,
                state,
                FfiError::InvalidArgument("expected a C value"),
            );
        };
        let value = scope.handle(value);
        match ffi.copy_value(heap, &scope, &value) {
            Ok(copy) => copy.as_tagged(heap).erase(),
            Err(error) => raise_ffi(vm, heap, state, error),
        }
    })
}

/// `type.wrap(pointer)` -> a value view over the pointer.
fn c_type_wrap<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let ffi = &vm.runtime::<VmCore>().ffi;
    state.handle_scope(|scope| {
        let Some(ty) = type_receiver(heap, &call) else {
            return raise_ffi(vm, heap, state, FfiError::NotATypeObject);
        };
        let Some(pointer) = args
            .get_handle(heap, 0)
            .as_tagged(heap)
            .get_as::<CPointer>(heap)
        else {
            return raise_ffi(
                vm,
                heap,
                state,
                FfiError::InvalidArgument("expected a C pointer"),
            );
        };
        let ty = scope.handle(ty);
        let pointer = scope.handle(pointer);
        match ffi.wrap_value(heap, &scope, &ty, &pointer) {
            Ok(value) => value.as_tagged(heap).erase(),
            Err(error) => raise_ffi(vm, heap, state, error),
        }
    })
}
