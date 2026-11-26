use super::*;
use crate::slow::*;
use crate::utils::*;
use vm_core::{Atom, Float, Handle};

#[cfg(not(feature = "fast"))]
use handler_derive::handler_loop_match as handler;
#[cfg(feature = "fast")]
use handler_derive::handler_tail_call as handler;

#[handler(Load)]
pub fn op_load(frame: Frame) {
    let r = frame.signed::<0>();
    frame.set_acc(frame.regs().get(r, frame.ctx().heap()));
    next!()
}

#[handler(Move)]
pub fn op_move(frame: Frame) {
    let dst = frame.signed::<0>();
    let src = frame.signed::<1>();
    let val = frame.regs().get(src, frame.ctx().heap());
    frame.regs().set(dst, val);
    next!()
}

#[handler(LoadSmi)]
pub fn op_load_smi(frame: Frame) {
    let imm = frame.signed::<0>();
    let val = Smi::new(imm as i64).into_tagged();
    frame.set_acc(val);
    next!()
}

#[handler(LoadZero)]
pub fn op_load_zero(frame: Frame) {
    frame.set_acc(Smi::new(0).into_tagged());
    next!()
}

#[handler(LoadNull)]
pub fn op_load_null(frame: Frame) {
    let heap = frame.ctx().heap();
    let val = roots.null.get(heap).erase();
    frame.set_acc(val);
    next!()
}

#[handler(LoadConstant)]
pub fn op_load_constant(frame: Frame) {
    let heap = frame.ctx().heap();
    let idx = frame.unsigned::<0>();
    let v = frame.ctx().constants_ref(heap).at(heap, idx);
    frame.set_acc(v);
    next!()
}

#[handler(LoadUndefined)]
pub fn op_load_undefined(frame: Frame) {
    let heap = frame.ctx().heap();
    let v = roots.undefined.get(heap).erase();
    frame.set_acc(v);
    next!()
}

#[handler(LoadTrue)]
pub fn op_load_true(frame: Frame) {
    let heap = frame.ctx().heap();
    let v = roots.true_object.get(heap).erase();
    frame.set_acc(v);
    next!()
}

#[handler(LoadFalse)]
pub fn op_load_false(frame: Frame) {
    let heap = frame.ctx().heap();
    let v = roots.false_object.get(heap).erase();
    frame.set_acc(v);
    next!()
}

#[handler(TestType)]
pub fn op_test_type(frame: Frame) {
    let kind = frame.unsigned::<2>();
    if kind == TestTypeKind::IntoArray as usize {
        let vm = frame.ctx().vm();
        let state = frame.ctx().state();
        let same = state.handle_scope(|scope| {
            let heap = frame.ctx().heap_mut();
            let value = frame.regs().get(frame.signed::<0>(), heap);
            if value
                .as_heap_object()
                .is_some_and(|object| object.as_ref().is_array_exotic(heap))
            {
                return true;
            }
            let Some(object) = value.as_heap_object() else {
                return false;
            };
            let object = scope.handle(object);
            let name = heap
                .known()
                .strings
                .into_array
                .get_handle(heap, &scope)
                .erase();
            Object::has_property(vm, heap, state, object.erase(), name).unwrap_or(false)
        });
        frame.set_acc(if same {
            roots.true_object.get(frame.ctx().heap()).erase()
        } else {
            roots.false_object.get(frame.ctx().heap()).erase()
        });
        next!()
    }
    let heap = frame.ctx().heap();
    let value = frame.regs().get(frame.signed::<0>(), heap);
    let ty = frame.regs().get(frame.signed::<1>(), heap);
    let kind = frame.unsigned::<2>();
    let same = if kind == TestTypeKind::Object as usize {
        value.as_heap_object().is_some()
            && StringRef::try_from(heap, value).is_none()
            && !value.ptr_eq(roots.null.get(heap))
            && !value.ptr_eq(roots.undefined.get(heap))
            && !value.ptr_eq(roots.true_object.get(heap))
            && !value.ptr_eq(roots.false_object.get(heap))
    } else if kind == TestTypeKind::Array as usize {
        value
            .as_heap_object()
            .is_some_and(|object| object.as_ref().is_array_exotic(heap))
    } else if kind == TestTypeKind::String as usize {
        StringRef::try_from(heap, value).is_some()
    } else if kind == TestTypeKind::Number as usize {
        Smi::decode(value.raw()).is_some() || value.get_as::<Float>(heap).is_some()
    } else if kind == TestTypeKind::Boolean as usize {
        value.ptr_eq(roots.true_object.get(heap)) || value.ptr_eq(roots.false_object.get(heap))
    } else if kind == TestTypeKind::Null as usize {
        value.ptr_eq(roots.null.get(heap))
    } else if kind == TestTypeKind::Undefined as usize {
        value.ptr_eq(roots.undefined.get(heap))
    } else if kind == TestTypeKind::Function as usize {
        value.is_callable(heap)
    } else if kind == TestTypeKind::User as usize {
        match (value.as_heap_object(), ty.as_heap_object()) {
            (Some(value), Some(ty)) => value.map(heap).ptr_eq(ty.map(heap)),
            _ => false,
        }
    } else {
        false
    };
    let v = if same {
        roots.true_object.get(heap)
    } else {
        roots.false_object.get(heap)
    };
    frame.set_acc(v.erase());
    next!()
}

#[handler(LoadLength)]
pub fn op_load_length(frame: Frame) {
    let ctx = frame.ctx();
    let vm = ctx.vm();
    let state = ctx.state();
    let result = state.handle_scope(|scope| -> Acc {
        let heap = ctx.heap_mut();
        let value = frame.regs().get(frame.signed::<0>(), heap);
        if let Some(string) = StringRef::try_from(heap, value) {
            return Acc::set(Smi::new(string.len() as i64).into_tagged());
        }
        let Some(object) = value.as_heap_object() else {
            return Acc::set(roots.undefined.get(heap).erase());
        };
        let object = scope.handle(object);
        if object.as_tagged(heap).as_ref().is_array_exotic(heap) {
            return Acc::set(
                Smi::new(object.as_tagged(heap).as_ref().length() as i64).into_tagged(),
            );
        }
        let length = heap.known().strings.length.get_handle(heap, &scope).erase();
        let has_length = match Object::has_property(vm, heap, state, object.erase(), length) {
            Ok(value) => value,
            Err(error) => return Acc::set(raise_runtime(heap, error)),
        };
        if has_length {
            let value = match Object::get_property(
                vm,
                heap,
                state,
                object.erase(),
                object.erase(),
                length,
                false,
            ) {
                Ok(value) => value,
                Err(error) => return Acc::set(raise_runtime(heap, error)),
            };
            return Acc::set(value);
        }
        let size = vm.interner().intern_str(heap, &scope, "size").erase();
        let has_size = match Object::has_property(vm, heap, state, object.erase(), size) {
            Ok(value) => value,
            Err(error) => return Acc::set(raise_runtime(heap, error)),
        };
        if has_size {
            let value = match Object::get_property(
                vm,
                heap,
                state,
                object.erase(),
                object.erase(),
                size,
                false,
            ) {
                Ok(value) => value,
                Err(error) => return Acc::set(raise_runtime(heap, error)),
            };
            return Acc::set(value);
        }
        Acc::set(roots.undefined.get(heap).erase())
    });
    frame.set_acc(result.get(frame.ctx().heap()));
    next!()
}

#[handler(TailGuard)]
pub fn op_tail_guard(frame: Frame) {
    let heap = frame.ctx().heap();
    let callee = frame.regs().get(frame.signed::<0>(), heap);
    let current = frame.ctx().stack().callable(heap, frame.ctx().frame_base());
    let v = if callee.ptr_eq(current) {
        roots.true_object.get(heap).erase()
    } else {
        roots.false_object.get(heap).erase()
    };
    frame.set_acc(v);
    next!()
}

#[handler(LoadHole)]
pub fn op_load_hole(frame: Frame) {
    let heap = frame.ctx().heap();
    let v = roots.the_hole.get(heap).erase();
    frame.set_acc(v);
    next!()
}

#[handler(LoadNewTarget)]
pub fn op_load_new_target(frame: Frame) {
    let val = frame
        .ctx()
        .stack()
        .frame_header(frame.ctx().frame_base())
        .new_target_slot()
        .get(frame.ctx().heap());
    frame.set_acc(val);
    next!()
}

#[handler(LoadContext)]
pub fn op_load_context(frame: Frame) {
    let val = frame
        .ctx()
        .stack()
        .frame_header(frame.ctx().frame_base())
        .context_slot()
        .get(frame.ctx().heap());
    frame.set_acc(val);
    next!()
}

