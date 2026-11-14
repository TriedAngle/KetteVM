use mark_sweep::{MarkSweep, MarkSweepConfig};
use vm::ffi::{TypeId, TypeKind};
use vm::{
    CPointer, CPointerFlags, CPointerInit, CValue, DefaultInterpreter, FixedByteArray, Handle,
    HandleScope, Heap, Smi, StringRef, TypeInfo, Utf8String, VM, VmCore,
};

fn test_vm() -> VM {
    VM::new::<MarkSweep, DefaultInterpreter>(MarkSweepConfig::default())
        .expect("failed to create heap")
        .add::<VmCore>()
        .expect("failed to install the VM core")
}

fn builtin<'s>(
    _vm: &VM,
    heap: &mut Heap,
    scope: &'s HandleScope<'_>,
    kind: TypeKind,
) -> Handle<'s, TypeInfo> {
    let id = TypeId::from_i64(kind as i64);
    scope.handle(heap.known().ffi_types.get(heap, id).expect("builtin"))
}

fn function_pointer<'s>(
    vm: &VM,
    heap: &mut Heap,
    scope: &'s HandleScope<'_>,
    library: &Handle<vm::ForeignLibrary>,
    name: &str,
    params: &[Handle<TypeInfo>],
    result: &Handle<TypeInfo>,
) -> Handle<'s, vm::CFunction> {
    let core = vm.runtime::<VmCore>();
    let ffi = &core.ffi;
    let signature = ffi
        .types()
        .function(heap, scope, params, result, false)
        .expect("signature");
    let target = ffi
        .library_symbol(heap, scope, library, name, &signature)
        .expect("symbol");
    ffi.function(heap, scope, core.ffi_call, &signature, &target, library)
        .expect("function")
}

fn char_pointer<'s>(vm: &VM, heap: &mut Heap, scope: &'s HandleScope<'_>) -> Handle<'s, TypeInfo> {
    let char_ty = builtin(vm, heap, scope, TypeKind::Char);
    vm.runtime::<VmCore>()
        .ffi
        .types()
        .pointer_to(heap, scope, &char_ty)
}

fn cstring<'s>(
    vm: &VM,
    heap: &mut Heap,
    scope: &'s HandleScope<'_>,
    text: &str,
) -> Handle<'s, CValue> {
    vm.runtime::<VmCore>()
        .ffi
        .cstring(heap, scope, text)
        .expect("cstring")
}

#[test]
fn calls_strlen_through_a_cfunction() {
    let vm = test_vm();
    let mut thread = vm.attach();
    thread.handle_scope(|thread, scope| {
        let ffi = &vm.runtime::<VmCore>().ffi;
        let library = ffi
            .open_library(&vm, thread.heap(), &scope, None)
            .expect("open");
        let param = char_pointer(&vm, thread.heap(), &scope);
        let result = builtin(&vm, thread.heap(), &scope, TypeKind::U64);
        let strlen = function_pointer(
            &vm,
            thread.heap(),
            &scope,
            &library,
            "strlen",
            &[param],
            &result,
        );

        let text = cstring(&vm, thread.heap(), &scope, "hello");
        let callee = scope.handle(strlen.as_tagged(thread.heap()).erase());
        let args = scope.stage(&[text.as_tagged(thread.heap()).erase()]);
        let raw = ffi
            .call_raw(thread.heap(), &scope, &callee, args.as_args())
            .expect("call");
        let value = ffi.read_raw(thread.heap(), &scope, &raw, None);
        assert_eq!(value.to_i64(), Some(5));
    });
}

