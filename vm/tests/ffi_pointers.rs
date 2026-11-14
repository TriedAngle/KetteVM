use mark_sweep::{MarkSweep, MarkSweepConfig};
use vm::ffi::{FfiError, TypeId, TypeKind, ValueRead};
use vm::{
    CPointer, CPointerFlags, CPointerInit, DefaultInterpreter, Handle, HandleScope, Heap, HeapPtr,
    RawCell, Smi, TypeInfo, VM, Visitor, VmCore, visit_object,
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

fn raw_pointer<'s>(
    heap: &mut Heap,
    scope: &'s HandleScope<'_>,
    ty: &Handle<TypeInfo>,
    address: usize,
    flags: CPointerFlags,
) -> Handle<'s, CPointer> {
    heap.allocate_handle::<CPointer>(
        CPointerInit {
            pointee: ty.clone(),
            address: address as *mut u8,
            flags,
            library: None,
        },
        scope,
    )
}

#[test]
fn external_scalar_roundtrip() {
    let vm = test_vm();
    let mut thread = vm.attach();
    thread.handle_scope(|thread, scope| {
        let mut storage = Box::new([0u8; 64]);
        let base = storage.as_mut_ptr() as usize;
        let u8_ty = builtin(&vm, thread.heap(), &scope, TypeKind::U8);
        let i16_ty = builtin(&vm, thread.heap(), &scope, TypeKind::I16);
        let u32_ty = builtin(&vm, thread.heap(), &scope, TypeKind::U32);
        let i64_ty = builtin(&vm, thread.heap(), &scope, TypeKind::I64);
        let f32_ty = builtin(&vm, thread.heap(), &scope, TypeKind::F32);
        let f64_ty = builtin(&vm, thread.heap(), &scope, TypeKind::F64);

        let u8_ptr = raw_pointer(thread.heap(), &scope, &u8_ty, base, CPointerFlags::NONE);
        let i16_ptr = raw_pointer(
            thread.heap(),
            &scope,
            &i16_ty,
            base + 8,
            CPointerFlags::NONE,
        );
        let u32_ptr = raw_pointer(
            thread.heap(),
            &scope,
            &u32_ty,
            base + 16,
            CPointerFlags::NONE,
        );
        let i64_ptr = raw_pointer(
            thread.heap(),
            &scope,
            &i64_ty,
            base + 24,
            CPointerFlags::NONE,
        );
        let f32_ptr = raw_pointer(
            thread.heap(),
            &scope,
            &f32_ty,
            base + 32,
            CPointerFlags::NONE,
        );
        let f64_ptr = raw_pointer(
            thread.heap(),
            &scope,
            &f64_ty,
            base + 40,
            CPointerFlags::NONE,
        );

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
    });
}

#[test]
fn unaligned_access_works() {
    let vm = test_vm();
    let mut thread = vm.attach();
    thread.handle_scope(|thread, scope| {
        let u64_ty = builtin(&vm, thread.heap(), &scope, TypeKind::U64);
        let mut storage = Box::new([0u8; 32]);
        let pointer = raw_pointer(
            thread.heap(),
            &scope,
            &u64_ty,
            storage.as_mut_ptr() as usize + 1,
            CPointerFlags::NONE,
        );

        let heap = thread.heap();
        let p = pointer.as_tagged(heap);
        p.write_u64(heap, 0x0102_0304_0506_0708).unwrap();
        assert_eq!(p.read_u64(heap), Ok(0x0102_0304_0506_0708));

        let mut out = [0u8; 8];
        p.read_bytes(heap, &mut out).unwrap();
        assert_eq!(out, [8, 7, 6, 5, 4, 3, 2, 1]);
    });
}