#[handler(LoadCurrentClosure)]
pub fn op_load_current_closure(frame: Frame) {
    let val = frame
        .ctx()
        .stack()
        .frame_header(frame.ctx().frame_base())
        .callable_slot()
        .get(frame.ctx().heap());
    frame.set_acc(val);
    next!()
}

enum GlobalLoadFast<'a> {
    Value(Tagged<'a, Value>),
    Getter(Tagged<'a, Value>),
    Uninitialized,
    Miss,
}

fn global_load_fast<'a>(
    heap: &'a Heap,
    vector: Option<Tagged<'a, FeedbackVector>>,
    fb_slot: usize,
    global: Tagged<'a, Value>,
) -> GlobalLoadFast<'a> {
    let hit = match InlineCache::probe_mono(heap, vector, fb_slot, global) {
        MonoProbe::Value(v) => Some(LoadHit::Value(v)),
        MonoProbe::Handler(obj, handler) => InlineCache::apply_mono(heap, obj, handler),
        MonoProbe::Poly {
            obj,
            map,
            pairs,
            start,
        } => InlineCache::try_load_resume(heap, obj, map, pairs, start),
        MonoProbe::Miss | MonoProbe::NotReceiver => None,
    };
    if let Some(hit) = hit {
        match hit {
            LoadHit::Value(v) => return GlobalLoadFast::Value(v),
            LoadHit::Getter(getter) => return GlobalLoadFast::Getter(getter),
            _ => {}
        }
    }
    match InlineCache::try_load_global_lexical(heap, vector, fb_slot) {
        Some(GlobalLexicalHit::Value(v)) => GlobalLoadFast::Value(v),
        Some(GlobalLexicalHit::Uninitialized) => GlobalLoadFast::Uninitialized,
        None => GlobalLoadFast::Miss,
    }
}

#[handler(LoadGlobal)]
pub fn op_load_global(frame: Frame) {
    let heap = frame.ctx().heap();
    let fb = frame.unsigned::<1>();
    let global = roots.global_object.get(heap).erase();
    match global_load_fast(heap, frame.ctx().feedback_ref(heap), fb, global) {
        GlobalLoadFast::Value(v) => {
            frame.set_acc(v);
            next!()
        }
        GlobalLoadFast::Getter(getter) => {
            frame.set_acc(getter);
            tail!(slow_global_getter)
        }
        GlobalLoadFast::Uninitialized => {
            let _ = frame.ctx().raise(VmError::UninitializedVariable);
            tail!(throw_dispatch)
        }
        GlobalLoadFast::Miss => {
            tail!(slow_global_load)
        }
    }
}

#[handler(LoadGlobalNoThrow)]
pub fn op_load_global_no_throw(frame: Frame) {
    let heap = frame.ctx().heap();
    let fb = frame.unsigned::<1>();
    let global = roots.global_object.get(heap).erase();
    match global_load_fast(heap, frame.ctx().feedback_ref(heap), fb, global) {
        GlobalLoadFast::Value(v) => {
            frame.set_acc(v);
            next!()
        }
        GlobalLoadFast::Getter(getter) => {
            frame.set_acc(getter);
            tail!(slow_global_getter)
        }
        GlobalLoadFast::Uninitialized => {
            let _ = frame.ctx().raise(VmError::UninitializedVariable);
            tail!(throw_dispatch)
        }
        GlobalLoadFast::Miss => {
            tail!(slow_global_load_no_throw)
        }
    }
}

#[handler(LoadNamedProperty)]
pub fn op_load_named_property(frame: Frame) {
    let heap = frame.ctx().heap();
    let recv = frame.regs().get(frame.signed::<0>(), heap);
    let fb = frame.unsigned::<2>();
    let hit = match InlineCache::probe_mono(heap, frame.ctx().feedback_ref(heap), fb, recv) {
        MonoProbe::Value(v) => Some(LoadHit::Value(v)),
        MonoProbe::Handler(obj, handler) => InlineCache::apply_mono(heap, obj, handler),
        MonoProbe::Poly {
            obj,
            map,
            pairs,
            start,
        } => InlineCache::try_load_resume(heap, obj, map, pairs, start),
        MonoProbe::Miss | MonoProbe::NotReceiver => None,
    };
    if let Some(hit) = hit {
        match hit {
            LoadHit::Value(v) => {
                frame.set_acc(v);
                next!()
            }
            LoadHit::Getter(getter) => {
                frame.set_acc(getter);
                tail!(slow_named_getter)
            }
            LoadHit::NativeGetter(accessor) => {
                frame.set_acc(accessor);
                tail!(slow_named_native_getter)
            }
            _ => {}
        }
    }
    tail!(slow_named_load)
}

#[handler(LoadKeyedProperty)]
pub fn op_load_keyed_property(frame: Frame) {
    let heap = frame.ctx().heap();
    let recv = frame.regs().get(frame.signed::<0>(), heap);
    let fb = frame.unsigned::<1>();
    if let Some(idx) = frame.acc().to_i64()
        && idx >= 0
    {
        if let Some(obj) = recv.as_heap_object()
            && !obj.as_ref().map(heap).kind().is_arguments()
            && let Some(v) = obj.as_ref().element_value_dense(heap, idx as usize)
        {
            frame.set_acc(v);
            next!()
        }
        if let Some(ElementHit::Value(v)) = InlineCache::try_load_element(
            heap,
            frame.ctx().feedback_ref(heap),
            fb,
            recv,
            idx as usize,
        ) {
            frame.set_acc(v);
            next!()
        }
    }
    tail!(slow_keyed_load)
}

#[handler(LoadOwnSlotOrHole)]
pub fn op_load_own_slot_or_hole(frame: Frame) {
    let heap = frame.ctx().heap();
    let value = frame.regs().get(frame.signed::<0>(), heap);
    let name_idx = frame.unsigned::<1>();
    let name = frame.ctx().constants_ref(heap).at(heap, name_idx).as_name();
    let hole = heap.known().the_hole.get(heap).erase();
    let mut result = hole;
    if let Some(object) = value.as_heap_object() {
        for descriptor in object.as_ref().map(heap).descriptors() {
            if descriptor.name(heap).ptr_eq(name) {
                if descriptor.flags().is_enumerable() && !descriptor.flags().is_accessor() {
                    result = object.as_ref().slot(heap, descriptor.offset()).get(heap);
                }
                break;
            }
        }
    }
    frame.set_acc(result);
    next!()
}

#[handler(OwnSlotCount)]
pub fn op_own_slot_count(frame: Frame) {
    let heap = frame.ctx().heap();
    let value = frame.regs().get(frame.signed::<0>(), heap);
    let count = value
        .as_heap_object()
        .map(|object| {
            object
                .as_ref()
                .map(heap)
                .descriptors()
                .iter()
                .filter(|descriptor| {
                    descriptor.flags().is_enumerable()
                        && !descriptor.flags().is_accessor()
                        && descriptor.name(heap).as_index().is_none()
                })
                .count()
        })
        .unwrap_or(0);
    frame.set_acc(Smi::new(count as i64).into_tagged());
    next!()
}

#[handler(LoadIndexed)]
pub fn op_load_indexed(frame: Frame) {
    let mut element_index = None;
    {
        let heap = frame.ctx().heap();
        let recv = frame.regs().get(frame.signed::<0>(), heap);
        let fb = frame.unsigned::<1>();
        if let Some(idx) = frame.acc().to_i64()
            && idx >= 0
        {
            element_index = Some(idx as usize);
            if let Some(obj) = recv.as_heap_object()
                && !obj.as_ref().map(heap).kind().is_arguments()
                && let Some(v) = obj.as_ref().element_value_dense(heap, idx as usize)
            {
                frame.set_acc(v);
                next!()
            }
            if let Some(ElementHit::Value(v)) = InlineCache::try_load_element(
                heap,
                frame.ctx().feedback_ref(heap),
                fb,
                recv,
                idx as usize,
            ) {
                frame.set_acc(v);
                next!()
            }
        }
    }
    if let Some(index) = element_index {
        let pc = frame.pc();
        let vm = frame.ctx().vm();
        let state = frame.ctx().state();
        let fb = frame.unsigned::<1>();
        let v = state.handle_scope(|scope| {
            let heap = frame.ctx().heap();
            let recv = frame.regs().get(frame.signed::<0>(), heap);
            let key = frame.acc();
            let method = InlineCache::try_load_index_method(
                heap,
                frame.ctx().feedback_ref(heap),
                fb,
                recv,
                index,
            )?;
            let recv = scope.handle(recv);
            let key = scope.handle(key);
            let method = scope.handle(method);
            let heap = frame.ctx().heap_mut();
            Some(call_index_get(vm, heap, state, &scope, recv, key, method))
        });
        if let Some(v) = v {
            frame.set_acc(v);
            reenter!(pc)
        }
    }
    tail!(slow_indexed_load)
}

