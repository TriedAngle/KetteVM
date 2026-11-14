use mark_sweep::{MarkSweep, MarkSweepConfig};
use vm::ffi::{TypeId, TypeKind, ValueRead};
use vm::{
    CFunction, DefaultInterpreter, Float, ForeignLibrary, Handle, HandleScope, Heap, Smi, TypeInfo,
    VM, VmCore,
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

fn struct_type<'s>(
    vm: &VM,
    heap: &mut Heap,
    scope: &'s HandleScope<'_>,
    fields: &[(&str, Handle<TypeInfo>)],
) -> Handle<'s, TypeInfo> {
    let fields: Vec<(Option<&str>, Handle<TypeInfo>)> = fields
        .iter()
        .map(|(name, ty)| (Some(*name), ty.clone()))
        .collect();
    vm.runtime::<VmCore>()
        .ffi
        .define_struct(vm, heap, scope, None, &fields)
        .expect("struct")
}

fn function_pointer<'s>(
    vm: &VM,
    heap: &mut Heap,
    scope: &'s HandleScope<'_>,
    library: &Handle<ForeignLibrary>,
    name: &str,
    params: &[Handle<TypeInfo>],
    result: &Handle<TypeInfo>,
) -> Handle<'s, CFunction> {
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
        let u8_ty = builtin(&vm, thread.heap(), &scope, TypeKind::U8);
        let color = struct_type(
            &vm,
            thread.heap(),
            &scope,
            &[
                ("r", u8_ty.clone()),
                ("g", u8_ty.clone()),
                ("b", u8_ty.clone()),
                ("a", u8_ty),
            ],
        );
        let i32_ty = builtin(&vm, thread.heap(), &scope, TypeKind::I32);
        let function = function_pointer(
            &vm,
            thread.heap(),
            &scope,
            &library,
            "ColorToInt",
            &[color.clone()],
            &i32_ty,
        );
        let buffer = ffi.value(thread.heap(), &scope, &color).unwrap();
        for (name, value) in [("r", 255), ("g", 0), ("b", 0), ("a", 128)] {
            let heap = thread.heap();
            ffi.value_set_field(
                heap,
                buffer.as_tagged(heap),
                name,
                Smi::new(value).into_tagged(),
            )
            .unwrap();
        }

        let callee = scope.handle(function.as_tagged(thread.heap()).erase());
        let args = scope.stage(&[buffer.as_tagged(thread.heap()).erase()]);
        let raw = ffi
            .call_raw(thread.heap(), &scope, &callee, args.as_args())
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
        let f32_ty = builtin(&vm, thread.heap(), &scope, TypeKind::F32);
        let vector2 = struct_type(
            &vm,
            thread.heap(),
            &scope,
            &[("x", f32_ty.clone()), ("y", f32_ty)],
        );
        let function = function_pointer(
            &vm,
            thread.heap(),
            &scope,
            &library,
            "Vector2Add",
            &[vector2.clone(), vector2.clone()],
            &vector2,
        );
        let left = ffi.value(thread.heap(), &scope, &vector2).unwrap();
        let right = ffi.value(thread.heap(), &scope, &vector2).unwrap();
        for (buffer, values) in [(&left, (1, 2)), (&right, (3, 4))] {
            let heap = thread.heap();
            ffi.value_set_field(
                heap,
                buffer.as_tagged(heap),
                "x",
                Smi::new(values.0).into_tagged(),
            )
            .unwrap();
            ffi.value_set_field(
                heap,
                buffer.as_tagged(heap),
                "y",
                Smi::new(values.1).into_tagged(),
            )
            .unwrap();
        }

        let callee = scope.handle(function.as_tagged(thread.heap()).erase());
        let args = {
            let heap = thread.heap();
            scope.stage(&[left.as_tagged(heap).erase(), right.as_tagged(heap).erase()])
        };
        let raw = ffi
            .call_raw(thread.heap(), &scope, &callee, args.as_args())
            .expect("call");
        let result = scope.handle(ffi.read_raw(thread.heap(), &scope, &raw, None));
        let result = {
            let heap = thread.heap();
            result.as_tagged(heap).get_as::<vm::CValue>(heap).unwrap()
        };
        let result = scope.handle(result);
        let x = {
            let read = ffi
                .value_get_field(thread.heap(), &scope, result.clone(), "x")
                .expect("x");
            match read {
                ValueRead::View(view) => scope.handle(view.as_tagged(thread.heap()).erase()),
                ValueRead::Raw(raw) => {
                    scope.handle(ffi.read_raw(thread.heap(), &scope, &raw, None))
                }
            }
        };
        let y = {
            let read = ffi
                .value_get_field(thread.heap(), &scope, result.clone(), "y")
                .expect("y");
            match read {
                ValueRead::View(view) => scope.handle(view.as_tagged(thread.heap()).erase()),
                ValueRead::Raw(raw) => {
                    scope.handle(ffi.read_raw(thread.heap(), &scope, &raw, None))
                }
            }
        };
        let heap = thread.heap();
        let x = x.as_tagged(heap).get_as::<Float>(heap).unwrap().value.get();
        let y = y.as_tagged(heap).get_as::<Float>(heap).unwrap().value.get();
        assert_eq!((x, y), (4.0, 6.0));
    });
}

#[test]
fn collected_types_drop_out_of_the_registry() {
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
    });
}
