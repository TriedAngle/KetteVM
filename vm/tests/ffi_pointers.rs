use mark_sweep::{MarkSweep, MarkSweepConfig};
use vm::ffi::{FfiError, TypeId};
use vm::{
    CPointer, CPointerFlags, CPointerInit, DefaultInterpreter, Handle, HandleScope, Heap, HeapPtr,
    RawCell, Smi, VM, Visitor, VmCore, visit_object,
};

fn test_vm() -> VM {
    VM::new::<MarkSweep, DefaultInterpreter>(MarkSweepConfig::default())
        .expect("failed to create heap")
        .add::<VmCore>()
        .expect("failed to install the VM core")
}

fn allocate<'s>(
    heap: &mut Heap,
    scope: &'s HandleScope<'_>,
    vm: &VM,
    ty: TypeId,
) -> Handle<'s, CPointer> {
    vm.runtime::<VmCore>()
        .ffi
        .allocate(heap, scope, ty)
        .expect("ffi allocation")
}

/// A pointer with a caller-chosen address, for null/dead/unaligned cases.
fn raw_pointer<'s>(
    heap: &mut Heap,
    scope: &'s HandleScope<'_>,
    ty: TypeId,
    address: usize,
    flags: CPointerFlags,
) -> Handle<'s, CPointer> {
    heap.allocate_handle::<CPointer>(
        CPointerInit {
            type_id: ty,
            address,
            flags,
            library: None,
        },
        scope,
    )
}

#[test]
fn allocation_is_zeroed_and_owned() {
    let vm = test_vm();
    let mut thread = vm.attach();
    thread.handle_scope(|thread, scope| {
        let pointer = allocate(thread.heap(), &scope, &vm, TypeId::U64);
        let heap = thread.heap();
        let p = pointer.as_tagged(heap);
        assert_eq!(p.c_type(), TypeId::U64);
        assert!(p.is_live());
        assert!(!p.is_null());
        assert!(p.flags().contains(CPointerFlags::OWNED_EXTERNAL));
        assert_eq!(p.read_u64(heap), Ok(0));
    });
}