#[handler(LoadIndexedOrHole)]
pub fn op_load_indexed_or_hole(frame: Frame) {
    let heap = frame.ctx().heap();
    let recv = frame.regs().get(frame.signed::<0>(), heap);
    let hole = heap.known().the_hole.get(heap).erase();
    let key = frame.acc();
    let result = if let Some(index) = key.to_i64()
        && index >= 0
        && let Some(object) = recv.as_heap_object()
    {
        if let Some(value) = object.as_ref().element_value(heap, index as usize) {
            value
        } else {
            let name = key.as_name();
            let mut found = hole;
            for descriptor in object.as_ref().map(heap).descriptors() {
                if descriptor.name(heap).ptr_eq(name) {
                    if descriptor.flags().is_enumerable() && !descriptor.flags().is_accessor() {
                        found = object.as_ref().slot(heap, descriptor.offset()).get(heap);
                    }
                    break;
                }
            }
            found
        }
    } else {
        hole
    };
    frame.set_acc(result);
    next!()
}

#[handler(LoadKeyedPropertyReg)]
pub fn op_load_keyed_property_reg(frame: Frame) {
    let heap = frame.ctx().heap();
    let recv = frame.regs().get(frame.signed::<0>(), heap);
    let key = frame.regs().get(frame.signed::<1>(), heap);
    let fb = frame.unsigned::<2>();
    if let Some(idx) = key.to_i64()
        && idx >= 0
    {
        if let Some(obj) = recv.as_heap_object()
            && !obj.as_ref().map(heap).kind().is_arguments()
            && let Some(v) = obj.as_ref().element_value_dense(heap, idx as usize)
        {
            frame.set_acc(v);
            next!()
        }
        if let Some(ElementHit::Value(v)) = InlineCache::try_load_element(
            heap,
            frame.ctx().feedback_ref(heap),
            fb,
            recv,
            idx as usize,
        ) {
            frame.set_acc(v);
            next!()
        }
    }
    tail!(slow_keyed_load_reg)
}

#[handler(LoadElementImm)]
pub fn op_load_element_imm(frame: Frame) {
    let heap = frame.ctx().heap();
    let recv = frame.regs().get(frame.signed::<0>(), heap);
    let idx = frame.unsigned::<1>();
    let fb = frame.unsigned::<2>();
    if let Some(obj) = recv.as_heap_object()
        && !obj.as_ref().map(heap).kind().is_arguments()
        && let Some(v) = obj.as_ref().element_value_dense(heap, idx)
    {
        frame.set_acc(v);
        next!()
    }
    if let Some(ElementHit::Value(v)) =
        InlineCache::try_load_element(heap, frame.ctx().feedback_ref(heap), fb, recv, idx)
    {
        frame.set_acc(v);
        next!()
    }
    tail!(slow_keyed_load_imm)
}

// --- stores ---

#[handler(Store)]
pub fn op_store(frame: Frame) {
    frame.regs().set(frame.signed::<0>(), frame.acc());
    next!()
}

#[handler(StoreNamedProperty)]
pub fn op_store_named_property(frame: Frame) {
    let recv = frame.regs().get(frame.signed::<0>(), frame.ctx().heap());
    let name_idx = frame.unsigned::<1>();
    let fb_slot = frame.unsigned::<2>();
    let name = frame
        .ctx()
        .constants_ref(frame.ctx().heap())
        .at(frame.ctx().heap(), name_idx)
        .as_name();

    if InlineCache::try_store_fast(
        frame.ctx().heap(),
        frame.ctx().feedback_ref(frame.ctx().heap()),
        fb_slot,
        recv,
        name,
        frame.acc(),
    ) {
        next!()
    }
    tail!(slow_store_named)
}

#[handler(StoreKeyedProperty)]
pub fn op_store_keyed_property(frame: Frame) {
    let heap = frame.ctx().heap();
    let recv = frame.regs().get(frame.signed::<0>(), heap);
    let key = frame.regs().get(frame.signed::<1>(), heap);
    let fb = frame.unsigned::<2>();
    if let Some(idx) = key.to_i64()
        && idx >= 0
    {
        let idx = idx as usize;
        if recv.store_array_element_in_place(heap, idx, frame.acc()) {
            next!()
        }
        if let Some(v) = InlineCache::try_store_element(
            heap,
            frame.ctx().feedback_ref(heap),
            fb,
            recv,
            idx,
            frame.acc(),
        ) {
            frame.set_acc(v);
            next!()
        }
    }
    tail!(slow_keyed_store)
}

#[handler(StoreIndexed)]
pub fn op_store_indexed(frame: Frame) {
    let mut element_index = None;
    {
        let heap = frame.ctx().heap();
        let recv = frame.regs().get(frame.signed::<0>(), heap);
        let key = frame.regs().get(frame.signed::<1>(), heap);
        let fb = frame.unsigned::<2>();
        if let Some(idx) = key.to_i64()
            && idx >= 0
        {
            let idx = idx as usize;
            element_index = Some(idx);
            if recv.store_array_element_in_place(heap, idx, frame.acc()) {
                next!()
            }
            if let Some(v) = InlineCache::try_store_element(
                heap,
                frame.ctx().feedback_ref(heap),
                fb,
                recv,
                idx,
                frame.acc(),
            ) {
                frame.set_acc(v);
                next!()
            }
        }
    }
    if let Some(index) = element_index {
        let pc = frame.pc();
        let vm = frame.ctx().vm();
        let state = frame.ctx().state();
        let fb = frame.unsigned::<2>();
        let v = state.handle_scope(|scope| {
            let heap = frame.ctx().heap();
            let recv = frame.regs().get(frame.signed::<0>(), heap);
            let key = frame.regs().get(frame.signed::<1>(), heap);
            let value = frame.acc();
            let method = InlineCache::try_store_index_method(
                heap,
                frame.ctx().feedback_ref(heap),
                fb,
                recv,
                index,
            )?;
            let recv = scope.handle(recv);
            let key = scope.handle(key);
            let value = scope.handle(value);
            let method = scope.handle(method);
            let heap = frame.ctx().heap_mut();
            Some(call_index_set(
                vm, heap, state, &scope, recv, key, value, method,
            ))
        });
        if let Some(v) = v {
            frame.set_acc(v);
            reenter!(pc)
        }
    }
    tail!(slow_indexed_store)
}

#[handler(StoreKeyedPropertyNoShadow)]
pub fn op_store_keyed_property_no_shadow(frame: Frame) {
    let heap = frame.ctx().heap();
    let recv = frame.regs().get(frame.signed::<0>(), heap);
    let key = frame.regs().get(frame.signed::<1>(), heap);
    let fb = frame.unsigned::<2>();
    if let Some(idx) = key.to_i64()
        && idx >= 0
    {
        let idx = idx as usize;
        if recv.store_array_element_in_place(heap, idx, frame.acc()) {
            next!()
        }
        if let Some(v) = InlineCache::try_store_element(
            heap,
            frame.ctx().feedback_ref(heap),
            fb,
            recv,
            idx,
            frame.acc(),
        ) {
            frame.set_acc(v);
            next!()
        }
    }
    tail!(slow_keyed_store_no_shadow)
}

#[handler(StoreNamedPropertyNoShadow)]
pub fn op_store_named_property_no_shadow(frame: Frame) {
    let heap = frame.ctx().heap();
    let recv = frame.regs().get(frame.signed::<0>(), heap);
    let name_idx = frame.unsigned::<1>();
    let fb_slot = frame.unsigned::<2>();
    let name = frame.ctx().constants_ref(heap).at(heap, name_idx).as_name();
    if InlineCache::try_store_fast(
        heap,
        frame.ctx().feedback_ref(heap),
        fb_slot,
        recv,
        name,
        frame.acc(),
    ) {
        next!()
    }
    tail!(slow_store_named_no_shadow)
}

#[handler(StoreGlobal)]
pub fn op_store_global(frame: Frame) {
    let heap = frame.ctx().heap();
    let fb = frame.unsigned::<1>();
    let value = frame.acc();
    let name_idx = frame.unsigned::<0>();
    let name = frame
        .ctx()
        .constants_ref(heap)
        .at(heap, name_idx)
        .erase()
        .as_name();
    let global = roots.global_object.get(heap).erase();
    if InlineCache::try_store_fast(
        heap,
        frame.ctx().feedback_ref(heap),
        fb,
        global,
        name,
        value,
    ) {
        frame.set_acc(value);
        next!()
    }
    if let Some(v) =
        InlineCache::try_store_global_lexical(heap, frame.ctx().feedback_ref(heap), fb, value)
    {
        frame.set_acc(v);
        next!()
    }
    tail!(slow_store_global)
}

