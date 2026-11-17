use mark_sweep::{MarkSweep, MarkSweepConfig};
use vm::ffi::{FfiError, TypeId, TypeKind, ValueRead};
use vm::{
    CValue, CValueFlags, CValueInit, DefaultInterpreter, FixedByteArray, Float, ForeignLibrary,
    Handle, HandleScope, Heap, Smi, StringRef, TypeInfo, Utf8String, VM, Value, VmCore,
};

fn test_vm() -> VM {
    VM::new::<MarkSweep, DefaultInterpreter>(MarkSweepConfig::default())
        .expect("failed to create heap")
        .add::<VmCore>()
        .expect("failed to install the VM core")
}

fn builtin<'s>(
    heap: &mut Heap,
    scope: &'s HandleScope<'_>,
    kind: TypeKind,
) -> Handle<'s, TypeInfo> {
    let id = TypeId::from_i64(kind as i64);
    scope.handle(heap.known().ffi_types.get(heap, id).expect("builtin"))
}

fn struct_type<'s>(
    vm: &VM,
    heap: &mut Heap,
    scope: &'s HandleScope<'_>,
    fields: &[(Option<&str>, Handle<TypeInfo>)],
) -> Handle<'s, TypeInfo> {
    vm.runtime::<VmCore>()
        .ffi
        .define_struct(vm, heap, scope, None, fields)
        .expect("struct type")
}

fn char_pointer<'s>(vm: &VM, heap: &mut Heap, scope: &'s HandleScope<'_>) -> Handle<'s, TypeInfo> {
    let char_ty = builtin(heap, scope, TypeKind::Char);
    vm.runtime::<VmCore>()
        .ffi
        .types()
        .pointer_to(heap, scope, &char_ty)
}

fn char16_pointer<'s>(
    vm: &VM,
    heap: &mut Heap,
    scope: &'s HandleScope<'_>,
) -> Handle<'s, TypeInfo> {
    let char16 = builtin(heap, scope, TypeKind::Char16);
    vm.runtime::<VmCore>()
        .ffi
        .types()
        .pointer_to(heap, scope, &char16)
}

fn raw_function_pointer<'s>(
    vm: &VM,
    heap: &mut Heap,
    scope: &'s HandleScope<'_>,
    library: &Handle<ForeignLibrary>,
    params: &[Handle<TypeInfo>],
    result: &Handle<TypeInfo>,
    address: usize,
) -> Handle<'s, CValue> {
    let core = vm.runtime::<VmCore>();
    let ffi = &core.ffi;
    let signature = ffi
        .types()
        .function(heap, scope, params, result, false)
        .expect("signature");
    let target = heap.allocate_handle::<CValue>(
        CValueInit {
            ty: signature.clone(),
            map: None,
            address: address as *mut u8,
            flags: CValueFlags::NONE,
            storage: None,
            library: None,
            callable: None,
        },
        scope,
    );
    ffi.function(heap, scope, core.ffi_call, &signature, &target, library)
        .expect("function")
}

fn call_pointer<'s>(
    vm: &VM,
    heap: &mut Heap,
    scope: &'s HandleScope<'_>,
    callee: &Handle<CValue>,
    args: &[Handle<Value>],
) -> Handle<'s, Value> {
    let ffi = &vm.runtime::<VmCore>().ffi;
    let callee = scope.handle(callee.as_tagged(heap).erase());
    let args = scope.stage_handles(args);
    let raw = ffi
        .call_raw(
            vm,
            vm.runtime::<VmCore>(),
            heap,
            None,
            scope,
            &callee,
            args.as_args(),
        )
        .expect("call");
    scope.handle(ffi.read_raw(heap, scope, &raw, None))
}

fn raw_pointer<'s>(
    heap: &mut Heap,
    scope: &'s HandleScope<'_>,
    ty: &Handle<TypeInfo>,
    address: usize,
    flags: CValueFlags,
) -> Handle<'s, CValue> {
    heap.allocate_handle::<CValue>(
        CValueInit {
            ty: ty.clone(),
            map: None,
            address: address as *mut u8,
            flags,
            storage: None,
            library: None,
            callable: None,
        },
        scope,
    )
}

fn call_value<'s>(
    vm: &VM,
    heap: &mut Heap,
    scope: &'s HandleScope<'_>,
    library: &Handle<ForeignLibrary>,
    address: usize,
    params: &[Handle<TypeInfo>],
    result: &Handle<TypeInfo>,
    args: &[Handle<Value>],
) -> Handle<'s, Value> {
    let function = raw_function_pointer(vm, heap, scope, library, params, result, address);
    call_pointer(vm, heap, scope, &function, args)
}

