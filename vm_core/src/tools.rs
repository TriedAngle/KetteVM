use crate::{
    Args, Handle, HandleScope, HandleSlice, Heap, HostCtx, Map, MapInit, MapKind, NativeIndex,
    Object, PropertyDescriptor, StringInterner, Tagged, Value, VmError, WellKnown,
};

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum Termination {
    Shutdown,
}

pub struct KetteTools;

impl KetteTools {
    pub fn force_minor_gc<'a>(
        ctx: HostCtx<'a>,
        _new_target: Option<Handle<'_, Value>>,
        _args: Args,
    ) -> Tagged<'a, Value> {
        let HostCtx { vm, heap, .. } = ctx;
        heap.collect_minor();
        vm.known().undefined.get(heap).erase()
    }

    pub fn force_major_gc<'a>(
        ctx: HostCtx<'a>,
        _new_target: Option<Handle<'_, Value>>,
        _args: Args,
    ) -> Tagged<'a, Value> {
        let HostCtx { vm, heap, .. } = ctx;
        heap.collect();
        vm.known().undefined.get(heap).erase()
    }

    pub fn shutdown<'a>(
        ctx: HostCtx<'a>,
        _new_target: Option<Handle<'_, Value>>,
        _args: Args,
    ) -> Tagged<'a, Value> {
        let HostCtx {
            vm, heap, state, ..
        } = ctx;
        heap.cancel_executions(&|| {});
        vm.note_shutdown();
        state.set_termination(Termination::Shutdown);
        state.set_pending_exception(vm.known().undefined.get(heap));
        vm.known().exception.get(heap).erase()
    }

    pub fn install(
        heap: &mut Heap,
        roots: &WellKnown,
        interner: &StringInterner,
        scope: &HandleScope<'_>,
        natives: [NativeIndex; 3],
    ) -> Result<(), VmError> {
        let global = roots.global_object.get_handle(heap, scope);
        let map = heap.allocate_handle::<Map>(
            MapInit {
                kind: MapKind::OBJECT.union(MapKind::EXTENDABLE),
                value_slot_count: 0,
                descriptors: &[],
                prototype: roots.object_prototype.get_handle(heap, scope).erase(),
            },
            scope,
        );
        let tools = scope.handle(heap.new_object(scope, map, HandleSlice::EMPTY));

        let methods: [(&str, NativeIndex); 3] = [
            ("forceMinorGC", natives[0]),
            ("forceMajorGC", natives[1]),
            ("shutdown", natives[2]),
        ];
        for (name, index) in methods {
            let method = Object::native_function(heap, scope, index)?;
            let name_str = interner.intern_str(heap, scope, name);
            let method_name = scope.handle(name_str.as_tagged(heap));
            Object::define_own_property(
                heap,
                scope,
                tools,
                method_name,
                PropertyDescriptor::method(method.erase()),
            )?;
        }

        let name_str = interner.intern_str(heap, scope, "KetteTools");
        let name = scope.handle(name_str.as_tagged(heap));
        Object::define_own_property(
            heap,
            scope,
            global,
            name,
            PropertyDescriptor::data(tools.erase()),
        )?;
        Ok(())
    }
}