#[test]
fn raylib_text_length_when_available() {
    let path = "/opt/homebrew/lib/libraylib.dylib";
    if !std::path::Path::new(path).exists() {
        return;
    }
    let vm = test_vm();
    let mut thread = vm.attach();
    thread.handle_scope(|thread, scope| {
        let ffi = &vm.runtime::<VmCore>().ffi;
        let library = ffi
            .open_library(&vm, thread.heap(), &scope, Some(path))
            .expect("open raylib");
        let param = char_pointer(&vm, thread.heap(), &scope);
        let result = builtin(&vm, thread.heap(), &scope, TypeKind::U32);
        let text_length = function_pointer(
            &vm,
            thread.heap(),
            &scope,
            &library,
            "TextLength",
            &[param],
            &result,
        );
        let text = cstring(&vm, thread.heap(), &scope, "hello raylib");
        let callee = scope.handle(text_length.as_tagged(thread.heap()).erase());
        let args = scope.stage(&[text.as_tagged(thread.heap()).erase()]);
        let raw = ffi
            .call_raw(thread.heap(), &scope, &callee, args.as_args())
            .expect("call");
        let value = ffi.read_raw(thread.heap(), &scope, &raw, None);
        assert_eq!(value.to_i64(), Some(12));
    });
}

fn call_pointer<'h, 's>(
    vm: &VM,
    heap: &'h mut Heap,
    scope: &'s HandleScope<'_>,
    callee: &Handle<vm::CFunction>,
    args: &[Handle<'_, vm::Value>],
) -> Handle<'s, vm::Value> {
    let ffi = &vm.runtime::<VmCore>().ffi;
    let callee = scope.handle(callee.as_tagged(heap).erase());
    let args = scope.stage_handles(args);
    let raw = ffi
        .call_raw(heap, scope, &callee, args.as_args())
        .expect("call");
    scope.handle(ffi.read_raw(heap, scope, &raw, None))
}

fn raw_function_pointer<'s>(
    vm: &VM,
    heap: &mut Heap,
    scope: &'s HandleScope<'_>,
    library: &Handle<vm::ForeignLibrary>,
    params: &[Handle<TypeInfo>],
    result: &Handle<TypeInfo>,
    address: usize,
) -> Handle<'s, vm::CFunction> {
    let core = vm.runtime::<VmCore>();
    let ffi = &core.ffi;
    let signature = ffi
        .types()
        .function(heap, scope, params, result, false)
        .expect("signature");
    let target = heap.allocate_handle::<CPointer>(
        CPointerInit {
            pointee: signature.clone(),
            address: address as *mut u8,
            flags: CPointerFlags::NONE,
            library: None,
        },
        scope,
    );
    ffi.function(heap, scope, core.ffi_call, &signature, &target, library)
        .expect("function")
}

extern "C" fn utf16_len(text: *const u16) -> i32 {
    let mut len = 0;
    unsafe {
        while *text.add(len) != 0 {
            len += 1;
        }
    }
    len as i32
}

static UTF16_NAME: [u16; 3] = [b'h' as u16, 0x20ac, 0];

extern "C" fn utf16_name() -> *const u16 {
    UTF16_NAME.as_ptr()
}

#[test]
fn string_arguments_borrow_and_transcode() {
    let vm = test_vm();
    let mut thread = vm.attach();
    thread.handle_scope(|thread, scope| {
        let ffi = &vm.runtime::<VmCore>().ffi;
        let library = ffi
            .open_library(&vm, thread.heap(), &scope, None)
            .expect("open");
        let param = char_pointer(&vm, thread.heap(), &scope);
        let result = builtin(&vm, thread.heap(), &scope, TypeKind::U64);
        let strlen = function_pointer(
            &vm,
            thread.heap(),
            &scope,
            &library,
            "strlen",
            &[param],
            &result,
        );

        let ascii = thread.intern(&scope, "hello");
        let arg = scope.handle(ascii.as_tagged(thread.heap()).erase());
        let value = call_pointer(&vm, thread.heap(), &scope, &strlen, &[arg]);
        assert_eq!(value.as_tagged(thread.heap()).to_i64(), Some(5));

        let wide = Utf8String::from_str(thread.heap(), &scope, "héllo");
        let arg = scope.handle(wide.as_tagged(thread.heap()).erase());
        let value = call_pointer(&vm, thread.heap(), &scope, &strlen, &[arg]);
        assert_eq!(value.as_tagged(thread.heap()).to_i64(), Some(6));

        // A DenseString keeps Latin-1 bytes; `char*` still needs UTF-8.
        let latin1 = thread.intern(&scope, "héllo");
        let arg = scope.handle(latin1.as_tagged(thread.heap()).erase());
        let value = call_pointer(&vm, thread.heap(), &scope, &strlen, &[arg]);
        assert_eq!(value.as_tagged(thread.heap()).to_i64(), Some(6));

        // A UTF-16 DenseString transcodes through code units.
        let utf16 = thread.intern(&scope, "h€llo");
        let arg = scope.handle(utf16.as_tagged(thread.heap()).erase());
        let value = call_pointer(&vm, thread.heap(), &scope, &strlen, &[arg]);
        assert_eq!(value.as_tagged(thread.heap()).to_i64(), Some(7));

        let bytes = thread
            .heap()
            .allocate_handle::<FixedByteArray>(b"bytes", &scope);
        let arg = scope.handle(bytes.as_tagged(thread.heap()).erase());
        let value = call_pointer(&vm, thread.heap(), &scope, &strlen, &[arg]);
        assert_eq!(value.as_tagged(thread.heap()).to_i64(), Some(5));
    });
}