#[test]
fn null_and_dead_are_distinct_errors() {
    let vm = test_vm();
    let mut thread = vm.attach();
    thread.handle_scope(|thread, scope| {
        let u64_ty = builtin(&vm, thread.heap(), &scope, TypeKind::U64);
        let null = raw_pointer(thread.heap(), &scope, &u64_ty, 0, CPointerFlags::NONE);
        let dead = raw_pointer(thread.heap(), &scope, &u64_ty, 0x1000, CPointerFlags::DEAD);

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

struct VisitedCells(Vec<usize>);

impl Visitor for VisitedCells {
    fn visit(&mut self, cell: &RawCell) {
        self.0.push(cell as *const RawCell as usize);
    }
}

#[test]
fn address_is_visited_as_a_gc_edge() {
    let vm = test_vm();
    let mut thread = vm.attach();
    thread.handle_scope(|thread, scope| {
        let u64_ty = builtin(&vm, thread.heap(), &scope, TypeKind::U64);
        let pointer = raw_pointer(thread.heap(), &scope, &u64_ty, 0x1000, CPointerFlags::NONE);
        let heap = thread.heap();
        let raw = pointer.as_tagged(heap).as_ref();
        let address_cell = raw.address.as_raw() as *const RawCell as usize;
        let object = HeapPtr::from_ref(raw).cast::<()>();

        let mut visited = VisitedCells(Vec::new());
        // Safety: `object` is a live heap object of this VM.
        unsafe { visit_object(object, &mut visited) };

        assert_eq!(
            visited.0.len(),
            6,
            "map, slots, elements, type, address, library"
        );
        assert!(visited.0.contains(&address_cell));
    });
}

#[test]
fn array_elements_and_views() {
    let vm = test_vm();
    let mut thread = vm.attach();
    thread.handle_scope(|thread, scope| {
        let ffi = &vm.runtime::<VmCore>().ffi;
        let i32_ty = builtin(&vm, thread.heap(), &scope, TypeKind::I32);
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

        let read = ffi
            .value_get_index(thread.heap(), &scope, buffer.clone(), 2)
            .unwrap();
        let value = match read {
            ValueRead::View(view) => view.as_tagged(thread.heap()).erase(),
            ValueRead::Raw(raw) => ffi.read_raw(thread.heap(), &scope, &raw, None),
        };
        assert_eq!(value.to_i64(), Some(20));
        assert_eq!(
            ffi.value_get_index(thread.heap(), &scope, buffer.clone(), 4)
                .err(),
            Some(FfiError::IndexOutOfBounds)
        );

        let second = ffi
            .value_add(thread.heap(), &scope, buffer.clone(), 1)
            .unwrap();
        let third = ffi
            .value_add(thread.heap(), &scope, buffer.clone(), 2)
            .unwrap();
        let read = ffi
            .value_get_index(thread.heap(), &scope, second, 0)
            .unwrap();
        let second_value = match read {
            ValueRead::View(view) => view.as_tagged(thread.heap()).erase(),
            ValueRead::Raw(raw) => ffi.read_raw(thread.heap(), &scope, &raw, None),
        };
        assert_eq!(second_value.to_i64(), Some(10));
        let read = ffi
            .value_get_index(thread.heap(), &scope, third, 0)
            .unwrap();
        let third_value = match read {
            ValueRead::View(view) => view.as_tagged(thread.heap()).erase(),
            ValueRead::Raw(raw) => ffi.read_raw(thread.heap(), &scope, &raw, None),
        };
        assert_eq!(third_value.to_i64(), Some(20));
    });
}

#[test]
fn vm_backed_value_survives_collection() {
    let vm = test_vm();
    vm.arm_gc_stress();
    let mut thread = vm.attach();
    thread.handle_scope(|thread, scope| {
        let ffi = &vm.runtime::<VmCore>().ffi;
        let u64_ty = builtin(&vm, thread.heap(), &scope, TypeKind::U64);
        let array = ffi
            .types()
            .array_of(thread.heap(), &scope, &u64_ty, 1)
            .unwrap();
        let buffer = ffi.value(thread.heap(), &scope, &array).unwrap();
        {
            let heap = thread.heap();
            ffi.value_set_index(heap, buffer.as_tagged(heap), 0, Smi::new(42).into_tagged())
                .unwrap();
        }

        for _ in 0..8 {
            let _ = ffi.value(thread.heap(), &scope, &array).unwrap();
        }
        thread.heap().collect();

        let read = ffi
            .value_get_index(thread.heap(), &scope, buffer, 0)
            .unwrap();
        let value = match read {
            ValueRead::View(view) => view.as_tagged(thread.heap()).erase(),
            ValueRead::Raw(raw) => ffi.read_raw(thread.heap(), &scope, &raw, None),
        };
        assert_eq!(value.to_i64(), Some(42));
    });
}
