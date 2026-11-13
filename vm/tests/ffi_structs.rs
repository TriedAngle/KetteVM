use mark_sweep::{MarkSweep, MarkSweepConfig};
use vm::ffi::{Abi, TypeId};
use vm::{
    CFunction, DefaultInterpreter, Float, ForeignLibrary, Handle, HandleScope, Heap, Smi, VM,
    VmCore,
};

fn test_vm() -> VM {
    VM::new::<MarkSweep, DefaultInterpreter>(MarkSweepConfig::default())
        .expect("failed to create heap")
        .add::<VmCore>()
        .expect("failed to install the VM core")
}

fn struct_type(vm: &VM, fields: &[(&str, TypeId)]) -> TypeId {
    let fields = fields
        .iter()
        .map(|(name, ty)| (Some(name.to_string()), *ty))
        .collect();
    vm.runtime::<VmCore>()
        .ffi
        .types_mut()
        .define_struct(None, fields)
        .expect("struct")
}

fn function_pointer<'s>(
    vm: &VM,
    heap: &mut Heap,
    scope: &'s HandleScope<'_>,
    library: &Handle<ForeignLibrary>,
    name: &str,
    params: Vec<TypeId>,
    result: TypeId,
) -> Handle<'s, CFunction> {
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

fn raylib() -> Option<&'static str> {
    let path = "/opt/homebrew/lib/libraylib.dylib";
    std::path::Path::new(path).exists().then_some(path)
}

#[test]
fn struct_by_value_argument() {
    let Some(path) = raylib() else {
        return;
    };
    let vm = test_vm();
    let mut thread = vm.attach();
    thread.handle_scope(|thread, scope| {
        let ffi = &vm.runtime::<VmCore>().ffi;
        let library = ffi
            .open_library(&vm, thread.heap(), &scope, Some(path))
            .expect("open");
        let color = struct_type(
            &vm,
            &[
                ("r", TypeId::U8),
                ("g", TypeId::U8),
                ("b", TypeId::U8),
                ("a", TypeId::U8),
            ],
        );
        let function = function_pointer(
            &vm,
            thread.heap(),
            &scope,
            &library,
            "ColorToInt",
            vec![color],
            TypeId::I32,
        );
        let buffer = ffi.buffer(thread.heap(), &scope, color).unwrap();
        for (name, value) in [("r", 255), ("g", 0), ("b", 0), ("a", 128)] {
            ffi.buffer_set(thread.heap(), &buffer, name, Smi::new(value).into_tagged())
                .unwrap();
        }

        let callee = scope.handle(function.as_tagged(thread.heap()).erase());
        let args = scope.stage(&[buffer.as_tagged(thread.heap()).erase()]);
        let raw = ffi
            .call_raw(thread.heap(), &callee, args.as_args())
            .expect("call");
        let value = ffi.read_raw(thread.heap(), &scope, &raw, None);
        assert_eq!(value.to_i64(), Some(0xFF00_0080u32 as i32 as i64));
    });
}

#[test]
fn hfa_struct_arguments_and_return() {
    let Some(path) = raylib() else {
        return;
    };
    let vm = test_vm();
    let mut thread = vm.attach();
    thread.handle_scope(|thread, scope| {
        let ffi = &vm.runtime::<VmCore>().ffi;
        let library = ffi
            .open_library(&vm, thread.heap(), &scope, Some(path))
            .expect("open");
        let vector2 = struct_type(&vm, &[("x", TypeId::F32), ("y", TypeId::F32)]);
        let function = function_pointer(
            &vm,
            thread.heap(),
            &scope,
            &library,
            "Vector2Add",
            vec![vector2, vector2],
            vector2,
        );
        let left = ffi.buffer(thread.heap(), &scope, vector2).unwrap();
        let right = ffi.buffer(thread.heap(), &scope, vector2).unwrap();
        for (buffer, values) in [(&left, (1, 2)), (&right, (3, 4))] {
            ffi.buffer_set(thread.heap(), buffer, "x", Smi::new(values.0).into_tagged())
                .unwrap();
            ffi.buffer_set(thread.heap(), buffer, "y", Smi::new(values.1).into_tagged())
                .unwrap();
        }

        let callee = scope.handle(function.as_tagged(thread.heap()).erase());
        let args = {
            let heap = thread.heap();
            scope.stage(&[left.as_tagged(heap).erase(), right.as_tagged(heap).erase()])
        };
        let raw = ffi
            .call_raw(thread.heap(), &callee, args.as_args())
            .expect("call");
        let result = scope.handle(ffi.read_raw(thread.heap(), &scope, &raw, None));
        let result = {
            let heap = thread.heap();
            result.as_tagged(heap).get_as::<vm::CBuffer>(heap).unwrap()
        };
        let result = scope.handle(result);
        let x = {
            let raw = ffi.buffer_get_raw(thread.heap(), &result, "x").expect("x");
            scope.handle(ffi.read_raw(thread.heap(), &scope, &raw, None))
        };
        let y = {
            let raw = ffi.buffer_get_raw(thread.heap(), &result, "y").expect("y");
            scope.handle(ffi.read_raw(thread.heap(), &scope, &raw, None))
        };
        let heap = thread.heap();
        let x = x.as_tagged(heap).get_as::<Float>(heap).unwrap().value.get();
        let y = y.as_tagged(heap).get_as::<Float>(heap).unwrap().value.get();
        assert_eq!((x, y), (4.0, 6.0));
    });
}
