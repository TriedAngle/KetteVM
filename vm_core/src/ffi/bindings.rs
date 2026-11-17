use crate::ffi::{FfiError, FfiResult, TypeId, TypeKind, ValueRead, memory};
use crate::{
    AccessorInfo, Args, CValue, CallInfo, Errors, FixedArray, FixedByteArray, ForeignLibrary,
    Handle, HandleSlice, Heap, HostCtx, Map, MapInit, MapKind, NativeFn, Object,
    PropertyDescriptor, SetupError, Smi, StringRef, Tagged, ThreadState, TypeInfo, VM, Value,
    VmCore, VmError,
};

/// Install the `FFI` namespace and register the FFI natives.
pub fn install(vm: &mut VM, state: &mut VmCore) -> Result<(), SetupError> {
    state.ffi_call = vm.register_native(ffi_trampoline);
    let library = vm.register_native(ffi_library);
    let cstring = vm.register_native(ffi_cstring);
    let wstring = vm.register_native(ffi_wstring);
    let string_at = vm.register_native(ffi_string_at);
    let wstring_at = vm.register_native(ffi_wstring_at);
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
    let array_type = vm.register_native(ffi_array);
    let field_get = vm.register_native(c_value_field_get);
    let field_set = vm.register_native(c_value_field_set);
    let value_copy = vm.register_native(c_value_copy);
    let value_add = vm.register_native(c_value_add);
    let value_cast = vm.register_native(c_value_cast);
    let value_free = vm.register_native(c_value_free);
    let type_wrap = vm.register_native(c_type_wrap);
    let buffer = vm.register_native(ffi_buffer);
    let read_bytes = vm.register_native(c_read_bytes);
    let write_bytes = vm.register_native(c_write_bytes);
    let scalars: Vec<(&'static str, _, _)> = SCALAR_METHODS
        .iter()
        .map(|&(name, read, write)| (name, vm.register_native(read), vm.register_native(write)))
        .collect();

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
        Object::install_method(thread, &scope, library_prototype, "fn", lib_fn, 3);
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
        Object::install_method(thread, &scope, buffer_prototype, "add", value_add, 1);
        Object::install_method(thread, &scope, buffer_prototype, "cast", value_cast, 1);
        Object::install_method(thread, &scope, buffer_prototype, "free", value_free, 0);
        Object::install_method(
            thread,
            &scope,
            buffer_prototype,
            "read_bytes",
            read_bytes,
            2,
        );
        Object::install_method(
            thread,
            &scope,
            buffer_prototype,
            "write_bytes",
            write_bytes,
            1,
        );
        {
            let heap = thread.heap();
            let map = heap.known().c_value_map.get(heap);
            map.prototype
                .set(heap, map.erase(), buffer_prototype.as_tagged(heap).erase());
        }

        for &(name, read, write) in &scalars {
            let read_name = format!("read_{name}");
            let write_name = format!("write_{name}");
            Object::install_method(thread, &scope, buffer_prototype, &read_name, read, 1);
            Object::install_method(thread, &scope, buffer_prototype, &write_name, write, 2);
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
        Object::install_method(thread, &scope, namespace, "wstring", wstring, 1);
        Object::install_method(thread, &scope, namespace, "string_at", string_at, 1);
        Object::install_method(thread, &scope, namespace, "wstring_at", wstring_at, 1);
        Object::install_method(thread, &scope, namespace, "pointer", pointer, 1);
        Object::install_method(thread, &scope, namespace, "struct", struct_type, 1);
        Object::install_method(thread, &scope, namespace, "array", array_type, 2);
        Object::install_method(thread, &scope, namespace, "buffer", buffer, 1);

        for (name, id) in [
            ("void", TypeId::VOID),
            ("bool", TypeId::BOOL),
            ("char", TypeId::CHAR),
            ("char16", TypeId::CHAR16),
            ("wchar", TypeId::WCHAR),
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
            let value = {
                let ty = thread
                    .heap()
                    .known()
                    .ffi_types
                    .get(thread.heap(), id)
                    .expect("C FFI builtins are installed during VM setup");
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

        let c_string = {
            let heap = thread.heap();
            let char_type = heap
                .known()
                .ffi_types
                .get(heap, TypeId::CHAR)
                .expect("C FFI builtins are installed during VM setup");
            let char_type = scope.handle(char_type);
            ffi.types().pointer_to(thread.heap(), &scope, &char_type)
        };
        let key = thread.intern(&scope, "CString");
        let key = scope.handle(key.as_tagged(thread.heap()).erase().as_name());
        Object::define_own_property_ordinary(
            thread.heap(),
            &scope,
            namespace,
            key,
            PropertyDescriptor::data(c_string.erase()),
        )
        .expect("install FFI CString type");

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

fn raise_ffi<'a>(
    vm: &'a VM,
    heap: &'a mut Heap,
    state: &'a ThreadState,
    error: FfiError,
) -> Tagged<'a, Value> {
    Errors::raise_with(vm, heap, state, VmError::FfiError, &error.to_string())
}

fn string_of<'a>(heap: &'a Heap, value: Tagged<'a, Value>) -> Option<String> {
    StringRef::try_from(heap, value).map(|string| string.to_rust_string(heap))
}

/// `FFI.library(path)` -> `ForeignLibrary`; `null` means the current process.
fn ffi_library<'a>(ctx: HostCtx<'a>, _call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let ffi = &vm.runtime::<VmCore>().ffi;
    let path = {
        let value = args.get_handle(heap, 0).as_tagged(heap);
        if value.is_nullish(heap) {
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

/// `FFI.cstring(text)` -> owned NUL-terminated C string value.
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

fn ffi_wstring<'a>(ctx: HostCtx<'a>, _call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let ffi = &vm.runtime::<VmCore>().ffi;
    state.handle_scope(|scope| {
        let value = args.get_handle(heap, 0).as_tagged(heap);
        let Some(units) = StringRef::try_from(heap, value).map(|text| text.to_units(heap)) else {
            return raise_ffi(
                vm,
                heap,
                state,
                FfiError::InvalidArgument("expected a string"),
            );
        };
        match ffi.wstring(heap, &scope, &units) {
            Ok(pointer) => pointer.as_tagged(heap).erase(),
            Err(error) => raise_ffi(vm, heap, state, error),
        }
    })
}

fn value_address(heap: &Heap, value: Tagged<Value>) -> FfiResult<*mut u8> {
    let value = value
        .get_as::<CValue>(heap)
        .ok_or(FfiError::InvalidArgument("expected a C value"))?;
    value.base(heap)
}

fn ffi_string_at<'a>(ctx: HostCtx<'a>, _call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let ffi = &vm.runtime::<VmCore>().ffi;
    state.handle_scope(|scope| {
        let value = args.get_handle(heap, 0).as_tagged(heap);
        let address = match value_address(heap, value) {
            Ok(address) => address,
            Err(error) => return raise_ffi(vm, heap, state, error),
        };
        match ffi.read_c_string(heap, &scope, address) {
            Ok(value) => value.as_tagged(heap).erase(),
            Err(error) => raise_ffi(vm, heap, state, error),
        }
    })
}

fn ffi_wstring_at<'a>(ctx: HostCtx<'a>, _call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let ffi = &vm.runtime::<VmCore>().ffi;
    state.handle_scope(|scope| {
        let value = args.get_handle(heap, 0).as_tagged(heap);
        let address = match value_address(heap, value) {
            Ok(address) => address,
            Err(error) => return raise_ffi(vm, heap, state, error),
        };
        match ffi.read_c_wstring(heap, &scope, address) {
            Ok(value) => value.as_tagged(heap).erase(),
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
        let Some(pointee) = args
            .get_handle(heap, 0)
            .as_tagged(heap)
            .get_as::<TypeInfo>(heap)
        else {
            return raise_ffi(vm, heap, state, FfiError::NotATypeObject);
        };
        let pointee = scope.handle(pointee);
        let pointer = ffi.types().pointer_to(heap, &scope, &pointee);
        pointer.as_tagged(heap).erase()
    })
}

/// `library.fn(name, [params], result)` -> callable C value.
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
        let Some(result) = args
            .get_handle(heap, 2)
            .as_tagged(heap)
            .get_as::<TypeInfo>(heap)
        else {
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
            let value = args.get_handle(heap, 1).as_tagged(heap);
            if !value.is_nullish(heap) {
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
                    let Some(ty) = element.get_as::<TypeInfo>(heap) else {
                        return raise_ffi(vm, heap, state, FfiError::NotATypeObject);
                    };
                    params.push(scope.handle(ty));
                }
            }
        }

        let signature = match ffi.types().function(heap, &scope, &params, &result, false) {
            Ok(signature) => signature,
            Err(error) => return raise_ffi(vm, heap, state, error),
        };
        let Some(library) = call.receiver.as_tagged(heap).get_as::<ForeignLibrary>(heap) else {
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

/// `library.symbol(name, type)` -> C value for the resolved symbol.
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
        let Some(ty) = args
            .get_handle(heap, 1)
            .as_tagged(heap)
            .get_as::<TypeInfo>(heap)
        else {
            return raise_ffi(vm, heap, state, FfiError::NotATypeObject);
        };
        let ty = scope.handle(ty);
        let Some(library) = call.receiver.as_tagged(heap).get_as::<ForeignLibrary>(heap) else {
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
        let Some(library) = call.receiver.as_tagged(heap).get_as::<ForeignLibrary>(heap) else {
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
        let Some(library) = call.receiver.as_tagged(heap).get_as::<ForeignLibrary>(heap) else {
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
                .and_then(|value| string_of(heap, value));
            names.push(name);
            let Some(ty) = pair
                .element_value(heap, 1)
                .and_then(|value| value.get_as::<TypeInfo>(heap))
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
    let Some(ty) = call.receiver.as_tagged(heap).get_as::<TypeInfo>(heap) else {
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
    let Some(ty) = call.receiver.as_tagged(heap).get_as::<TypeInfo>(heap) else {
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
        let Some(ty) = call.receiver.as_tagged(heap).get_as::<TypeInfo>(heap) else {
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
            if let Err(error) = ffi.value_set_field(heap, value.as_tagged(heap), name, guest) {
                return raise_ffi(vm, heap, state, error);
            }
        }
        value.as_tagged(heap).erase()
    })
}

/// `value.get("field")`.
/// `value.get(index | field)` -> array element or struct field.
fn c_value_get<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let ffi = &vm.runtime::<VmCore>().ffi;
    state.handle_scope(|scope| {
        let key = args.get_handle(heap, 0).as_tagged(heap);
        let index = key.to_i64().and_then(|index| usize::try_from(index).ok());
        let name = if index.is_none() {
            StringRef::try_from(heap, key).map(|name| name.to_rust_string(heap))
        } else {
            None
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
        let read = if let Some(index) = index {
            let is_array = value.as_tagged(heap).c_type(heap).as_ref().kind() == TypeKind::Array;
            if is_array {
                ffi.value_get_index(heap, &scope, value, index)
            } else {
                ffi.element_read_raw(heap, &scope, &value, index)
                    .map(ValueRead::Raw)
            }
        } else if let Some(name) = name {
            ffi.value_get_field(heap, &scope, value, &name)
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

/// `value.set(index | field, value)`.
fn c_value_set<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let ffi = &vm.runtime::<VmCore>().ffi;
    state.handle_scope(|scope| {
        let key = args.get_handle(heap, 0).as_tagged(heap);
        let index = key.to_i64().and_then(|index| usize::try_from(index).ok());
        let name = if index.is_none() {
            StringRef::try_from(heap, key).map(|name| name.to_rust_string(heap))
        } else {
            None
        };
        let guest = args.get_handle(heap, 1).as_tagged(heap);
        let Some(value) = call.receiver.as_tagged(heap).get_as::<CValue>(heap) else {
            return raise_ffi(
                vm,
                heap,
                state,
                FfiError::InvalidArgument("expected a C value"),
            );
        };
        let value = scope.handle(value);
        let result = if let Some(index) = index {
            let is_array = value.as_tagged(heap).c_type(heap).as_ref().kind() == TypeKind::Array;
            if is_array {
                ffi.value_set_index(heap, value.as_tagged(heap), index, guest)
            } else {
                ffi.element_write(heap, &value, index, guest)
            }
        } else if let Some(name) = name {
            ffi.value_set_field(heap, value.as_tagged(heap), &name, guest)
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
        let Some(element) = args
            .get_handle(heap, 0)
            .as_tagged(heap)
            .get_as::<TypeInfo>(heap)
        else {
            return raise_ffi(vm, heap, state, FfiError::NotATypeObject);
        };
        let element = scope.handle(element);
        match ffi.types().array_of(heap, &scope, &element, count as usize) {
            Ok(ty) => ty.as_tagged(heap).erase(),
            Err(error) => raise_ffi(vm, heap, state, error.into()),
        }
    })
}

/// The trampoline every C function value maps to.
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
        let library = callee
            .as_tagged(heap)
            .get_as::<CValue>(heap)
            .and_then(|function| function.library(heap))
            .map(|library| scope.handle(library));
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
        match ffi.value_read(heap, &scope, value, field_ty, offset) {
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
    state.handle_scope(|_scope| {
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
        let guest = args.get_handle(heap, 0).as_tagged(heap);
        match ffi.value_write(heap, value, field_ty, offset, guest) {
            Ok(()) => heap.known().undefined.get(heap).erase(),
            Err(error) => raise_ffi(vm, heap, state, error),
        }
    })
}

fn c_value_add<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let ffi = &vm.runtime::<VmCore>().ffi;
    state.handle_scope(|scope| {
        let Some(value) = call.receiver.as_tagged(heap).get_as::<CValue>(heap) else {
            return raise_ffi(
                vm,
                heap,
                state,
                FfiError::InvalidArgument("expected a C value"),
            );
        };
        let Some(elements) = args.get_handle(heap, 0).as_tagged(heap).to_i64() else {
            return raise_ffi(
                vm,
                heap,
                state,
                FfiError::InvalidArgument("expected an integer offset"),
            );
        };
        let value = scope.handle(value);
        match ffi.value_add(heap, &scope, value, elements) {
            Ok(value) => value.as_tagged(heap).erase(),
            Err(error) => raise_ffi(vm, heap, state, error),
        }
    })
}

fn c_value_cast<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let ffi = &vm.runtime::<VmCore>().ffi;
    state.handle_scope(|scope| {
        let Some(ty) = args
            .get_handle(heap, 0)
            .as_tagged(heap)
            .get_as::<TypeInfo>(heap)
        else {
            return raise_ffi(vm, heap, state, FfiError::NotATypeObject);
        };
        let Some(value) = call.receiver.as_tagged(heap).get_as::<CValue>(heap) else {
            return raise_ffi(
                vm,
                heap,
                state,
                FfiError::InvalidArgument("expected a C value"),
            );
        };
        let ty = scope.handle(ty);
        let value = scope.handle(value);
        ffi.value_cast(heap, &scope, value, ty)
            .as_tagged(heap)
            .erase()
    })
}

/// `value.copy()` -> detached copy with its own storage.
fn c_value_copy<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, _args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let ffi = &vm.runtime::<VmCore>().ffi;
    state.handle_scope(|scope| {
        let Some(value) = call.receiver.as_tagged(heap).get_as::<CValue>(heap) else {
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

/// `type.wrap(value | byteArray)` -> a borrowed value view.
fn c_type_wrap<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let ffi = &vm.runtime::<VmCore>().ffi;
    state.handle_scope(|scope| {
        let Some(ty) = call.receiver.as_tagged(heap).get_as::<TypeInfo>(heap) else {
            return raise_ffi(vm, heap, state, FfiError::NotATypeObject);
        };
        let ty = scope.handle(ty);
        let source = args.get_handle(heap, 0).as_tagged(heap);
        if let Some(value) = source.get_as::<CValue>(heap) {
            let value = scope.handle(value);
            return match ffi.wrap_value(heap, &scope, &ty, &value) {
                Ok(value) => value.as_tagged(heap).erase(),
                Err(error) => raise_ffi(vm, heap, state, error),
            };
        }
        if let Some(bytes) = source.get_as::<FixedByteArray>(heap) {
            let bytes = scope.handle(bytes);
            let value = ffi.wrap_bytes(heap, &scope, &ty, &bytes);
            return value.as_tagged(heap).erase();
        }
        raise_ffi(
            vm,
            heap,
            state,
            FfiError::InvalidArgument("expected a C value or byte array"),
        )
    })
}

/// `value.free()` -> release owned external storage.
fn c_value_free<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, _args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let ffi = &vm.runtime::<VmCore>().ffi;
    state.handle_scope(|scope| {
        let Some(value) = call.receiver.as_tagged(heap).get_as::<CValue>(heap) else {
            return raise_ffi(
                vm,
                heap,
                state,
                FfiError::InvalidArgument("expected a C value"),
            );
        };
        let value = scope.handle(value);
        match ffi.free_value(heap, &value) {
            Ok(()) => heap.known().undefined.get(heap).erase(),
            Err(error) => raise_ffi(vm, heap, state, error),
        }
    })
}

/// Base address and available bytes of a C value.
fn receiver_window(heap: &Heap, receiver: Tagged<Value>) -> FfiResult<(*mut u8, usize)> {
    let value = receiver
        .get_as::<CValue>(heap)
        .ok_or(FfiError::InvalidArgument("expected a C value"))?;
    Ok((value.base(heap)?, value.remaining(heap)))
}

/// `base + offset`, checked against the receiver's remaining bytes.
fn checked_span(window: (*mut u8, usize), offset: usize, len: usize) -> FfiResult<*mut u8> {
    let end = offset.checked_add(len).ok_or(FfiError::AddressOverflow)?;
    if end > window.1 {
        return Err(FfiError::InvalidArgument("access out of bounds"));
    }
    // Safety: the caller keeps the receiver alive across the access.
    Ok(unsafe { window.0.add(offset) })
}

fn builtin_type(heap: &Heap, id: TypeId) -> Tagged<'_, TypeInfo> {
    heap.known()
        .ffi_types
        .get(heap, id)
        .expect("C FFI builtins are installed during VM setup")
}

fn byte_offset(heap: &Heap, args: Args, index: usize) -> Option<usize> {
    args.get_handle(heap, index)
        .as_tagged(heap)
        .to_i64()
        .and_then(|offset| usize::try_from(offset).ok())
}

/// `receiver.read_<t>(byteOffset)`.
fn scalar_read<'a>(
    ctx: HostCtx<'a>,
    call: CallInfo<'_>,
    args: Args,
    id: TypeId,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let ffi = &vm.runtime::<VmCore>().ffi;
    state.handle_scope(|scope| {
        let Some(offset) = byte_offset(heap, args, 0) else {
            return raise_ffi(
                vm,
                heap,
                state,
                FfiError::InvalidArgument("expected a byte offset"),
            );
        };
        let ty = {
            let ty = builtin_type(heap, id);
            scope.handle(ty)
        };
        let size = ty.as_tagged(heap).size();
        let window = match receiver_window(heap, call.receiver.as_tagged(heap)) {
            Ok(window) => window,
            Err(error) => return raise_ffi(vm, heap, state, error),
        };
        let at = match checked_span(window, offset, size) {
            Ok(at) => at,
            Err(error) => return raise_ffi(vm, heap, state, error),
        };
        let mut bytes = [0u8; 8];
        memory::copy_out(at, &mut bytes[..size]);
        ffi.read_value(heap, &scope, &ty, &bytes[..size], None)
    })
}

/// `receiver.write_<t>(byteOffset, value)`.
fn scalar_write<'a>(
    ctx: HostCtx<'a>,
    call: CallInfo<'_>,
    args: Args,
    id: TypeId,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let ffi = &vm.runtime::<VmCore>().ffi;
    state.handle_scope(|_scope| {
        let Some(offset) = byte_offset(heap, args, 0) else {
            return raise_ffi(
                vm,
                heap,
                state,
                FfiError::InvalidArgument("expected a byte offset"),
            );
        };
        let guest = args.get_handle(heap, 1).as_tagged(heap);
        let ty = builtin_type(heap, id);
        let size = ty.size();
        let window = match receiver_window(heap, call.receiver.as_tagged(heap)) {
            Ok(window) => window,
            Err(error) => return raise_ffi(vm, heap, state, error),
        };
        let at = match checked_span(window, offset, size) {
            Ok(at) => at,
            Err(error) => return raise_ffi(vm, heap, state, error),
        };
        let mut bytes = [0u8; 8];
        if let Err(error) = ffi.write_typed(heap, ty, guest, &mut bytes[..size]) {
            return raise_ffi(vm, heap, state, error);
        }
        memory::copy_in(at, &bytes[..size]);
        heap.known().undefined.get(heap).erase()
    })
}

macro_rules! scalar_methods {
    ($($read:ident, $write:ident, $id:ident, $name:literal;)*) => {
        $(
            fn $read<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
                scalar_read(ctx, call, args, TypeId::$id)
            }

            fn $write<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
                scalar_write(ctx, call, args, TypeId::$id)
            }
        )*

        const SCALAR_METHODS: &[(&str, NativeFn, NativeFn)] = &[
            $(($name, $read, $write),)*
        ];
    };
}

scalar_methods! {
    read_i8, write_i8, I8, "i8";
    read_u8, write_u8, U8, "u8";
    read_i16, write_i16, I16, "i16";
    read_u16, write_u16, U16, "u16";
    read_i32, write_i32, I32, "i32";
    read_u32, write_u32, U32, "u32";
    read_i64, write_i64, I64, "i64";
    read_u64, write_u64, U64, "u64";
    read_f32, write_f32, F32, "f32";
    read_f64, write_f64, F64, "f64";
}

/// `FFI.buffer(size)` -> zeroed `u8[size]` buffer.
fn ffi_buffer<'a>(ctx: HostCtx<'a>, _call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let ffi = &vm.runtime::<VmCore>().ffi;
    state.handle_scope(|scope| {
        let Some(len) = byte_offset(heap, args, 0) else {
            return raise_ffi(
                vm,
                heap,
                state,
                FfiError::InvalidArgument("expected a byte length"),
            );
        };
        let element = {
            let ty = builtin_type(heap, TypeId::U8);
            scope.handle(ty)
        };
        let ty = match ffi.types.array_of(heap, &scope, &element, len) {
            Ok(ty) => ty,
            Err(error) => return raise_ffi(vm, heap, state, error.into()),
        };
        match ffi.value(heap, &scope, &ty) {
            Ok(value) => value.as_tagged(heap).erase(),
            Err(error) => raise_ffi(vm, heap, state, error),
        }
    })
}

/// `receiver.read_bytes(byteOffset, len)` -> new `u8[len]` buffer.
fn c_read_bytes<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let ffi = &vm.runtime::<VmCore>().ffi;
    state.handle_scope(|scope| {
        let (Some(offset), Some(len)) = (byte_offset(heap, args, 0), byte_offset(heap, args, 1))
        else {
            return raise_ffi(
                vm,
                heap,
                state,
                FfiError::InvalidArgument("expected a byte offset and length"),
            );
        };
        let element = {
            let ty = builtin_type(heap, TypeId::U8);
            scope.handle(ty)
        };
        let ty = match ffi.types.array_of(heap, &scope, &element, len) {
            Ok(ty) => ty,
            Err(error) => return raise_ffi(vm, heap, state, error.into()),
        };
        let value = match ffi.value(heap, &scope, &ty) {
            Ok(value) => value,
            Err(error) => return raise_ffi(vm, heap, state, error),
        };
        // Re-derive the source after the allocation above.
        let window = match receiver_window(heap, call.receiver.as_tagged(heap)) {
            Ok(window) => window,
            Err(error) => return raise_ffi(vm, heap, state, error),
        };
        let at = match checked_span(window, offset, len) {
            Ok(at) => at,
            Err(error) => return raise_ffi(vm, heap, state, error),
        };
        let to = match value.as_tagged(heap).base(heap) {
            Ok(base) => base,
            Err(error) => return raise_ffi(vm, heap, state, error),
        };
        memory::copy_bytes(at, to, len);
        value.as_tagged(heap).erase()
    })
}

/// `receiver.write_bytes(source, byteOffset)` -> copy `source`'s bytes in.
fn c_write_bytes<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    state.handle_scope(|_scope| {
        let source = match args
            .get_handle(heap, 0)
            .as_tagged(heap)
            .get_as::<CValue>(heap)
        {
            Some(source) => source,
            None => {
                return raise_ffi(
                    vm,
                    heap,
                    state,
                    FfiError::InvalidArgument("expected a C buffer"),
                );
            }
        };
        let offset = byte_offset(heap, args, 1).unwrap_or(0);
        let len = source.c_type(heap).size();
        let from = match source.base(heap) {
            Ok(base) => base,
            Err(error) => return raise_ffi(vm, heap, state, error),
        };
        let window = match receiver_window(heap, call.receiver.as_tagged(heap)) {
            Ok(window) => window,
            Err(error) => return raise_ffi(vm, heap, state, error),
        };
        let at = match checked_span(window, offset, len) {
            Ok(at) => at,
            Err(error) => return raise_ffi(vm, heap, state, error),
        };
        memory::copy_bytes(from, at, len);
        heap.known().undefined.get(heap).erase()
    })
}