#[test]
fn scalar_roundtrip_at_each_width() {
    let vm = test_vm();
    let mut thread = vm.attach();
    thread.handle_scope(|thread, scope| {
        let u8_ptr = allocate(thread.heap(), &scope, &vm, TypeId::U8);
        let i16_ptr = allocate(thread.heap(), &scope, &vm, TypeId::I16);
        let u32_ptr = allocate(thread.heap(), &scope, &vm, TypeId::U32);
        let i64_ptr = allocate(thread.heap(), &scope, &vm, TypeId::I64);
        let f32_ptr = allocate(thread.heap(), &scope, &vm, TypeId::F32);
        let f64_ptr = allocate(thread.heap(), &scope, &vm, TypeId::F64);

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
        let bytes = vm
            .runtime::<VmCore>()
            .ffi
            .types_mut()
            .array_of(TypeId::U8, 32)
            .unwrap();
        let base = allocate(thread.heap(), &scope, &vm, bytes);
        let address = base.as_tagged(thread.heap()).address();
        let unaligned = raw_pointer(
            thread.heap(),
            &scope,
            TypeId::U64,
            address + 1,
            CPointerFlags::BORROWED,
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
fn null_and_dead_are_distinct_errors() {
    let vm = test_vm();
    let mut thread = vm.attach();
    thread.handle_scope(|thread, scope| {
        let null = raw_pointer(
            thread.heap(),
            &scope,
            TypeId::U64,
            0,
            CPointerFlags::BORROWED,
        );
        let dead = raw_pointer(
            thread.heap(),
            &scope,
            TypeId::U64,
            0x1000,
            CPointerFlags::BORROWED.union(CPointerFlags::DEAD),
        );

        let heap = thread.heap();
        let p = null.as_tagged(heap);
        assert!(p.is_null());
        assert!(p.is_live());
        assert_eq!(p.read_u64(heap), Err(FfiError::NullPointer));

        let p = dead.as_tagged(heap);
        assert!(p.is_dead());
        assert!(!p.is_null());
        assert_eq!(p.read_u64(heap), Err(FfiError::DeadPointer));
    });
}

#[test]
fn free_is_idempotent_and_rejects_borrowed() {
    let vm = test_vm();
    let mut thread = vm.attach();
    thread.handle_scope(|thread, scope| {
        let pointer = allocate(thread.heap(), &scope, &vm, TypeId::U32);
        let borrowed = raw_pointer(
            thread.heap(),
            &scope,
            TypeId::U32,
            0x1000,
            CPointerFlags::BORROWED,
        );

        let heap = thread.heap();
        let p = pointer.as_tagged(heap);
        p.free_external(heap).unwrap();
        assert!(p.is_dead());
        // second free is a no-op
        p.free_external(heap).unwrap();

        let b = borrowed.as_tagged(heap);
        assert_eq!(b.free_external(heap), Err(FfiError::NotOwned));
    });
}

struct VisitedCells(Vec<usize>);

impl Visitor for VisitedCells {
    fn visit(&mut self, cell: &RawCell) {
        self.0.push(cell as *const RawCell as usize);
    }
}

#[test]
fn payload_is_never_visited_as_a_gc_edge() {
    let vm = test_vm();
    let mut thread = vm.attach();
    thread.handle_scope(|thread, scope| {
        let pointer = allocate(thread.heap(), &scope, &vm, TypeId::U64);
        let heap = thread.heap();
        let raw = pointer.as_tagged(heap).as_ref();
        let address_cell = &raw.address as *const RawCell as usize;
        let object = HeapPtr::from_ref(raw).cast::<()>();

        let mut visited = VisitedCells(Vec::new());
        // Safety: `object` is a live heap object of this VM.
        unsafe { visit_object(object, &mut visited) };

        assert_eq!(visited.0.len(), 4, "map, slots, elements, library");
        assert!(!visited.0.contains(&address_cell));
    });
}

#[test]
fn array_elements_and_pointer_arithmetic() {
    let vm = test_vm();
    let mut thread = vm.attach();
    thread.handle_scope(|thread, scope| {
        let ffi = &vm.runtime::<VmCore>().ffi;
        let array = ffi.types_mut().array_of(TypeId::I32, 4).unwrap();
        let buffer = ffi.buffer(thread.heap(), &scope, array).unwrap();
        for index in 0..4 {
            ffi.buffer_set_index(
                thread.heap(),
                &buffer,
                index,
                Smi::new((index as i64) * 10).into_tagged(),
            )
            .unwrap();
        }

        let raw = ffi.buffer_get_index_raw(thread.heap(), &buffer, 2).unwrap();
        let value = ffi.read_raw(thread.heap(), &scope, &raw, None);
        assert_eq!(value.to_i64(), Some(20));
        assert_eq!(
            ffi.buffer_get_index_raw(thread.heap(), &buffer, 4).err(),
            Some(FfiError::IndexOutOfBounds)
        );

        let storage = {
            let heap = thread.heap();
            scope.handle(buffer.as_tagged(heap).storage(heap))
        };
        let element_pointer = ffi.types_mut().pointer_to(TypeId::I32);
        let storage = ffi
            .cast_pointer(thread.heap(), &scope, &storage, element_pointer)
            .unwrap();
        let second = ffi
            .pointer_offset(thread.heap(), &scope, &storage, 1)
            .unwrap();
        let third = ffi
            .pointer_offset(thread.heap(), &scope, &storage, 2)
            .unwrap();

        let heap = thread.heap();
        assert_eq!(second.as_tagged(heap).read_i32(heap), Ok(10));
        assert_eq!(third.as_tagged(heap).read_i32(heap), Ok(20));
    });
}

#[test]
fn address_survives_collection() {
    let vm = test_vm();
    vm.arm_gc_stress();
    let mut thread = vm.attach();
    thread.handle_scope(|thread, scope| {
        let pointer = allocate(thread.heap(), &scope, &vm, TypeId::U64);
        let address = pointer.as_tagged(thread.heap()).address();
        {
            let heap = thread.heap();
            pointer
                .as_tagged(heap)
                .write_u64(heap, 0x0102_0304_0506_0708)
                .unwrap();
        }

        // churn: each allocation collects under gc stress
        for _ in 0..8 {
            let _ = allocate(thread.heap(), &scope, &vm, TypeId::U64);
        }
        thread.heap().collect();

        let heap = thread.heap();
        let p = pointer.as_tagged(heap);
        assert_eq!(p.address(), address);
        assert_eq!(p.read_u64(heap), Ok(0x0102_0304_0506_0708));
    });
}