#[test]
fn scalar_pointer_roundtrip_and_unaligned() {
    let vm = test_vm();
    let mut thread = vm.attach();
    thread.handle_scope(|thread, scope| {
        let mut storage = Box::new([0u8; 64]);
        let base = storage.as_mut_ptr() as usize;
        let u8_ty = builtin(thread.heap(), &scope, TypeKind::U8);
        let i16_ty = builtin(thread.heap(), &scope, TypeKind::I16);
        let u32_ty = builtin(thread.heap(), &scope, TypeKind::U32);
        let u64_ty = builtin(thread.heap(), &scope, TypeKind::U64);
        let i64_ty = builtin(thread.heap(), &scope, TypeKind::I64);
        let f32_ty = builtin(thread.heap(), &scope, TypeKind::F32);
        let f64_ty = builtin(thread.heap(), &scope, TypeKind::F64);

        let u8_ptr = raw_pointer(thread.heap(), &scope, &u8_ty, base, CValueFlags::NONE);
        let i16_ptr = raw_pointer(thread.heap(), &scope, &i16_ty, base + 8, CValueFlags::NONE);
        let u32_ptr = raw_pointer(thread.heap(), &scope, &u32_ty, base + 16, CValueFlags::NONE);
        let i64_ptr = raw_pointer(thread.heap(), &scope, &i64_ty, base + 24, CValueFlags::NONE);
        let f32_ptr = raw_pointer(thread.heap(), &scope, &f32_ty, base + 32, CValueFlags::NONE);
        let f64_ptr = raw_pointer(thread.heap(), &scope, &f64_ty, base + 40, CValueFlags::NONE);

        let heap = thread.heap();
        let p = u8_ptr.as_tagged(heap);
        p.write_u8(heap, 0xab).unwrap();
        assert_eq!(p.read_u8(heap), Ok(0xab));
        assert_eq!(p.read_i8(heap), Ok(-85));

        let p = i16_ptr.as_tagged(heap);
        p.write_i16(heap, -1234).unwrap();
        assert_eq!(p.read_i16(heap), Ok(-1234));
        assert_eq!(p.read_u16(heap), Ok(-1234i16 as u16));

        let p = u32_ptr.as_tagged(heap);
        p.write_u32(heap, 0xdead_beef).unwrap();
        assert_eq!(p.read_u32(heap), Ok(0xdead_beef));

        let p = i64_ptr.as_tagged(heap);
        p.write_i64(heap, i64::MIN + 1).unwrap();
        assert_eq!(p.read_i64(heap), Ok(i64::MIN + 1));

        let p = f32_ptr.as_tagged(heap);
        p.write_f32(heap, -1.5).unwrap();
        assert_eq!(p.read_f32(heap), Ok(-1.5));

        let p = f64_ptr.as_tagged(heap);
        p.write_f64(heap, core::f64::consts::PI).unwrap();
        assert_eq!(p.read_f64(heap), Ok(core::f64::consts::PI));

        // Unaligned 64-bit access across a byte boundary.
        let unaligned = raw_pointer(
            thread.heap(),
            &scope,
            &u64_ty,
            storage.as_mut_ptr() as usize + 1,
            CValueFlags::NONE,
        );
        let heap = thread.heap();
        let p = unaligned.as_tagged(heap);
        p.write_u64(heap, 0x0102_0304_0506_0708).unwrap();
        assert_eq!(p.read_u64(heap), Ok(0x0102_0304_0506_0708));
        let mut out = [0u8; 8];
        p.read_bytes(heap, &mut out).unwrap();
        assert_eq!(out, [8, 7, 6, 5, 4, 3, 2, 1]);
    });
}

#[test]
fn null_and_dead_pointer_errors() {
    let vm = test_vm();
    let mut thread = vm.attach();
    thread.handle_scope(|thread, scope| {
        let u64_ty = builtin(thread.heap(), &scope, TypeKind::U64);
        let null = raw_pointer(thread.heap(), &scope, &u64_ty, 0, CValueFlags::NONE);
        let dead = raw_pointer(thread.heap(), &scope, &u64_ty, 0x1000, CValueFlags::DEAD);

        let heap = thread.heap();
        let p = null.as_tagged(heap);
        assert!(p.is_null());
        assert!(p.is_live());
        assert_eq!(p.read_u64(heap), Err(FfiError::NullPointer));

        let p = dead.as_tagged(heap);
        assert!(p.is_dead());
        assert!(!p.is_null());
        assert_eq!(p.read_u64(heap), Err(FfiError::DeadPointer));

        let ffi = &vm.runtime::<VmCore>().ffi;
        assert_eq!(
            ffi.wrap_value(thread.heap(), &scope, &u64_ty, &null).err(),
            Some(FfiError::NullPointer)
        );
    });
}