#[test]
fn char_pointer_returns_decode_to_guest_strings() {
    let vm = test_vm();
    let mut thread = vm.attach();
    thread.handle_scope(|thread, scope| {
        let ffi = &vm.runtime::<VmCore>().ffi;
        let library = ffi
            .open_library(&vm, thread.heap(), &scope, None)
            .expect("open");
        let param = char_pointer(&vm, thread.heap(), &scope);
        let result = char_pointer(&vm, thread.heap(), &scope);
        let getenv = function_pointer(
            &vm,
            thread.heap(),
            &scope,
            &library,
            "getenv",
            &[param],
            &result,
        );
        let name = thread.intern(&scope, "HOME");
        let arg = scope.handle(name.as_tagged(thread.heap()).erase());
        let value = call_pointer(&vm, thread.heap(), &scope, &getenv, &[arg]);
        let heap = thread.heap();
        let view = StringRef::try_from(heap, value.as_tagged(heap)).expect("decoded string");
        let text = view.to_rust_string(heap);
        assert_eq!(text, std::env::var("HOME").expect("HOME"));

        let missing = thread.intern(&scope, "KETTEVM_MISSING_ENV");
        let arg = scope.handle(missing.as_tagged(thread.heap()).erase());
        let value = call_pointer(&vm, thread.heap(), &scope, &getenv, &[arg]);
        let heap = thread.heap();
        assert_eq!(value.as_tagged(heap), heap.known().null.get(heap).erase());
    });
}

#[test]
fn char16_strings_are_marshalled_and_decoded() {
    let vm = test_vm();
    let mut thread = vm.attach();
    thread.handle_scope(|thread, scope| {
        let ffi = &vm.runtime::<VmCore>().ffi;
        let library = ffi
            .open_library(&vm, thread.heap(), &scope, None)
            .expect("open");
        let char16_pointer = {
            let char16 = builtin(&vm, thread.heap(), &scope, TypeKind::Char16);
            ffi.types().pointer_to(thread.heap(), &scope, &char16)
        };
        let result = builtin(&vm, thread.heap(), &scope, TypeKind::I32);

        let length = raw_function_pointer(
            &vm,
            thread.heap(),
            &scope,
            &library,
            &[char16_pointer.clone()],
            &result,
            utf16_len as *const () as usize,
        );
        let name = raw_function_pointer(
            &vm,
            thread.heap(),
            &scope,
            &library,
            &[],
            &char16_pointer,
            utf16_name as *const () as usize,
        );

        let wide = thread.intern(&scope, "h€");
        let arg = scope.handle(wide.as_tagged(thread.heap()).erase());
        let value = call_pointer(&vm, thread.heap(), &scope, &length, &[arg]);
        assert_eq!(value.as_tagged(thread.heap()).to_i64(), Some(2));

        let narrow = thread.intern(&scope, "hi");
        let arg = scope.handle(narrow.as_tagged(thread.heap()).erase());
        let value = call_pointer(&vm, thread.heap(), &scope, &length, &[arg]);
        assert_eq!(value.as_tagged(thread.heap()).to_i64(), Some(2));

        let utf8 = Utf8String::from_str(thread.heap(), &scope, "h€");
        let arg = scope.handle(utf8.as_tagged(thread.heap()).erase());
        let value = call_pointer(&vm, thread.heap(), &scope, &length, &[arg]);
        assert_eq!(value.as_tagged(thread.heap()).to_i64(), Some(2));

        let allocated = ffi
            .wstring(thread.heap(), &scope, &[b'h' as u16, 0x20ac])
            .expect("wstring");
        let arg = scope.handle(allocated.as_tagged(thread.heap()).erase());
        let value = call_pointer(&vm, thread.heap(), &scope, &length, &[arg]);
        assert_eq!(value.as_tagged(thread.heap()).to_i64(), Some(2));

        let value = call_pointer(&vm, thread.heap(), &scope, &name, &[]);
        let heap = thread.heap();
        let view = StringRef::try_from(heap, value.as_tagged(heap)).expect("decoded string");
        assert_eq!(view.to_rust_string(heap), "h€");
    });
}

