use mark_sweep::{MarkSweep, MarkSweepConfig};
use vm::ffi::{Abi, TypeId};
use vm::{CPointer, DefaultInterpreter, Handle, HandleScope, Heap, VM, VmCore};

fn test_vm() -> VM {
    VM::new::<MarkSweep, DefaultInterpreter>(MarkSweepConfig::default())
        .expect("failed to create heap")
        .add::<VmCore>()
        .expect("failed to install the VM core")
}

fn function_pointer<'s>(
    vm: &VM,
    heap: &mut Heap,
    scope: &'s HandleScope<'_>,
    library: &Handle<vm::ForeignLibrary>,
    name: &str,
    params: Vec<TypeId>,
    result: TypeId,
) -> Handle<'s, vm::CFunction> {
    let core = vm.runtime::<VmCore>();
    let ffi = &core.ffi;
    let (signature, target_type) = {
        let mut types = ffi.types_mut();
        let signature = types.function(params, result, false, Abi::C).unwrap();
        let target_type = types.pointer_to(signature);
        (signature, target_type)
    };
    let target = ffi
        .library_symbol(heap, scope, library, name, target_type)
        .expect("symbol");
    ffi.function(heap, scope, core.ffi_call, signature, &target, library)
        .expect("function")
}

fn char_pointer(vm: &VM) -> TypeId {
    vm.runtime::<VmCore>()
        .ffi
        .types_mut()
        .pointer_to(TypeId::CHAR)
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
        let strlen = function_pointer(
            &vm,
            thread.heap(),
            &scope,
            &library,
            "strlen",
            vec![char_pointer(&vm)],
            TypeId::SIZE,
        );

        let text = cstring(&vm, thread.heap(), &scope, "hello");
        let callee = scope.handle(strlen.as_tagged(thread.heap()).erase());
        let args = scope.stage(&[text.as_tagged(thread.heap()).erase()]);
        let raw = ffi
            .call_raw(thread.heap(), &callee, args.as_args())
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
        let text_length = function_pointer(
            &vm,
            thread.heap(),
            &scope,
            &library,
            "TextLength",
            vec![char_pointer(&vm)],
            TypeId::U32,
        );
        let text = cstring(&vm, thread.heap(), &scope, "hello raylib");
        let callee = scope.handle(text_length.as_tagged(thread.heap()).erase());
        let args = scope.stage(&[text.as_tagged(thread.heap()).erase()]);
        let raw = ffi
            .call_raw(thread.heap(), &callee, args.as_args())
            .expect("call");
        let value = ffi.read_raw(thread.heap(), &scope, &raw, None);
        assert_eq!(value.to_i64(), Some(12));
    });
}