#[test]
fn pointer_arithmetic_and_bounds() {
    let vm = test_vm();
    let mut thread = vm.attach();
    thread.handle_scope(|thread, scope| {
        let ffi = &vm.runtime::<VmCore>().ffi;
        let i32_ty = builtin(thread.heap(), &scope, TypeKind::I32);
        let array = ffi
            .types()
            .array_of(thread.heap(), &scope, &i32_ty, 4)
            .unwrap();
        let buffer = ffi.value(thread.heap(), &scope, &array).unwrap();
        for index in 0..4 {
            let heap = thread.heap();
            ffi.value_set_index(
                heap,
                buffer.as_tagged(heap),
                index,
                Smi::new((index as i64) * 10).into_tagged(),
            )
            .unwrap();
        }

        let second = ffi
            .value_add(thread.heap(), &scope, buffer.clone(), 1)
            .unwrap();
        let read = ffi
            .value_get_index(thread.heap(), &scope, second, 0)
            .unwrap();
        let value = match read {
            ValueRead::View(view) => view.as_tagged(thread.heap()).erase(),
            ValueRead::Raw(raw) => ffi.read_raw(thread.heap(), &scope, &raw, None),
        };
        assert_eq!(value.to_i64(), Some(10));

        assert_eq!(
            ffi.value_get_index(thread.heap(), &scope, buffer, 4).err(),
            Some(FfiError::IndexOutOfBounds)
        );
    });
}

#[test]
fn library_ownership_and_close() {
    let vm = test_vm();
    let mut thread = vm.attach();
    thread.handle_scope(|thread, scope| {
        let ffi = &vm.runtime::<VmCore>().ffi;
        let library = ffi
            .open_library(&vm, thread.heap(), &scope, None)
            .expect("open");
        let result = builtin(thread.heap(), &scope, TypeKind::I32);
        let signature = ffi
            .types()
            .function(thread.heap(), &scope, &[], &result, false)
            .expect("signature");
        let symbol = ffi
            .library_symbol(thread.heap(), &scope, &library, "malloc", &signature)
            .expect("symbol");

        let heap = thread.heap();
        let p = symbol.as_tagged(heap);
        assert!(p.check_live(heap).is_ok());
        assert!(!p.is_null());
        let owned = p.library(heap).expect("symbol references its library");
        assert!(owned.is_live(heap));
        assert_eq!(ffi.library_path(heap, &library), Some(String::new()));

        ffi.close_library(heap, &library).unwrap();
        assert!(!library.as_tagged(heap).is_live(heap));
        assert_eq!(p.check_live(heap), Err(FfiError::LibraryClosed));
        assert!(!p.is_dead());
        assert_eq!(p.read_u32(heap), Err(FfiError::LibraryClosed));
        ffi.close_library(heap, &library).unwrap();
    });
}

#[test]
fn library_errors() {
    let vm = test_vm();
    let mut thread = vm.attach();
    thread.handle_scope(|thread, scope| {
        let ffi = &vm.runtime::<VmCore>().ffi;
        let library = ffi
            .open_library(&vm, thread.heap(), &scope, None)
            .expect("open");
        let result = builtin(thread.heap(), &scope, TypeKind::I32);
        let signature = ffi
            .types()
            .function(thread.heap(), &scope, &[], &result, false)
            .expect("signature");
        assert!(matches!(
            ffi.library_symbol(
                thread.heap(),
                &scope,
                &library,
                "kettevm_no_such_symbol",
                &signature
            ),
            Err(FfiError::SymbolNotFound { .. })
        ));
        assert!(matches!(
            ffi.open_library(
                &vm,
                thread.heap(),
                &scope,
                Some("/nonexistent/kettevm-no-such-library.dylib")
            ),
            Err(FfiError::LoadFailed(_))
        ));
        assert!(matches!(
            ffi.open_library(&vm, thread.heap(), &scope, Some("")),
            Err(FfiError::LoadFailed(_))
        ));
    });
}

