use mark_sweep::{MarkSweep, MarkSweepConfig};
use vm::ffi::{TypeId, TypeKind};
use vm::{
    AccessorInfo, AccessorInfoInit, AccessorPair, AtomicI64Object, AtomicI64ObjectInit, CValue,
    CValueInit, CallableInfoInit, CallableInfoObject, Cell, CellInit, Code, CodeInit, ContextInit,
    ContextObject, DataHandler, DataHandlerInit, DefaultInterpreter, DenseString, Encoding,
    ExternalString, ExternalStringInit, FeedbackVector, FeedbackVectorInit, FixedArray,
    FixedByteArray, Float, ForeignLibrary, ForeignLibraryInit, Handle, HandleSlice,
    HandlerEntryInit, HandlerTable, HandlerTableInit, HeapPtr, Map, MapInit, MapKind, MaybeWeak,
    NameToIndexHashTable, NameToIndexHashTableInit, NativeIndex, NumberDictionary,
    NumberDictionaryInit, PrototypeInfo, ProxyInit, ProxyObject, RawCell, RuntimeId, ScopeInfo,
    ScopeInfoInit, ScriptContextTable, ScriptContextTableInit, Smi, Symbol, Tagged, ThreadKind,
    ThreadObject, ThreadObjectInit, ThreadStatus, TypeInfo, TypeInfoInit, Utf8String, VM, Value,
    Visitor, VmCore, WEAK_PTR, WeakFixedArray, WeakFixedArrayInit, object_layout, visit_object,
};

struct Visited(Vec<usize>);

impl Visitor for Visited {
    fn visit(&mut self, cell: &RawCell) {
        self.0.push(cell as *const RawCell as usize);
    }
}