#[test]
fn explicit_string_readers_copy() {
    let vm = test_vm();
    let mut thread = vm.attach();
    thread.handle_scope(|thread, scope| {
        let ffi = &vm.runtime::<VmCore>().ffi;
        let text = ffi.cstring(thread.heap(), &scope, "hé").expect("cstring");
        let address = {
            let heap = thread.heap();
            text.as_tagged(heap).base(heap).expect("base")
        };
        let decoded = ffi
            .read_c_string(thread.heap(), &scope, address)
            .expect("string_at");
        let heap = thread.heap();
        assert_eq!(decoded.as_tagged(heap).to_rust_string(heap), "hé");

        let wide = ffi
            .wstring(thread.heap(), &scope, &[b'h' as u16, 0x20ac])
            .expect("wstring");
        let address = {
            let heap = thread.heap();
            wide.as_tagged(heap).base(heap).expect("base")
        };
        let decoded = ffi
            .read_c_wstring(thread.heap(), &scope, address)
            .expect("wstring_at");
        let heap = thread.heap();
        assert_eq!(decoded.as_tagged(heap).to_rust_string(heap), "h€");
    });
}

#[test]
fn byte_arrays_pass_to_raw_byte_pointers() {
    let vm = test_vm();
    let mut thread = vm.attach();
    thread.handle_scope(|thread, scope| {
        let ffi = &vm.runtime::<VmCore>().ffi;
        let library = ffi
            .open_library(&vm, thread.heap(), &scope, None)
            .expect("open");
        let void = builtin(&vm, thread.heap(), &scope, TypeKind::Void);
        let byte_pointer = ffi.types().pointer_to(thread.heap(), &scope, &void);
        let size = builtin(&vm, thread.heap(), &scope, TypeKind::U64);
        let result = builtin(&vm, thread.heap(), &scope, TypeKind::I32);
        let memcmp = function_pointer(
            &vm,
            thread.heap(),
            &scope,
            &library,
            "memcmp",
            &[byte_pointer.clone(), byte_pointer, size],
            &result,
        );

        let left = thread
            .heap()
            .allocate_handle::<FixedByteArray>(b"abc", &scope);
        let same = thread
            .heap()
            .allocate_handle::<FixedByteArray>(b"abc", &scope);
        let other = thread
            .heap()
            .allocate_handle::<FixedByteArray>(b"abd", &scope);
        let count = scope.handle(Smi::new(3).into_tagged());

        let left = scope.handle(left.as_tagged(thread.heap()).erase());
        let same = scope.handle(same.as_tagged(thread.heap()).erase());
        let other = scope.handle(other.as_tagged(thread.heap()).erase());

        let value = call_pointer(
            &vm,
            thread.heap(),
            &scope,
            &memcmp,
            &[left.clone(), same, count.clone()],
        );
        assert_eq!(value.as_tagged(thread.heap()).to_i64(), Some(0));

        let value = call_pointer(&vm, thread.heap(), &scope, &memcmp, &[left, other, count]);
        assert!(value.as_tagged(thread.heap()).to_i64().unwrap() != 0);
    });
}