extern "C" fn echo_i8(value: i8) -> i8 {
    value
}

extern "C" fn echo_i16(value: i16) -> i16 {
    value
}

extern "C" fn echo_i32(value: i32) -> i32 {
    value
}

extern "C" fn echo_i64(value: i64) -> i64 {
    value
}

extern "C" fn echo_u8(value: u8) -> u8 {
    value
}

extern "C" fn echo_u16(value: u16) -> u16 {
    value
}

extern "C" fn echo_u32(value: u32) -> u32 {
    value
}

extern "C" fn echo_u64(value: u64) -> u64 {
    value
}

extern "C" fn echo_f32(value: f32) -> f32 {
    value
}

extern "C" fn echo_f64(value: f64) -> f64 {
    value
}

#[test]
fn scalar_argument_and_return_conversions() {
    let vm = test_vm();
    let mut thread = vm.attach();
    thread.handle_scope(|thread, scope| {
        let ffi = &vm.runtime::<VmCore>().ffi;
        let library = ffi
            .open_library(&vm, thread.heap(), &scope, None)
            .expect("open");

        macro_rules! check_int {
            ($kind:expr, $function:expr, $value:expr) => {{
                let ty = builtin(thread.heap(), &scope, $kind);
                let arg = scope.handle(Smi::new($value).into_tagged());
                let result = call_value(
                    &vm,
                    thread.heap(),
                    &scope,
                    &library,
                    $function as *const () as usize,
                    &[ty.clone()],
                    &ty,
                    &[arg],
                );
                assert_eq!(result.as_tagged(thread.heap()).to_i64(), Some($value));
            }};
        }

        check_int!(TypeKind::I8, echo_i8, -12);
        check_int!(TypeKind::I16, echo_i16, -1234);
        check_int!(TypeKind::I32, echo_i32, -123456);
        check_int!(TypeKind::I64, echo_i64, -123456789);
        check_int!(TypeKind::U8, echo_u8, 200);
        check_int!(TypeKind::U16, echo_u16, 60000);
        check_int!(TypeKind::U32, echo_u32, 4000000000);
        check_int!(TypeKind::U64, echo_u64, 123456789);

        let floats: [(TypeKind, usize, f64); 2] = [
            (TypeKind::F32, echo_f32 as *const () as usize, -1.5),
            (TypeKind::F64, echo_f64 as *const () as usize, -2.25),
        ];
        for (kind, function, value) in floats {
            let ty = builtin(thread.heap(), &scope, kind);
            let arg = thread
                .heap()
                .allocate_handle::<Float>(value as f64, &scope)
                .as_tagged(thread.heap())
                .erase();
            let arg = scope.handle(arg);
            let result = call_value(
                &vm,
                thread.heap(),
                &scope,
                &library,
                function,
                &[ty.clone()],
                &ty,
                &[arg],
            );
            let heap = thread.heap();
            let result = result
                .as_tagged(heap)
                .get_as::<Float>(heap)
                .expect("float result");
            assert_eq!(result.as_ref().value.get(), value as f64);
        }
    });
}

extern "C" fn c_strlen(text: *const u8) -> usize {
    let mut len = 0;
    unsafe {
        while *text.add(len) != 0 {
            len += 1;
        }
    }
    len
}

extern "C" fn w_strlen(text: *const u16) -> i32 {
    let mut len = 0;
    unsafe {
        while *text.add(len) != 0 {
            len += 1;
        }
    }
    len as i32
}

static C_NAME: &[u8] = b"h\xc3\xa9llo\0";
static W_NAME: [u16; 3] = [0x68, 0x20ac, 0];

extern "C" fn c_name() -> *const u8 {
    C_NAME.as_ptr()
}

extern "C" fn w_name() -> *const u16 {
    W_NAME.as_ptr()
}