fn weak(value: Handle<'_, Value>) -> Tagged<'static, MaybeWeak<Value>> {
    unsafe {
        Tagged::<MaybeWeak<Value>>::from_maybe_weak_unchecked(Value::from_bits(
            value.raw().to_bits() | WEAK_PTR,
        ))
    }
}

fn strong(value: Handle<'_, Value>) -> Tagged<'static, Value> {
    unsafe { Tagged::from_value_unchecked(value.raw()) }
}

macro_rules! assert_edges {
    ($label:expr, $raw:expr, [$($cell:expr),* $(,)?]) => {{
        let raw = $raw;
        let object = HeapPtr::from_ref(raw).cast::<()>();
        let start = object.addr();
        let size = unsafe { object_layout(object) }.size();
        let mut visitor = Visited(Vec::new());
        unsafe { visit_object(object, &mut visitor) };
        let mut actual = visitor.0;
        actual.sort_unstable();
        let mut expected = vec![$($cell.as_raw() as *const RawCell as usize),*];
        expected.sort_unstable();
        assert_eq!(actual, expected, "edge set: {}", $label);
        assert!(
            actual.iter().all(|cell| *cell >= start && *cell < start + size),
            "edge outside layout: {}",
            $label
        );
    }};
}

fn test_vm() -> VM {
    VM::new::<MarkSweep, DefaultInterpreter>(MarkSweepConfig::default())
        .expect("heap")
        .add::<VmCore>()
        .expect("vm core")
}

#[test]
fn every_object_kind_visits_its_entire_layout() {
    let vm = test_vm();
    let mut thread = vm.attach();
    thread.handle_scope(|thread, scope| {
        let heap = thread.heap();
        let null = heap.known().null.get_handle(heap, &scope).erase();
        let undefined = heap.known().undefined.get_handle(heap, &scope).erase();
        let empty_array = heap.known().empty_fixed_array.get_handle(heap, &scope);
        let object_map = heap.known().object_initial_map.get_handle(heap, &scope);
        let array_map = heap.known().js_array_map.get_handle(heap, &scope);

        let atomic =
            heap.allocate_handle::<AtomicI64Object>(AtomicI64ObjectInit { value: 7 }, &scope);
        assert_edges!(
            "AtomicI64Object",
            atomic.as_tagged(heap).as_ref(),
            [atomic.as_tagged(heap).header.map,]
        );

        let thread_object = heap.allocate_handle::<ThreadObject>(
            ThreadObjectInit {
                kind: ThreadKind::Platform,
                target: undefined,
                name: undefined,
                task: None,
                state: ThreadStatus::New,
                tid: 1,
                platform_slot: -1,
            },
            &scope,
        );
        {
            let raw = thread_object.as_tagged(heap).as_ref();
            assert_edges!(
                "ThreadObject",
                raw,
                [
                    raw.header.map,
                    raw.slots,
                    raw.elements,
                    raw.length,
                    raw.tid,
                    raw.kind,
                    raw.target,
                    raw.name,
                    raw.task,
                    raw.state,
                    raw.interrupted,
                    raw.platform_slot,
                ]
            );
        }

        let object = scope.handle(heap.new_object(&scope, object_map, HandleSlice::EMPTY));
        {
            let raw = object.as_tagged(heap).as_ref();
            assert_edges!("Object", raw, [raw.header.map, raw.slots, raw.elements]);
        }
        let array = scope.handle(heap.new_object(&scope, array_map, HandleSlice::EMPTY));
        {
            let raw = array.as_tagged(heap).as_ref();
            assert_edges!("Array", raw, [raw.header.map, raw.slots, raw.elements]);
        }

        let values = [Smi::new(1).into_tagged(), Smi::new(2).into_tagged()];
        let fixed = heap.allocate_handle::<FixedArray>(scope.stage(&values), &scope);
        {
            let raw = fixed.as_tagged(heap).as_ref();
            assert_edges!(
                "FixedArray",
                raw,
                [raw.header.map, raw.element_slot(0), raw.element_slot(1),]
            );
        }

        let weak_values = [weak(object.erase()), weak(array.erase())];
        let weak_array = heap.allocate_handle::<WeakFixedArray>(
            WeakFixedArrayInit {
                values: &weak_values,
            },
            &scope,
        );
        {
            let raw = weak_array.as_tagged(heap).as_ref();
            assert_edges!(
                "WeakFixedArray",
                raw,
                [raw.header.map, raw.element_slot(0), raw.element_slot(1),]
            );
        }

        let bytes = heap.allocate_handle::<FixedByteArray>(b"bytes", &scope);
        assert_edges!(
            "FixedByteArray",
            bytes.as_tagged(heap).as_ref(),
            [bytes.as_tagged(heap).header.map,]
        );

        let dense = DenseString::from_utf8(heap, &scope, "dense");
        assert_edges!(
            "DenseString",
            dense.as_tagged(heap).as_ref(),
            [dense.as_tagged(heap).header.map,]
        );

        let utf8 = Utf8String::from_str(heap, &scope, "utf8");
        assert_edges!(
            "Utf8String",
            utf8.as_tagged(heap).as_ref(),
            [utf8.as_tagged(heap).header.map,]
        );

        static LATIN1: [u8; 3] = [b'h', b'i', 0];
        let external = heap.allocate_handle::<ExternalString>(
            ExternalStringInit {
                address: LATIN1.as_ptr() as usize,
                len: 2,
                unit_len: 2,
                encoding: Encoding::Latin1,
                owner: Some(strong(dense.erase())),
            },
            &scope,
        );
        {
            let raw = external.as_tagged(heap).as_ref();
            assert_edges!("ExternalString", raw, [raw.header.map, raw.owner]);
        }

        let float = heap.allocate_handle::<Float>(1.5, &scope);
        assert_edges!(
            "Float",
            float.as_tagged(heap).as_ref(),
            [float.as_tagged(heap).header.map,]
        );

        let symbol = Symbol::new(heap, &scope, Some(dense));
        {
            let raw = symbol.as_tagged(heap).as_ref();
            assert_edges!("Symbol", raw, [raw.header.map, raw.description]);
        }

        let cell = heap.allocate_handle::<Cell>(
            CellInit {
                value: weak(object.erase()),
            },
            &scope,
        );
        {
            let raw = cell.as_tagged(heap).as_ref();
            assert_edges!("Cell", raw, [raw.header.map, raw.maybe_value]);
        }

        let code = heap.allocate_handle::<Code>(
            CodeInit {
                runtime: RuntimeId(2),
                bytes: &[0x90],
            },
            &scope,
        );
        {
            let raw = code.as_tagged(heap).as_ref();
            assert_edges!("Code", raw, [raw.header.map, raw.size]);
        }

        let feedback =
            heap.allocate_handle::<FeedbackVector>(FeedbackVectorInit { length: 2 }, &scope);
        {
            let raw = feedback.as_tagged(heap).as_ref();
            let slot = |i| unsafe { &*raw.slots.as_ptr().add(i) };
            assert_edges!("FeedbackVector", raw, [raw.header.map, slot(0), slot(1),]);
        }

        let handlers = heap.allocate_handle::<HandlerTable>(
            HandlerTableInit {
                entries: &[HandlerEntryInit::new(0, 1, 2)],
            },
            &scope,
        );
        assert_edges!(
            "HandlerTable",
            handlers.as_tagged(heap).as_ref(),
            [handlers.as_tagged(heap).header.map,]
        );

        let pair = heap.allocate_handle::<AccessorPair>((undefined, undefined), &scope);
        {
            let raw = pair.as_tagged(heap).as_ref();
            assert_edges!("AccessorPair", raw, [raw.header.map, raw.get, raw.set]);
        }

        let info = heap.allocate_handle::<AccessorInfo>(
            AccessorInfoInit {
                getter: Some(NativeIndex(0)),
                setter: Some(NativeIndex(1)),
                data: Some(object.erase()),
            },
            &scope,
        );
        {
            let raw = info.as_tagged(heap).as_ref();
            assert_edges!("AccessorInfo", raw, [raw.header.map, raw.data]);
        }

        let prototype_info = heap.allocate_handle::<PrototypeInfo>((), &scope);
        {
            let raw = prototype_info.as_tagged(heap).as_ref();
            assert_edges!(
                "PrototypeInfo",
                raw,
                [
                    raw.header.map,
                    raw.prototype_users,
                    raw.users_len,
                    raw.registry_slot,
                ]
            );
        }

        let handler_data = [weak(undefined.clone()), weak(object.erase())];
        let data_handler = heap.allocate_handle::<DataHandler>(
            DataHandlerInit {
                smi_handler: Smi::new(1),
                validity_cell: {
                    let cell = heap
                        .known()
                        .invalid_prototype_validity_cell
                        .get_handle(heap, &scope)
                        .erase();
                    weak(cell)
                },
                data: &handler_data,
            },
            &scope,
        );
        {
            let raw = data_handler.as_tagged(heap).as_ref();
            assert_edges!(
                "DataHandler",
                raw,
                [
                    raw.header.map,
                    raw.validity_cell,
                    unsafe { &*raw.data.as_ptr().add(0) },
                    unsafe { &*raw.data.as_ptr().add(1) },
                ]
            );
        }

        let dictionary =
            heap.allocate_handle::<NumberDictionary>(NumberDictionaryInit { capacity: 4 }, &scope);
        {
            let raw = dictionary.as_tagged(heap).as_ref();
            let mut expected = vec![
                raw.header.map.as_raw() as *const RawCell as usize,
                raw.capacity.as_raw() as *const RawCell as usize,
                raw.nof.as_raw() as *const RawCell as usize,
                raw.nod.as_raw() as *const RawCell as usize,
                raw.flags.as_raw() as *const RawCell as usize,
            ];
            let object = HeapPtr::from_ref(raw).cast::<()>();
            let start = object.addr();
            let size = unsafe { object_layout(object) }.size();
            let mut visitor = Visited(Vec::new());
            unsafe { visit_object(object, &mut visitor) };
            let mut actual = visitor.0;
            actual.sort_unstable();
            for i in 0..raw.capacity() * 3 {
                expected.push(
                    unsafe { &*raw.entries.as_ptr().add(i) }.as_raw() as *const RawCell as usize,
                );
            }
            expected.sort_unstable();
            assert_eq!(actual, expected, "edge set: NumberDictionary");
            assert!(
                actual
                    .iter()
                    .all(|cell| *cell >= start && *cell < start + size)
            );
        }

        let names = [
            DenseString::from_utf8(heap, &scope, "a"),
            DenseString::from_utf8(heap, &scope, "b"),
        ];
        let scope_info = heap.allocate_handle::<ScopeInfo>(
            ScopeInfoInit {
                names: scope.stage_handles(&names),
                infos: &[0, 0],
                flags: 0,
            },
            &scope,
        );
        {
            let raw = scope_info.as_tagged(heap).as_ref();
            assert_edges!(
                "ScopeInfo",
                raw,
                [
                    raw.header.map,
                    raw.flags,
                    raw.local_count,
                    unsafe { &*raw.data.as_ptr().add(0) },
                    unsafe { &*raw.data.as_ptr().add(1) },
                    unsafe { &*raw.data.as_ptr().add(2) },
                    unsafe { &*raw.data.as_ptr().add(3) },
                ]
            );
        }

        let context = heap.allocate_handle::<ContextObject>(
            ContextInit {
                outer: None,
                slots: empty_array.clone(),
                scope_info: scope_info.clone(),
            },
            &scope,
        );
        {
            let raw = context.as_tagged(heap).as_ref();
            assert_edges!(
                "ContextObject",
                raw,
                [
                    raw.header.map,
                    raw.outer,
                    raw.slots,
                    raw.scope_info,
                    raw.extension,
                ]
            );
        }

        let name_index = heap.allocate_handle::<NameToIndexHashTable>(
            NameToIndexHashTableInit { capacity: 4 },
            &scope,
        );
        {
            let raw = name_index.as_tagged(heap).as_ref();
            let object = HeapPtr::from_ref(raw).cast::<()>();
            let start = object.addr();
            let size = unsafe { object_layout(object) }.size();
            let mut visitor = Visited(Vec::new());
            unsafe { visit_object(object, &mut visitor) };
            let mut actual = visitor.0;
            actual.sort_unstable();
            let mut expected = vec![
                raw.header.map.as_raw() as *const RawCell as usize,
                raw.capacity.as_raw() as *const RawCell as usize,
                raw.count.as_raw() as *const RawCell as usize,
            ];
            for i in 0..raw.capacity() {
                let entry = raw.entry(i);
                expected.push(entry.name.as_raw() as *const RawCell as usize);
                expected.push(entry.index.as_raw() as *const RawCell as usize);
            }
            expected.sort_unstable();
            assert_eq!(actual, expected, "edge set: NameToIndexHashTable");
            assert!(
                actual
                    .iter()
                    .all(|cell| *cell >= start && *cell < start + size)
            );
        }

        let script_context = heap.allocate_handle::<ScriptContextTable>(
            ScriptContextTableInit {
                capacity: 2,
                names: name_index.clone(),
            },
            &scope,
        );
        {
            let raw = script_context.as_tagged(heap).as_ref();
            let object = HeapPtr::from_ref(raw).cast::<()>();
            let start = object.addr();
            let size = unsafe { object_layout(object) }.size();
            let mut visitor = Visited(Vec::new());
            unsafe { visit_object(object, &mut visitor) };
            let mut actual = visitor.0;
            actual.sort_unstable();
            let mut expected = vec![
                raw.header.map.as_raw() as *const RawCell as usize,
                raw.capacity.as_raw() as *const RawCell as usize,
                raw.length.as_raw() as *const RawCell as usize,
                raw.names.as_raw() as *const RawCell as usize,
            ];
            for i in 0..raw.capacity() {
                expected.push(
                    unsafe { &*raw.contexts.as_ptr().add(i) }.as_raw() as *const RawCell as usize,
                );
            }
            expected.sort_unstable();
            assert_eq!(actual, expected, "edge set: ScriptContextTable");
            assert!(
                actual
                    .iter()
                    .all(|cell| *cell >= start && *cell < start + size)
            );
        }

        let proxy = heap.allocate_handle::<ProxyObject>(
            ProxyInit {
                map: heap.known().proxy_map.get_handle(heap, &scope),
                target: object.erase(),
                handler: array.erase(),
            },
            &scope,
        );
        {
            let raw = proxy.as_tagged(heap).as_ref();
            assert_edges!(
                "ProxyObject",
                raw,
                [raw.header.map, raw.target, raw.handler]
            );
        }

        let map = heap.allocate_handle::<Map>(
            MapInit {
                kind: MapKind::OBJECT.union(MapKind::EXTENDABLE),
                value_slot_count: 0,
                descriptors: &[],
                prototype: null,
            },
            &scope,
        );
        {
            let raw = map.as_tagged(heap).as_ref();
            assert_edges!(
                "Map",
                raw,
                [
                    raw.header.map,
                    raw.prototype,
                    raw.pred,
                    raw.transitions,
                    raw.prototype_info,
                    raw.prototype_validity_cell,
                ]
            );
        }

        let callable_info = heap.allocate_handle::<CallableInfoObject>(
            CallableInfoInit::Native {
                index: NativeIndex(0),
                mode: 0,
            },
            &scope,
        );
        {
            let raw = callable_info.as_tagged(heap).as_ref();
            assert_edges!(
                "CallableInfoObject",
                raw,
                [
                    raw.header.map,
                    raw.function_data,
                    raw.constants,
                    raw.handlers,
                    raw.feedback,
                    raw.name,
                    raw.descriptor,
                ]
            );
        }

        let u8_ty = vm
            .ffi()
            .types()
            .get(heap, TypeId::from_i64(TypeKind::U8 as i64))
            .expect("u8");
        let u8_handle = scope.handle(u8_ty);
        let type_name = DenseString::from_utf8(heap, &scope, "T");
        let fields = heap.allocate_handle::<FixedArray>(
            scope.stage(&[undefined.as_tagged(heap), u8_handle.as_tagged(heap).erase()]),
            &scope,
        );
        let params = heap.allocate_handle::<FixedArray<TypeInfo>>(
            scope.stage(&[u8_handle.as_tagged(heap)]),
            &scope,
        );
        let type_info = heap.allocate_handle::<TypeInfo>(
            TypeInfoInit {
                kind: TypeKind::Struct,
                id: TypeId::from_i64(999),
                size: 8,
                align: 8,
                flags: 0,
                name: Some(type_name),
                pointee: Some(u8_handle.clone()),
                element: Some(u8_handle.clone()),
                len: 1,
                fields: Some(fields),
                params: Some(params),
                result: Some(u8_handle.clone()),
                cif: 0,
            },
            &scope,
        );
        type_info
            .as_tagged(heap)
            .set_value_map(heap, map.as_tagged(heap));
        {
            let raw = type_info.as_tagged(heap).as_ref();
            assert_edges!(
                "TypeInfo",
                raw,
                [
                    raw.header.map,
                    raw.name,
                    raw.pointee,
                    raw.element,
                    raw.fields,
                    raw.params,
                    raw.result,
                    raw.value_map,
                ]
            );
        }

        let cvalue = heap.allocate_handle::<CValue>(
            CValueInit {
                ty: type_info.clone(),
                map: None,
                address: bytes.as_tagged(heap).as_ptr() as *mut u8,
                flags: vm::CValueFlags::NONE,
                storage: Some(bytes.clone()),
                library: None,
                callable: None,
            },
            &scope,
        );
        let library_path = DenseString::from_utf8(heap, &scope, "");
        let library = heap.allocate_handle::<ForeignLibrary>(
            ForeignLibraryInit {
                handle: cvalue.clone(),
                path: library_path,
            },
            &scope,
        );
        {
            let raw = library.as_tagged(heap).as_ref();
            assert_edges!(
                "ForeignLibrary",
                raw,
                [raw.header.map, raw.handle, raw.path]
            );
        }

        {
            let raw = cvalue.as_tagged(heap).as_ref();
            assert_edges!(
                "CValue",
                raw,
                [
                    raw.header.map,
                    raw.slots,
                    raw.elements,
                    raw.ty,
                    raw.address,
                    raw.flags,
                    raw.storage,
                    raw.library,
                    raw.callable,
                ]
            );
        }

        let _ = undefined;
    });
}
