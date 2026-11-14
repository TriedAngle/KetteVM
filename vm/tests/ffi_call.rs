use mark_sweep::{MarkSweep, MarkSweepConfig};
use vm::ffi::TypeKind;
use vm::{CPointer, DefaultInterpreter, Handle, HandleScope, Heap, TypeInfo, VM, VmCore};

fn test_vm() -> VM {
    VM::new::<MarkSweep, DefaultInterpreter>(MarkSweepConfig::default())
        .expect("failed to create heap")
        .add::<VmCore>()
        .expect("failed to install the VM core")
}

fn builtin<'s>(
    vm: &VM,
    heap: &mut Heap,
    scope: &'s HandleScope<'_>,
    kind: TypeKind,
) -> Handle<'s, TypeInfo> {
    scope.handle(vm.runtime::<VmCore>().ffi.builtin(heap, kind))
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
) -> Handle<'s, CPointer> {
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