#[handler(AddParent)]
pub fn op_add_parent(frame: Frame) {
    tail!(slow_add_parent)
}

#[handler(DefineNamedOwnProperty)]
pub fn op_define_named_own_property(frame: Frame) {
    let pc = frame.pc();
    let base = frame.ctx().frame_base();
    let name_idx = frame.unsigned::<1>();
    let fb = frame.unsigned::<3>();
    {
        let heap = frame.ctx().heap();
        let recv = frame.regs().get(frame.signed::<0>(), heap);
        let name = frame.ctx().constants_ref(heap).at(heap, name_idx).as_name();
        if InlineCache::try_store_fast(
            heap,
            frame.ctx().feedback_ref(heap),
            fb,
            recv,
            name,
            frame.acc(),
        ) {
            next!()
        }
    }
    let state = frame.ctx().state();
    let (hit, val) = state.handle_scope(|scope| {
        let recv = scope.handle(frame.regs().get(frame.signed::<0>(), frame.ctx().heap()));
        let value = scope.handle(frame.acc());
        let heap = frame.ctx().heap_mut();
        let name = scope.handle(
            state
                .stack()
                .frame_header(base)
                .constants(heap)
                .at(heap, name_idx)
                .erase()
                .as_name(),
        );
        let feedback = state
            .stack()
            .frame_header(base)
            .feedback(heap)
            .map(|v| scope.handle(v));
        let hit = InlineCache::try_define_own(heap, &scope, feedback, fb, recv, name, value);
        (hit, value.as_tagged(heap).erase())
    });
    frame.set_acc(val);
    if hit {
        reenter!(pc)
    } else {
        tail!(slow_define_named_own_property)
    }
}

#[handler(DefineKeyedOwnProperty)]
pub fn op_define_keyed_own_property(frame: Frame) {
    let heap = frame.ctx().heap();
    let recv = frame.regs().get(frame.signed::<0>(), heap);
    let key = frame.regs().get(frame.signed::<1>(), heap);
    let fb = frame.unsigned::<3>();
    if let Some(index) = key.to_i64()
        && index >= 0
        && let Some(v) = InlineCache::try_store_element(
            heap,
            frame.ctx().feedback_ref(heap),
            fb,
            recv,
            index as usize,
            frame.acc(),
        )
    {
        frame.set_acc(v);
        next!()
    }
    tail!(slow_define_keyed_own_property)
}

#[handler(StoreInArrayLiteral)]
pub fn op_store_in_array_literal(frame: Frame) {
    let heap = frame.ctx().heap();
    let array = frame.regs().get(frame.signed::<0>(), heap);
    let index = frame.unsigned::<1>();
    let fb = frame.unsigned::<2>();
    if let Some(v) = InlineCache::try_store_element(
        heap,
        frame.ctx().feedback_ref(heap),
        fb,
        array,
        index,
        frame.acc(),
    ) {
        frame.set_acc(v);
        next!()
    }
    tail!(slow_store_in_array_literal)
}

// --- arithmetic ---

#[handler(Add)]
pub fn op_add(frame: Frame) {
    let h = frame.ctx().heap();
    let lhs = frame.regs().get(frame.signed::<0>(), h);
    if let (Some(a), Some(b)) = (lhs.smi_bits(), frame.acc().smi_bits())
        && let Some(sum) = a.checked_add(b)
    {
        frame.set_acc_smi(sum);
        next!()
    }
    if let (Some(a), Some(b)) = (lhs.as_number(h), frame.acc().as_number(h)) {
        let sum = a + b;
        if let Some(s) = Smi::from_f64(sum) {
            frame.set_acc(s.into_tagged());
            next!()
        }
        if let Some(v) = frame.ctx().heap_mut().try_new_float(sum) {
            frame.set_acc(v);
            next!()
        }
        frame.set_float(sum);
        tail!(slow_box_number)
    }
    tail!(slow_add)
}

#[handler(Sub)]
pub fn op_sub(frame: Frame) {
    let heap = frame.ctx().heap();
    let lhs = frame.regs().get(frame.signed::<0>(), heap);
    if let (Some(a), Some(b)) = (lhs.smi_bits(), frame.acc().smi_bits())
        && let Some(diff) = a.checked_sub(b)
    {
        frame.set_acc_smi(diff);
        next!()
    }
    if let (Some(a), Some(b)) = (lhs.as_number(heap), frame.acc().as_number(heap)) {
        let diff = a - b;
        if let Some(s) = Smi::from_f64(diff) {
            frame.set_acc(s.into_tagged());
            next!()
        }
        if let Some(v) = frame.ctx().heap_mut().try_new_float(diff) {
            frame.set_acc(v);
            next!()
        }
        frame.set_float(diff);
        tail!(slow_box_number_sub)
    }
    tail!(slow_numeric)
}

#[handler(AddImmediate)]
pub fn op_add_immediate(frame: Frame) {
    let heap = frame.ctx().heap();
    let lhs = frame.regs().get(frame.signed::<0>(), heap);
    let imm = frame.signed::<1>();
    if let Some(bits) = lhs.smi_bits()
        && let Some(sum) = bits.checked_add((imm as i64) << 1)
    {
        frame.set_acc_smi(sum);
        next!()
    }
    tail!(slow_add_immediate)
}

#[handler(SubImmediate)]
pub fn op_sub_immediate(frame: Frame) {
    let heap = frame.ctx().heap();
    let lhs = frame.regs().get(frame.signed::<0>(), heap);
    let imm = frame.signed::<1>();
    if let Some(bits) = lhs.smi_bits()
        && let Some(diff) = bits.checked_sub((imm as i64) << 1)
    {
        frame.set_acc_smi(diff);
        next!()
    }
    tail!(slow_numeric_immediate)
}

#[handler(Mul)]
pub fn op_mul(frame: Frame) {
    let heap = frame.ctx().heap();
    let lhs = frame.regs().get(frame.signed::<0>(), heap);
    if let (Some(a), Some(b)) = (lhs.smi_bits(), frame.acc().smi_bits())
        && let Some(product) = (a >> 1).checked_mul(b)
    {
        frame.set_acc_smi(product);
        next!()
    }
    if let (Some(a), Some(b)) = (lhs.as_number(heap), frame.acc().as_number(heap)) {
        let product = a * b;
        if let Some(s) = Smi::from_f64(product) {
            frame.set_acc(s.into_tagged());
            next!()
        }
        if let Some(v) = frame.ctx().heap_mut().try_new_float(product) {
            frame.set_acc(v);
            next!()
        }
        frame.set_float(product);
        tail!(slow_box_number_mul)
    }
    tail!(slow_mul)
}

#[handler(Div)]
pub fn op_div(frame: Frame) {
    let heap = frame.ctx().heap();
    let lhs = frame.regs().get(frame.signed::<0>(), heap);
    if let (Some(a), Some(b)) = (lhs.smi_bits(), frame.acc().smi_bits())
        && b != 0
        && a % b == 0
        && let Some(quotient) = a.checked_div(b)
        && let Some(encoded) = quotient.checked_mul(2)
    {
        frame.set_acc_smi(encoded);
        next!()
    }
    if let (Some(a), Some(b)) = (lhs.as_number(heap), frame.acc().as_number(heap)) {
        let quotient = a / b;
        if let Some(s) = Smi::from_f64(quotient) {
            frame.set_acc(s.into_tagged());
            next!()
        }
        if let Some(v) = frame.ctx().heap_mut().try_new_float(quotient) {
            frame.set_acc(v);
            next!()
        }
        frame.set_float(quotient);
        tail!(slow_box_number_div)
    }
    tail!(slow_div)
}

#[handler(Mod)]
pub fn op_mod(frame: Frame) {
    let heap = frame.ctx().heap();
    let lhs = frame.regs().get(frame.signed::<0>(), heap);
    if let (Some(a), Some(b)) = (lhs.to_i64(), frame.acc().to_i64())
        && b != 0
    {
        let val = Smi::new(a % b).into_tagged();
        frame.set_acc(val);
        next!()
    }
    tail!(slow_mod)
}

#[handler(Exp)]
pub fn op_exp(frame: Frame) {
    let _ = frame.signed::<0>();
    tail!(slow_exp)
}

