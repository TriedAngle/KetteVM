use mark_sweep::{MarkSweep, MarkSweepConfig};
use vm::ffi::{Abi, FfiError, TypeId};
use vm::{
    CPointer, CPointerFlags, DefaultInterpreter, ForeignLibrary, Handle, HandleScope, Heap, VM,
    VmCore,
};

fn test_vm() -> VM {
    VM::new::<MarkSweep, DefaultInterpreter>(MarkSweepConfig::default())
        .expect("failed to create heap")
        .add::<VmCore>()
        .expect("failed to install the VM core")
}

/// The pointer type used for a library function symbol.
fn function_pointer(vm: &VM) -> TypeId {
    let ffi = &vm.runtime::<VmCore>().ffi;
    let mut types = ffi.types_mut();
    let function = types.function(vec![], TypeId::I32, false, Abi::C).unwrap();
    types.pointer_to(function)
}

fn open<'s>(
    vm: &VM,
    heap: &mut Heap,
    scope: &'s HandleScope<'_>,
    path: Option<&str>,
) -> Handle<'s, ForeignLibrary> {
    vm.runtime::<VmCore>()
        .ffi
        .open_library(vm, heap, scope, path)
        .expect("open library")
}

fn symbol<'s>(
    vm: &VM,
    heap: &mut Heap,
    scope: &'s HandleScope<'_>,
    library: &Handle<ForeignLibrary>,
    name: &str,
) -> Handle<'s, CPointer> {
    let ty = function_pointer(vm);
    vm.runtime::<VmCore>()
        .ffi
        .library_symbol(heap, scope, library, name, ty)
        .expect("library symbol")
}

#[test]
fn open_process_lookup_and_close() {
    let vm = test_vm();
    let mut thread = vm.attach();
    thread.handle_scope(|thread, scope| {
        let ffi = &vm.runtime::<VmCore>().ffi;
        let library = open(&vm, thread.heap(), &scope, None);
        let symbol = symbol(&vm, thread.heap(), &scope, &library, "malloc");

        let heap = thread.heap();
        let p = symbol.as_tagged(heap);
        assert!(p.check_live(heap).is_ok());
        assert!(!p.is_null());
        assert!(p.flags().contains(CPointerFlags::LIBRARY));
        assert!(p.flags().contains(CPointerFlags::BORROWED));
        assert!(p.library(heap).is_some());
        assert_eq!(ffi.library_path(heap, &library), Some(String::new()));

        // symbol addresses are owned by the library, never by the VM
        assert_eq!(p.free_external(heap), Err(FfiError::NotOwned));

        ffi.close_library(heap, &library).unwrap();
        assert!(!library.as_tagged(heap).is_live(heap));
        assert_eq!(p.check_live(heap), Err(FfiError::LibraryClosed));
        // the pointer's own bits are untouched; only the library is dead
        assert!(!p.is_dead());
        assert_eq!(p.read_u32(heap), Err(FfiError::LibraryClosed));

        // idempotent
        ffi.close_library(heap, &library).unwrap();
    });
}

#[test]
fn missing_symbol_is_reported() {
    let vm = test_vm();
    let mut thread = vm.attach();
    thread.handle_scope(|thread, scope| {
        let ffi = &vm.runtime::<VmCore>().ffi;
        let library = open(&vm, thread.heap(), &scope, None);
        let ty = function_pointer(&vm);
        let result = ffi.library_symbol(
            thread.heap(),
            &scope,
            &library,
            "kettevm_no_such_symbol",
            ty,
        );
        assert!(matches!(result, Err(FfiError::SymbolNotFound { .. })));
    });
}

#[test]
fn bad_path_is_reported() {
    let vm = test_vm();
    let mut thread = vm.attach();
    thread.handle_scope(|thread, scope| {
        let ffi = &vm.runtime::<VmCore>().ffi;
        let result = ffi.open_library(
            &vm,
            thread.heap(),
            &scope,
            Some("/nonexistent/kettevm-no-such-library.dylib"),
        );
        assert!(matches!(result, Err(FfiError::LoadFailed(_))));
    });
}

#[test]
fn symbol_requires_a_pointer_type() {
    let vm = test_vm();
    let mut thread = vm.attach();
    thread.handle_scope(|thread, scope| {
        let ffi = &vm.runtime::<VmCore>().ffi;
        let library = open(&vm, thread.heap(), &scope, None);
        let result = ffi.library_symbol(thread.heap(), &scope, &library, "malloc", TypeId::I32);
        assert!(matches!(result, Err(FfiError::NotAPointer(TypeId::I32))));
    });
}

#[test]
fn symbol_holds_a_typed_pointer_to_its_library() {
    let vm = test_vm();
    let mut thread = vm.attach();
    thread.handle_scope(|thread, scope| {
        let library = open(&vm, thread.heap(), &scope, None);
        let symbol = symbol(&vm, thread.heap(), &scope, &library, "malloc");
        let heap = thread.heap();
        let p = symbol.as_tagged(heap);
        let owned = p.library(heap).expect("symbol references its library");
        assert!(owned.is_live(heap));
        assert_eq!(owned.path(heap).to_rust_string(heap), "");
    });
}

#[test]
fn raylib_symbol_when_available() {
    let path = "/opt/homebrew/lib/libraylib.dylib";
    if !std::path::Path::new(path).exists() {
        return;
    }
    let vm = test_vm();
    let mut thread = vm.attach();
    thread.handle_scope(|thread, scope| {
        let ffi = &vm.runtime::<VmCore>().ffi;
        let library = open(&vm, thread.heap(), &scope, Some(path));
        let symbol = symbol(&vm, thread.heap(), &scope, &library, "GetScreenWidth");
        let heap = thread.heap();
        assert!(!symbol.as_tagged(heap).is_null());
        assert_eq!(ffi.library_path(heap, &library), Some(path.to_string()));
        ffi.close_library(heap, &library).unwrap();
        assert_eq!(
            symbol.as_tagged(heap).check_live(heap),
            Err(FfiError::LibraryClosed)
        );
    });
}