#[test]
fn string_marshalling() {
    let vm = test_vm();
    let mut thread = vm.attach();
    thread.handle_scope(|thread, scope| {
        let ffi = &vm.runtime::<VmCore>().ffi;
        let library = ffi
            .open_library(&vm, thread.heap(), &scope, None)
            .expect("open");

        let char_ptr = char_pointer(&vm, thread.heap(), &scope);
        let char16_ptr = char16_pointer(&vm, thread.heap(), &scope);
        let usize_ty = builtin(thread.heap(), &scope, TypeKind::U64);
        let i32_ty = builtin(thread.heap(), &scope, TypeKind::I32);

        let length = raw_function_pointer(
            &vm,
            thread.heap(),
            &scope,
            &library,
            &[char_ptr.clone()],
            &usize_ty,
            c_strlen as *const () as usize,
        );
        let wide_length = raw_function_pointer(
            &vm,
            thread.heap(),
            &scope,
            &library,
            &[char16_ptr.clone()],
            &i32_ty,
            w_strlen as *const () as usize,
        );
        let get_name = raw_function_pointer(
            &vm,
            thread.heap(),
            &scope,
            &library,
            &[],
            &char_ptr,
            c_name as *const () as usize,
        );
        let get_wide_name = raw_function_pointer(
            &vm,
            thread.heap(),
            &scope,
            &library,
            &[],
            &char16_ptr,
            w_name as *const () as usize,
        );

        for (text, expected) in [("hello", 5), ("héllo", 6), ("h€llo", 7)] {
            let value = if text == "héllo" {
                let utf8 = Utf8String::from_str(thread.heap(), &scope, text);
                scope.handle(utf8.as_tagged(thread.heap()).erase())
            } else {
                let string = thread.intern(&scope, text);
                scope.handle(string.as_tagged(thread.heap()).erase())
            };
            let value = call_pointer(&vm, thread.heap(), &scope, &length, &[value]);
            assert_eq!(value.as_tagged(thread.heap()).to_i64(), Some(expected));
        }

        let latin1 = thread.intern(&scope, "héllo");
        let arg = scope.handle(latin1.as_tagged(thread.heap()).erase());
        let value = call_pointer(&vm, thread.heap(), &scope, &length, &[arg]);
        assert_eq!(value.as_tagged(thread.heap()).to_i64(), Some(6));

        let bytes = thread
            .heap()
            .allocate_handle::<FixedByteArray>(b"bytes", &scope);
        let arg = scope.handle(bytes.as_tagged(thread.heap()).erase());
        let value = call_pointer(&vm, thread.heap(), &scope, &length, &[arg]);
        assert_eq!(value.as_tagged(thread.heap()).to_i64(), Some(5));

        let wide = thread.intern(&scope, "h€");
        let arg = scope.handle(wide.as_tagged(thread.heap()).erase());
        let value = call_pointer(&vm, thread.heap(), &scope, &wide_length, &[arg]);
        assert_eq!(value.as_tagged(thread.heap()).to_i64(), Some(2));

        let value = call_pointer(&vm, thread.heap(), &scope, &get_name, &[]);
        let heap = thread.heap();
        let text = StringRef::try_from(heap, value.as_tagged(heap)).expect("decoded string");
        assert_eq!(text.to_rust_string(heap), "héllo");

        let value = call_pointer(&vm, thread.heap(), &scope, &get_wide_name, &[]);
        let heap = thread.heap();
        let text = StringRef::try_from(heap, value.as_tagged(heap)).expect("decoded string");
        assert_eq!(text.to_rust_string(heap), "h€");
    });
}

#[repr(C)]
struct Color {
    r: u8,
    g: u8,
    b: u8,
    a: u8,
}

extern "C" fn color_to_int(color: Color) -> i32 {
    ((color.r as i32) << 24) | ((color.g as i32) << 16) | ((color.b as i32) << 8) | color.a as i32
}

#[repr(C)]
struct Vec2 {
    x: f32,
    y: f32,
}

extern "C" fn vector2_add(left: Vec2, right: Vec2) -> Vec2 {
    Vec2 {
        x: left.x + right.x,
        y: left.y + right.y,
    }
}