#[handler(Negate)]
pub fn op_negate(frame: Frame) {
    if let Some(bits) = frame.acc().smi_bits()
        && bits != 0
        && let Some(neg) = bits.checked_neg()
    {
        frame.set_acc_smi(neg);
        next!()
    }
    if let Some(a) = frame.acc().as_number(frame.ctx().heap()) {
        let neg = -a;
        if let Some(s) = Smi::from_f64(neg) {
            frame.set_acc(s.into_tagged());
            next!()
        }
        if let Some(v) = frame.ctx().heap_mut().try_new_float(neg) {
            frame.set_acc(v);
            next!()
        }
        frame.set_float(neg);
        tail!(slow_box_number_negate)
    }
    tail!(slow_negate)
}

#[handler(MulImmediate)]
pub fn op_mul_immediate(frame: Frame) {
    let heap = frame.ctx().heap();
    let lhs = frame.regs().get(frame.signed::<0>(), heap);
    let imm = frame.signed::<1>();
    if let Some(bits) = lhs.smi_bits()
        && let Some(product) = (bits >> 1).checked_mul(imm as i64)
        && let Some(encoded) = product.checked_mul(2)
    {
        frame.set_acc_smi(encoded);
        next!()
    }
    tail!(slow_mul_immediate)
}

#[handler(DivImmediate)]
pub fn op_div_immediate(frame: Frame) {
    let heap = frame.ctx().heap();
    let lhs = frame.regs().get(frame.signed::<0>(), heap);
    let imm = frame.signed::<1>();
    if let Some(bits) = lhs.smi_bits() {
        let value = bits >> 1;
        if imm != 0
            && value % imm as i64 == 0
            && let Some(quotient) = value.checked_div(imm as i64)
            && let Some(encoded) = quotient.checked_mul(2)
        {
            frame.set_acc_smi(encoded);
            next!()
        }
    }
    tail!(slow_div_immediate)
}

#[handler(ModImmediate)]
pub fn op_mod_immediate(frame: Frame) {
    let heap = frame.ctx().heap();
    let lhs = frame.regs().get(frame.signed::<0>(), heap);
    let imm = frame.signed::<1>();
    if imm != 0
        && let Some(bits) = lhs.smi_bits()
    {
        frame.set_acc_smi(((bits >> 1) % imm as i64) << 1);
        next!()
    }
    tail!(slow_mod_immediate)
}

#[handler(ExpImmediate)]
pub fn op_exp_immediate(frame: Frame) {
    let _ = (frame.signed::<0>(), frame.signed::<1>());
    tail!(slow_exp_immediate)
}

#[handler(AddLoc)]
pub fn op_add_loc(frame: Frame) {
    let heap = frame.ctx().heap();
    let dst = frame.signed::<0>();
    let src = frame.signed::<1>();
    let lhs = frame.regs().get(dst, heap);
    let rhs = frame.regs().get(src, heap);
    if let (Some(a), Some(b)) = (lhs.smi_bits(), rhs.smi_bits())
        && let Some(sum) = a.checked_add(b)
    {
        let val = Tagged::from_smi_bits(sum);
        frame.regs().set(dst, val);
        frame.set_acc(val);
        next!()
    }
    if let (Some(a), Some(b)) = (lhs.as_number(heap), rhs.as_number(heap)) {
        let sum = a + b;
        if let Some(s) = Smi::from_f64(sum) {
            let val = s.into_tagged();
            frame.regs().set(dst, val);
            frame.set_acc(val);
            next!()
        }
        if let Some(val) = frame.ctx().heap_mut().try_new_float(sum) {
            frame.regs().set(dst, val);
            frame.set_acc(val);
            next!()
        }
        frame.set_float(sum);
        tail!(slow_box_add_loc)
    }
    tail!(slow_add_loc)
}

#[handler(SubLoc)]
pub fn op_sub_loc(frame: Frame) {
    let heap = frame.ctx().heap();
    let dst = frame.signed::<0>();
    let src = frame.signed::<1>();
    let lhs = frame.regs().get(dst, heap);
    let rhs = frame.regs().get(src, heap);
    if let (Some(a), Some(b)) = (lhs.smi_bits(), rhs.smi_bits())
        && let Some(diff) = a.checked_sub(b)
    {
        let val = Tagged::from_smi_bits(diff);
        frame.regs().set(dst, val);
        frame.set_acc(val);
        next!()
    }
    if let (Some(a), Some(b)) = (lhs.as_number(heap), rhs.as_number(heap)) {
        let diff = a - b;
        if let Some(s) = Smi::from_f64(diff) {
            let val = s.into_tagged();
            frame.regs().set(dst, val);
            frame.set_acc(val);
            next!()
        }
        if let Some(val) = frame.ctx().heap_mut().try_new_float(diff) {
            frame.regs().set(dst, val);
            frame.set_acc(val);
            next!()
        }
        frame.set_float(diff);
        tail!(slow_box_sub_loc)
    }
    tail!(slow_sub_loc)
}

// --- bitwise / shifts ---

#[handler(BitwiseOr)]
pub fn op_bitwise_or(frame: Frame) {
    let heap = frame.ctx().heap();
    let lhs = frame.regs().get(frame.signed::<0>(), heap);
    if let (Some(a), Some(b)) = (lhs.to_i64(), frame.acc().to_i64()) {
        let val = Smi::new((a as i32 | b as i32) as i64).into_tagged();
        frame.set_acc(val);
        next!()
    }
    tail!(slow_bitwise_or)
}

#[handler(BitwiseXor)]
pub fn op_bitwise_xor(frame: Frame) {
    let heap = frame.ctx().heap();
    let lhs = frame.regs().get(frame.signed::<0>(), heap);
    if let (Some(a), Some(b)) = (lhs.to_i64(), frame.acc().to_i64()) {
        let val = Smi::new((a as i32 ^ b as i32) as i64).into_tagged();
        frame.set_acc(val);
        next!()
    }
    tail!(slow_bitwise_xor)
}

#[handler(BitwiseAnd)]
pub fn op_bitwise_and(frame: Frame) {
    let heap = frame.ctx().heap();
    let lhs = frame.regs().get(frame.signed::<0>(), heap);
    if let (Some(a), Some(b)) = (lhs.to_i64(), frame.acc().to_i64()) {
        let val = Smi::new((a as i32 & b as i32) as i64).into_tagged();
        frame.set_acc(val);
        next!()
    }
    tail!(slow_bitwise_and)
}

#[handler(ShiftLeft)]
pub fn op_shift_left(frame: Frame) {
    let heap = frame.ctx().heap();
    let lhs = frame.regs().get(frame.signed::<0>(), heap);
    if let (Some(a), Some(b)) = (lhs.to_i64(), frame.acc().to_i64()) {
        let val = Smi::new((a as i32).wrapping_shl(b as u32 & 31) as i64).into_tagged();
        frame.set_acc(val);
        next!()
    }
    tail!(slow_shift_left)
}

#[handler(ShiftRight)]
pub fn op_shift_right(frame: Frame) {
    let heap = frame.ctx().heap();
    let lhs = frame.regs().get(frame.signed::<0>(), heap);
    if let (Some(a), Some(b)) = (lhs.to_i64(), frame.acc().to_i64()) {
        let val = Smi::new((a as i32).wrapping_shr(b as u32 & 31) as i64).into_tagged();
        frame.set_acc(val);
        next!()
    }
    tail!(slow_shift_right)
}

#[handler(ShiftRightLogical)]
pub fn op_shift_right_logical(frame: Frame) {
    let heap = frame.ctx().heap();
    let lhs = frame.regs().get(frame.signed::<0>(), heap);
    if let (Some(a), Some(b)) = (lhs.to_i64(), frame.acc().to_i64()) {
        let val = Smi::new((a as u32).wrapping_shr(b as u32 & 31) as i64).into_tagged();
        frame.set_acc(val);
        next!()
    }
    tail!(slow_shift_right_logical)
}

#[handler(BitwiseOrImmediate)]
pub fn op_bitwise_or_immediate(frame: Frame) {
    let heap = frame.ctx().heap();
    let lhs = frame.regs().get(frame.signed::<0>(), heap);
    let imm = frame.signed::<1>();
    if let Some(a) = lhs.to_i64() {
        let val = Smi::new((a as i32 | imm) as i64).into_tagged();
        frame.set_acc(val);
        next!()
    }
    tail!(slow_bitwise_or_immediate)
}

#[handler(BitwiseXorImmediate)]
pub fn op_bitwise_xor_immediate(frame: Frame) {
    let heap = frame.ctx().heap();
    let lhs = frame.regs().get(frame.signed::<0>(), heap);
    let imm = frame.signed::<1>();
    if let Some(a) = lhs.to_i64() {
        let val = Smi::new((a as i32 ^ imm) as i64).into_tagged();
        frame.set_acc(val);
        next!()
    }
    tail!(slow_bitwise_xor_immediate)
}

#[handler(BitwiseAndImmediate)]
pub fn op_bitwise_and_immediate(frame: Frame) {
    let heap = frame.ctx().heap();
    let lhs = frame.regs().get(frame.signed::<0>(), heap);
    let imm = frame.signed::<1>();
    if let Some(a) = lhs.to_i64() {
        let val = Smi::new((a as i32 & imm) as i64).into_tagged();
        frame.set_acc(val);
        next!()
    }
    tail!(slow_bitwise_and_immediate)
}

#[handler(ShiftLeftImmediate)]
pub fn op_shift_left_immediate(frame: Frame) {
    let heap = frame.ctx().heap();
    let lhs = frame.regs().get(frame.signed::<0>(), heap);
    let imm = frame.signed::<1>();
    if let Some(a) = lhs.to_i64() {
        let val = Smi::new((a as i32).wrapping_shl(imm as u32 & 31) as i64).into_tagged();
        frame.set_acc(val);
        next!()
    }
    tail!(slow_shift_left_immediate)
}

#[handler(ShiftRightImmediate)]
pub fn op_shift_right_immediate(frame: Frame) {
    let heap = frame.ctx().heap();
    let lhs = frame.regs().get(frame.signed::<0>(), heap);
    let imm = frame.signed::<1>();
    if let Some(a) = lhs.to_i64() {
        let val = Smi::new((a as i32).wrapping_shr(imm as u32 & 31) as i64).into_tagged();
        frame.set_acc(val);
        next!()
    }
    tail!(slow_shift_right_immediate)
}

#[handler(ShiftRightLogicalImmediate)]
pub fn op_shift_right_logical_immediate(frame: Frame) {
    let heap = frame.ctx().heap();
    let lhs = frame.regs().get(frame.signed::<0>(), heap);
    let imm = frame.signed::<1>();
    if let Some(a) = lhs.to_i64() {
        let val = Smi::new((a as u32).wrapping_shr(imm as u32 & 31) as i64).into_tagged();
        frame.set_acc(val);
        next!()
    }
    tail!(slow_shift_right_logical_immediate)
}

// --- comparisons ---

#[handler(Equal)]
pub fn op_equal(frame: Frame) {
    let heap = frame.ctx().heap();
    let other = frame.regs().get(frame.signed::<0>(), heap);
    if let (Some(a), Some(b)) = (frame.acc().smi_bits(), other.smi_bits()) {
        frame.set_acc(Convert::boolean(heap, a == b));
        next!()
    }
    tail!(slow_equal)
}

#[handler(LessThan)]
pub fn op_less_than(frame: Frame) {
    let heap = frame.ctx().heap();
    let other = frame.regs().get(frame.signed::<0>(), heap);
    if let (Some(a), Some(b)) = (frame.acc().smi_bits(), other.smi_bits()) {
        frame.set_acc(Convert::boolean(heap, a < b));
        next!()
    }
    tail!(slow_less_than)
}

#[handler(GreaterThan)]
pub fn op_greater_than(frame: Frame) {
    let heap = frame.ctx().heap();
    let other = frame.regs().get(frame.signed::<0>(), heap);
    if let (Some(a), Some(b)) = (frame.acc().smi_bits(), other.smi_bits()) {
        frame.set_acc(Convert::boolean(heap, a > b));
        next!()
    }
    tail!(slow_greater_than)
}

#[handler(LessThanOrEqual)]
pub fn op_less_than_or_equal(frame: Frame) {
    let heap = frame.ctx().heap();
    let other = frame.regs().get(frame.signed::<0>(), heap);
    if let (Some(a), Some(b)) = (frame.acc().smi_bits(), other.smi_bits()) {
        frame.set_acc(Convert::boolean(heap, a <= b));
        next!()
    }
    tail!(slow_less_than_or_equal)
}

#[handler(GreaterThanOrEqual)]
pub fn op_greater_than_or_equal(frame: Frame) {
    let heap = frame.ctx().heap();
    let other = frame.regs().get(frame.signed::<0>(), heap);
    if let (Some(a), Some(b)) = (frame.acc().smi_bits(), other.smi_bits()) {
        frame.set_acc(Convert::boolean(heap, a >= b));
        next!()
    }
    tail!(slow_greater_than_or_equal)
}

#[handler(EqualStrict)]
pub fn op_equal_strict(frame: Frame) {
    let heap = frame.ctx().heap();
    let other = frame.regs().get(frame.signed::<0>(), heap);
    frame.set_acc(Convert::boolean(
        heap,
        Compare::strict_equal(heap, frame.acc(), other),
    ));
    next!()
}

#[handler(TestReferenceEqual)]
pub fn op_test_reference_equal(frame: Frame) {
    let heap = frame.ctx().heap();
    let other = frame.regs().get(frame.signed::<0>(), heap);
    frame.set_acc(Convert::boolean(heap, other == frame.acc()));
    next!()
}

#[handler(TestTypeof)]
pub fn op_test_typeof(frame: Frame) {
    let heap = frame.ctx().heap();
    frame.set_acc(Object::type_of(heap, frame.acc()));
    next!()
}

#[handler(InstanceOf)]
pub fn op_instance_of(frame: Frame) {
    let _ = frame.signed::<0>();
    tail!(slow_instance_of)
}

#[handler(ArraySlice)]
pub fn op_array_slice(frame: Frame) {
    let from = frame.unsigned::<1>() as usize;
    let pc = frame.pc();
    let state = frame.ctx().state();
    let result = state.handle_scope(|scope| {
        let heap = frame.ctx().heap_mut();
        let value = frame.regs().get(frame.signed::<0>(), heap);
        let Some(object) = value.as_heap_object() else {
            return heap.known().the_hole.get(heap).erase();
        };
        if !object.as_ref().is_array_exotic(heap) {
            return heap.known().the_hole.get(heap).erase();
        }
        let source = scope.handle(object);
        let kind = source.as_tagged(heap).as_ref().map(heap).kind();
        let length = source.as_tagged(heap).as_ref().length();
        let count = length.saturating_sub(from);
        if kind.is_dense_elements() && !kind.is_holey() {
            let mut values: Vec<Handle<'_, Value>> = Vec::with_capacity(count);
            for index in 0..count {
                let value = source
                    .as_tagged(heap)
                    .as_ref()
                    .element_value_dense(heap, from + index)
                    .expect("packed array element");
                values.push(scope.handle(value));
            }
            let values = scope.stage_handles(&values);
            return heap.new_array(&scope, values).erase();
        }
        let result = scope.handle(heap.new_array_with_capacity(&scope, count));
        Object::set_array_length(heap, &scope, &result, count);
        for index in 0..count {
            if let Some(value) = source
                .as_tagged(heap)
                .as_ref()
                .element_value(heap, from + index)
            {
                let value = scope.handle(value);
                Object::store_array_element(heap, &scope, &result, index, &value);
            }
        }
        result.as_tagged(heap).erase()
    });
    frame.set_acc(result);
    reenter!(pc)
}

#[handler(SwitchInt)]
pub fn op_switch_int(frame: Frame) {
    let heap = frame.ctx().heap();
    let value = frame.regs().get(frame.signed::<0>(), heap);
    let table_idx = frame.unsigned::<1>();
    let switch_table = frame.ctx().constants_ref(heap).at(heap, table_idx);
    let switch_table = switch_table
        .get_as::<FixedArray>(heap)
        .expect("switch table");
    let last = switch_table.len() - 1;
    let default = Smi::decode(switch_table.at(heap, last).raw())
        .expect("switch default")
        .value();
    let number = Smi::decode(value.raw()).map(|smi| smi.value()).or_else(|| {
        let number = value.as_number(heap)?;
        if number == 0.0 {
            Some(0)
        } else {
            Smi::from_f64(number).map(|smi| smi.value())
        }
    });
    let offset = match number {
        Some(value) => {
            let min = Smi::decode(switch_table.at(heap, 0).raw())
                .expect("switch min")
                .value();
            let count = switch_table.len() - 2;
            let index = value - min;
            if index >= 0 && (index as usize) < count {
                Smi::decode(switch_table.at(heap, 1 + index as usize).raw())
                    .expect("switch target")
                    .value()
            } else {
                default
            }
        }
        None => default,
    };
    dispatch!(frame.ip().wrapping_offset(offset as isize))
}