#[test]
fn struct_by_value_and_hfa() {
    let vm = test_vm();
    let mut thread = vm.attach();
    thread.handle_scope(|thread, scope| {
        let ffi = &vm.runtime::<VmCore>().ffi;
        let library = ffi
            .open_library(&vm, thread.heap(), &scope, None)
            .expect("open");

        let u8_ty = builtin(thread.heap(), &scope, TypeKind::U8);
        let color = struct_type(
            &vm,
            thread.heap(),
            &scope,
            &[
                (Some("r"), u8_ty.clone()),
                (Some("g"), u8_ty.clone()),
                (Some("b"), u8_ty.clone()),
                (Some("a"), u8_ty),
            ],
        );
        let i32_ty = builtin(thread.heap(), &scope, TypeKind::I32);
        let color_buffer = ffi.value(thread.heap(), &scope, &color).unwrap();
        for (name, value) in [("r", 255), ("g", 0), ("b", 0), ("a", 128)] {
            let heap = thread.heap();
            ffi.value_set_field(
                heap,
                color_buffer.as_tagged(heap),
                name,
                Smi::new(value).into_tagged(),
            )
            .unwrap();
        }
        let arg = scope.handle(color_buffer.as_tagged(thread.heap()).erase());
        let result = call_value(
            &vm,
            thread.heap(),
            &scope,
            &library,
            color_to_int as *const () as usize,
            &[color],
            &i32_ty,
            &[arg],
        );
        assert_eq!(
            result.as_tagged(thread.heap()).to_i64(),
            Some(0xFF00_0080u32 as i32 as i64)
        );

        let f32_ty = builtin(thread.heap(), &scope, TypeKind::F32);
        let vector2 = struct_type(
            &vm,
            thread.heap(),
            &scope,
            &[(Some("x"), f32_ty.clone()), (Some("y"), f32_ty)],
        );
        let left = ffi.value(thread.heap(), &scope, &vector2).unwrap();
        let right = ffi.value(thread.heap(), &scope, &vector2).unwrap();
        for (buffer, values) in [(&left, (1, 2)), (&right, (3, 4))] {
            for (name, value) in [("x", values.0), ("y", values.1)] {
                let heap = thread.heap();
                ffi.value_set_field(
                    heap,
                    buffer.as_tagged(heap),
                    name,
                    Smi::new(value).into_tagged(),
                )
                .unwrap();
            }
        }
        let args = {
            let heap = thread.heap();
            scope.stage(&[left.as_tagged(heap).erase(), right.as_tagged(heap).erase()])
        };
        let function = raw_function_pointer(
            &vm,
            thread.heap(),
            &scope,
            &library,
            &[vector2.clone(), vector2.clone()],
            &vector2,
            vector2_add as *const () as usize,
        );
        let callee = scope.handle(function.as_tagged(thread.heap()).erase());
        let (vm_ref, heap, state) = thread.split();
        let raw = ffi
            .call_raw(
                vm_ref,
                vm_ref.runtime::<VmCore>(),
                heap,
                Some(state),
                &scope,
                &callee,
                args.as_args(),
            )
            .expect("call");
        let result = scope.handle(ffi.read_raw(thread.heap(), &scope, &raw, None));
        let result = {
            let heap = thread.heap();
            scope.handle(
                result
                    .as_tagged(heap)
                    .get_as::<CValue>(heap)
                    .expect("struct result"),
            )
        };
        let x = read_float_field(&vm, thread, &scope, result.clone(), "x");
        let y = read_float_field(&vm, thread, &scope, result, "y");
        assert_eq!((x, y), (4.0, 6.0));
    });
}

fn read_float_field(
    vm: &VM,
    thread: &mut vm::Thread,
    scope: &HandleScope<'_>,
    value: Handle<CValue>,
    name: &str,
) -> f64 {
    let ffi = &vm.runtime::<VmCore>().ffi;
    let read = ffi
        .value_get_field(thread.heap(), scope, value, name)
        .expect("field");
    let value = match read {
        ValueRead::View(view) => scope.handle(view.as_tagged(thread.heap()).erase()),
        ValueRead::Raw(raw) => scope.handle(ffi.read_raw(thread.heap(), scope, &raw, None)),
    };
    let heap = thread.heap();
    value
        .as_tagged(heap)
        .get_as::<Float>(heap)
        .expect("float")
        .as_ref()
        .value
        .get()
}

#[test]
fn type_registry_weak_collection() {
    let vm = test_vm();
    let mut thread = vm.attach();

    let first = thread.handle_scope(|thread, scope| {
        let ffi = &vm.runtime::<VmCore>().ffi;
        let ty = ffi
            .define_struct(&vm, thread.heap(), &scope, Some("Transient"), &[])
            .expect("struct");
        ty.as_tagged(thread.heap()).id()
    });

    thread.heap().collect();
    assert!(
        vm.runtime::<VmCore>()
            .ffi
            .types()
            .get(thread.heap(), first)
            .is_none()
    );

    thread.handle_scope(|thread, scope| {
        let ffi = &vm.runtime::<VmCore>().ffi;
        let ty = ffi
            .define_struct(&vm, thread.heap(), &scope, Some("Transient"), &[])
            .expect("struct");
        assert_ne!(ty.as_tagged(thread.heap()).id(), first);
        let _ = scope;
    });
}