#[handler(SwitchAtom)]
pub fn op_switch_atom(frame: Frame) {
    let heap = frame.ctx().heap();
    let value = frame.regs().get(frame.signed::<0>(), heap);
    let table_idx = frame.unsigned::<1>();
    let switch_table = frame.ctx().constants_ref(heap).at(heap, table_idx);
    let switch_table = switch_table
        .get_as::<FixedArray>(heap)
        .expect("atom switch table");
    let header = Smi::decode(switch_table.at(heap, 0).raw())
        .expect("atom switch header")
        .value();
    let atom = value.get_as::<Atom>(heap);
    let offset = if header < 0 {
        let count = Smi::decode(switch_table.at(heap, 1).raw())
            .expect("atom switch count")
            .value() as usize;
        let default = Smi::decode(switch_table.at(heap, 2).raw())
            .expect("atom switch default")
            .value();
        let Some(atom) = atom else {
            dispatch!(frame.ip().wrapping_offset(default as isize))
        };
        let id = atom.id() as i64;
        let mut lo = 0usize;
        let mut hi = count;
        let mut offset = default;
        while lo < hi {
            let mid = (lo + hi) / 2;
            let key = Smi::decode(switch_table.at(heap, 3 + mid * 2).raw())
                .expect("atom switch key")
                .value();
            if key == id {
                offset = Smi::decode(switch_table.at(heap, 4 + mid * 2).raw())
                    .expect("atom switch target")
                    .value();
                break;
            }
            if key < id {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        offset
    } else {
        let default = Smi::decode(switch_table.at(heap, switch_table.len() - 1).raw())
            .expect("atom switch default")
            .value();
        let Some(atom) = atom else {
            dispatch!(frame.ip().wrapping_offset(default as isize))
        };
        let count = switch_table.len() - 2;
        let index = atom.id() as i64 - header;
        if index >= 0 && (index as usize) < count {
            Smi::decode(switch_table.at(heap, 1 + index as usize).raw())
                .expect("atom switch target")
                .value()
        } else {
            default
        }
    };
    dispatch!(frame.ip().wrapping_offset(offset as isize))
}

#[handler(CompareJump)]
pub fn op_compare_jump(frame: Frame) {
    let heap = frame.ctx().heap();
    let other = frame.regs().get(frame.signed::<0>(), heap);
    let kind = frame.unsigned::<1>();
    let off = frame.signed::<2>();
    let cmp = (kind / 2) as u8;
    let falsy_jump = kind % 2 == 1;
    if let (Some(a), Some(b)) = (frame.acc().smi_bits(), other.smi_bits()) {
        let taken = match cmp {
            0 | 1 => a == b,
            2 => a < b,
            3 => a <= b,
            4 => a > b,
            _ => a >= b,
        };
        frame.set_acc(Convert::boolean(heap, taken));
        if taken != falsy_jump {
            dispatch!(frame.ip().wrapping_offset(off as isize))
        }
        next!()
    }
    tail!(slow_compare_jump)
}

// --- locals ---

#[handler(IncLoc)]
pub fn op_inc_loc(frame: Frame) {
    let r = frame.signed::<0>();
    if let Some(bits) = frame.regs().get(r, frame.ctx().heap()).smi_bits() {
        if let Some(new) = bits.checked_add(2) {
            frame.regs().set(r, Tagged::from_smi_bits(new));
            frame.set_acc_smi(bits);
            next!()
        }
        tail!(slow_inc_loc)
    }
    let heap = frame.ctx().heap();
    let v = frame.regs().get(r, heap);
    if let Some(n) = v.as_number(heap) {
        let new = n + 1.0;
        if let Some(s) = Smi::from_f64(new) {
            frame.regs().set(r, s.into_tagged());
            frame.set_acc(v);
            next!()
        }
        // acc keeps the old value; park it before the allocating tail
        frame.set_acc(v);
        if let Some(t) = frame.ctx().heap_mut().try_new_float(new) {
            frame.regs().set(r, t);
            next!()
        }
    }
    tail!(slow_inc_loc)
}

#[handler(DecLoc)]
pub fn op_dec_loc(frame: Frame) {
    let r = frame.signed::<0>();
    if let Some(bits) = frame.regs().get(r, frame.ctx().heap()).smi_bits() {
        if let Some(new) = bits.checked_sub(2) {
            frame.regs().set(r, Tagged::from_smi_bits(new));
            frame.set_acc_smi(bits);
            next!()
        }
        tail!(slow_dec_loc)
    }
    let heap = frame.ctx().heap();
    let v = frame.regs().get(r, heap);
    if let Some(n) = v.as_number(heap) {
        let new = n - 1.0;
        if let Some(s) = Smi::from_f64(new) {
            frame.regs().set(r, s.into_tagged());
            frame.set_acc(v);
            next!()
        }
        frame.set_acc(v);
        if let Some(t) = frame.ctx().heap_mut().try_new_float(new) {
            frame.regs().set(r, t);
            next!()
        }
    }
    tail!(slow_dec_loc)
}

#[handler(Jump)]
pub fn op_jump(frame: Frame) {
    dispatch!(frame.ip().wrapping_offset(frame.signed::<0>() as isize))
}

#[handler(JumpIfTruthy)]
pub fn op_jump_if_truthy(frame: Frame) {
    let off = frame.signed::<0>();
    if frame.acc().is_truthy(frame.ctx().heap()) {
        dispatch!(frame.ip().wrapping_offset(off as isize))
    }
    next!()
}

#[handler(JumpIfFalsy)]
pub fn op_jump_if_falsy(frame: Frame) {
    let off = frame.signed::<0>();
    if !frame.acc().is_truthy(frame.ctx().heap()) {
        dispatch!(frame.ip().wrapping_offset(off as isize))
    }
    next!()
}

#[handler(JumpIfNotUndefined)]
pub fn op_jump_if_not_undefined(frame: Frame) {
    let off = frame.signed::<0>();
    let undefined = roots.undefined.get(frame.ctx().heap()).erase();
    if frame.acc() != undefined {
        dispatch!(frame.ip().wrapping_offset(off as isize))
    }
    next!()
}

#[handler(JumpIfNullish)]
pub fn op_jump_if_nullish(frame: Frame) {
    let off = frame.signed::<0>();
    let heap = frame.ctx().heap();
    let null = roots.null.get(heap).erase();
    let undefined = roots.undefined.get(heap).erase();
    let value = frame.acc();
    if value == null || value == undefined {
        dispatch!(frame.ip().wrapping_offset(off as isize))
    }
    next!()
}

#[handler(BindThis)]
pub fn op_bind_this(frame: Frame) {
    let heap = frame.ctx().heap();
    if !frame
        .regs()
        .get(0, heap)
        .as_heap_object()
        .is_some_and(|obj| obj.map(heap).kind().kind().is_receiver())
    {
        tail!(slow_bind_this)
    }
    next!()
}

#[handler(CallNative)]
pub fn op_call_native(frame: Frame) {
    let pc = frame.pc();
    let rt = frame.unsigned::<0>();
    let base = frame.signed::<1>();
    let count = frame.unsigned::<2>();
    let args = frame
        .ctx()
        .stack()
        .window(frame.ctx().frame_base(), base, count);
    let v = frame.ctx().call_native_direct(NativeIndex(rt), args);
    frame.set_acc(v);
    reenter!(pc)
}

#[handler(CreateClosure)]
pub fn op_create_closure(frame: Frame) {
    let pc = frame.pc();
    let base = frame.ctx().frame_base();
    let info_idx = frame.unsigned::<0>();
    let vm = frame.ctx().vm();
    let state = frame.ctx().state();
    let v = state.handle_scope(|scope| -> Acc {
        let heap = frame.ctx().heap_mut();
        let Some(info) = scope.cast::<CallableInfoObject>(
            heap,
            state
                .stack()
                .frame_header(base)
                .constants(heap)
                .at(heap, info_idx),
        ) else {
            return Acc::set(Errors::raise(vm, heap, state, VmError::InvalidOperandType));
        };
        let context = scope
            .cast::<ContextObject>(
                heap,
                state.stack().frame_header(base).context_slot().get(heap),
            )
            .expect("frame context slot holds a ContextObject");
        Acc::set(Object::create_closure(heap, &scope, info, context).erase())
    });
    frame.set_acc(v.get(frame.ctx().heap()));
    reenter!(pc)
}

#[handler(CreateEmptyArrayLiteral)]
pub fn op_create_empty_array_literal(frame: Frame) {
    if let Some(v) = frame.ctx().heap_mut().try_new_array_literal() {
        frame.set_acc(v.erase());
        next!()
    }
    let pc = frame.pc();
    let state = frame.ctx().state();
    let v = state.handle_scope(|scope| {
        let heap = frame.ctx().heap_mut();
        let map = roots.js_array_map.get_handle(heap, &scope);
        heap.new_object(&scope, map, HandleSlice::EMPTY).erase()
    });
    frame.set_acc(v);
    reenter!(pc)
}

#[handler(CreateSizedArrayLiteral)]
pub fn op_create_sized_array_literal(frame: Frame) {
    let capacity = frame.unsigned::<0>();
    if let Some(v) = frame.ctx().heap_mut().try_new_array_with_capacity(capacity) {
        frame.set_acc(v.erase());
        next!()
    }
    let pc = frame.pc();
    let state = frame.ctx().state();
    let val = state.handle_scope(|scope| {
        frame
            .ctx()
            .heap_mut()
            .new_array_with_capacity(&scope, capacity)
            .erase()
    });
    frame.set_acc(val);
    reenter!(pc)
}

#[handler(CreateRestParameter)]
pub fn op_create_rest_parameter(frame: Frame) {
    let first = frame.unsigned::<0>() as usize;
    let base = frame.ctx().frame_base();
    let state = frame.ctx().state();
    let pc = frame.pc();
    let val = state.handle_scope(|scope| {
        let heap = frame.ctx().heap_mut();
        let argc = state.stack().argc(base);
        let count = argc.saturating_sub(1).saturating_sub(first);
        let mut values = Vec::with_capacity(count);
        for i in 0..count {
            values.push(scope.handle(state.stack().reg(heap, base, (first + i + 1) as i32)));
        }
        let values = scope.stage_handles(&values);
        heap.new_array(&scope, values).erase()
    });
    frame.set_acc(val);
    reenter!(pc)
}

#[handler(CreateEmptyObjectLiteral)]
pub fn op_create_empty_object_literal(frame: Frame) {
    if let Some(v) = frame.ctx().heap_mut().try_new_empty_object() {
        frame.set_acc(v.erase());
        next!()
    }
    let pc = frame.pc();
    let state = frame.ctx().state();
    let val = state.handle_scope(|scope| {
        let heap = frame.ctx().heap_mut();
        let map = roots.object_initial_map.get_handle(heap, &scope);
        heap.new_object(&scope, map, HandleSlice::EMPTY).erase()
    });
    frame.set_acc(val);
    reenter!(pc)
}

#[handler(CreateBlockContext)]
pub fn op_create_block_context(frame: Frame) {
    tail!(slow_create_block_context)
}

// --- contexts ---

#[handler(LoadContextSlot)]
pub fn op_load_context_slot(frame: Frame) {
    let slot = frame.unsigned::<0>();
    let depth = frame.unsigned::<1>();
    let base = frame.ctx().frame_base();
    let heap = frame.ctx().heap();
    if depth == 0 {
        let Some(context) = frame
            .ctx()
            .stack()
            .frame_header(base)
            .context_slot()
            .get(heap)
            .get_as::<ContextObject>(heap)
        else {
            let _ = frame.ctx().raise(VmError::InvalidOperandType);
            tail!(throw_dispatch)
        };
        let v = context
            .as_ref()
            .slots
            .get(heap)
            .as_ref()
            .element_slot(slot)
            .get(heap);
        frame.set_acc(v);
        next!()
    }
    let Some(mut context) = frame
        .ctx()
        .stack()
        .frame_header(base)
        .context_slot()
        .get(heap)
        .get_as::<ContextObject>(heap)
    else {
        let _ = frame.ctx().raise(VmError::InvalidOperandType);
        tail!(throw_dispatch)
    };
    for _ in 0..depth {
        context = match context.as_ref().outer.get(heap) {
            Some(context) => context,
            None => {
                let _ = frame.ctx().raise(VmError::InvalidOperandType);
                tail!(throw_dispatch)
            }
        };
    }
    let v = context
        .as_ref()
        .slots
        .get(heap)
        .as_ref()
        .element_slot(slot)
        .get(heap);
    frame.set_acc(v);
    next!()
}

#[handler(StoreContextSlot)]
pub fn op_store_context_slot(frame: Frame) {
    let slot = frame.unsigned::<0>();
    let depth = frame.unsigned::<1>();
    let base = frame.ctx().frame_base();
    let heap = frame.ctx().heap();
    let Some(mut context) = frame
        .ctx()
        .stack()
        .frame_header(base)
        .context_slot()
        .get(heap)
        .get_as::<ContextObject>(heap)
    else {
        let _ = frame.ctx().raise(VmError::InvalidOperandType);
        tail!(throw_dispatch)
    };
    for _ in 0..depth {
        context = match context.as_ref().outer.get(heap) {
            Some(context) => context,
            None => {
                let _ = frame.ctx().raise(VmError::InvalidOperandType);
                tail!(throw_dispatch)
            }
        };
    }
    let host = context.erase();
    let value = frame.acc();
    context
        .as_ref()
        .slots
        .get(heap)
        .as_ref()
        .element_slot(slot)
        .set(heap, host, value);
    next!()
}

#[handler(PushContext)]
pub fn op_push_context(frame: Frame) {
    let r = frame.signed::<0>();
    let base = frame.ctx().frame_base();
    let heap = frame.ctx().heap();
    let old = frame
        .ctx()
        .stack()
        .frame_header(base)
        .context_slot()
        .get(heap);
    frame.regs().set(r, old);
    if frame.acc().get_as::<ContextObject>(heap).is_none() {
        let _ = frame.ctx().raise(VmError::InvalidOperandType);
        tail!(throw_dispatch)
    }
    let value = frame.acc();
    frame
        .ctx()
        .stack()
        .frame_header(base)
        .context_slot()
        .store(value);
    next!()
}

#[handler(PopContext)]
pub fn op_pop_context(frame: Frame) {
    let r = frame.signed::<0>();
    let base = frame.ctx().frame_base();
    let context = frame.regs().get(r, frame.ctx().heap());
    if context
        .get_as::<ContextObject>(frame.ctx().heap())
        .is_none()
    {
        let _ = frame.ctx().raise(VmError::InvalidOperandType);
        tail!(throw_dispatch)
    }
    frame
        .ctx()
        .stack()
        .frame_header(base)
        .context_slot()
        .store(context);
    next!()
}

#[handler(CreateFunctionContext)]
pub fn op_create_function_context(frame: Frame) {
    // allocation is the operation and there is no faster path to tail
    // into: the whole body lives here
    let pc = frame.pc();
    let base = frame.ctx().frame_base();
    let scope_idx = frame.unsigned::<0>();
    let vm = frame.ctx().vm();
    let state = frame.ctx().state();
    let v = state.handle_scope(|scope| {
        let heap = frame.ctx().heap_mut();
        let outer = scope
            .cast::<ContextObject>(
                heap,
                state.stack().frame_header(base).context_slot().get(heap),
            )
            .expect("frame context slot holds a ContextObject");
        let Some(count) = state
            .stack()
            .frame_header(base)
            .constants(heap)
            .at(heap, scope_idx)
            .get_as::<ScopeInfo>(heap)
            .map(|r| r.local_count())
        else {
            raise!(vm, heap, state, VmError::InvalidOperandType);
        };
        let Some(scope_info) = scope.cast::<ScopeInfo>(
            heap,
            state
                .stack()
                .frame_header(base)
                .constants(heap)
                .at(heap, scope_idx),
        ) else {
            raise!(vm, heap, state, VmError::InvalidOperandType);
        };
        let slots = if count == 0 {
            roots.empty_fixed_array.get_handle(heap, &scope)
        } else {
            let values = scope.stage(&vec![roots.the_hole.get(heap).erase(); count]);
            heap.allocate_handle::<FixedArray>(values, &scope)
        };
        let ctx_obj = heap.allocate::<ContextObject>(ContextInit {
            outer: Some(outer),
            slots,
            scope_info,
        });
        ctx_obj.erase()
    });
    frame.set_acc(v);
    reenter!(pc)
}

// --- exceptions ---

#[handler(Throw)]
pub fn op_throw(frame: Frame) {
    frame.ctx().state().set_pending_exception(frame.acc());
    tail!(throw_dispatch)
}
#[handler(ThrowReferenceErrorIfHole)]
pub fn op_throw_reference_error_if_hole(frame: Frame) {
    let heap = frame.ctx().heap();
    if frame.acc() == roots.the_hole.get(heap).erase() {
        let _ = frame.ctx().raise(VmError::UninitializedVariable);
        tail!(throw_dispatch)
    }
    next!()
}
