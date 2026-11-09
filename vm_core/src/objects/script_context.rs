use core::alloc::Layout;

use crate::{
    ContextObject, DenseString, EdgeVisitable, GcSlot, Handle, HandleScope, Header, Heap,
    HeapObject, INFO_GLOBAL, ObjectKind, Result, Smi, Tagged, Value, Visitor, VmError,
    info_is_immutable, info_is_lexical,
};

const INITIAL_HASH_CAPACITY: usize = 16;

#[repr(C)]
pub struct NameIndexEntry {
    pub name: GcSlot<Value>,
    pub index: GcSlot<Smi>,
}

#[repr(C)]
pub struct NameToIndexHashTable {
    pub header: Header,
    pub capacity: GcSlot<Smi>,
    pub count: GcSlot<Smi>,
    pub entries: [NameIndexEntry; 0],
}

pub struct NameToIndexHashTableInit {
    pub capacity: usize,
}

impl NameToIndexHashTable {
    pub fn layout_for(capacity: usize) -> Layout {
        let entries = Layout::array::<NameIndexEntry>(capacity).expect("name index entries");
        Layout::new::<Self>()
            .extend(entries)
            .expect("name index layout")
            .0
    }

    pub fn capacity(&self) -> usize {
        self.capacity.to_smi_unchecked().value() as usize
    }

    pub fn count(&self) -> usize {
        self.count.to_smi_unchecked().value() as usize
    }

    #[inline]
    pub fn entry(&self, index: usize) -> &NameIndexEntry {
        debug_assert!(index < self.capacity());
        // Safety: index checked against the inline entry array.
        unsafe { &*self.entries.as_ptr().add(index) }
    }

    fn slot(&self, heap: &Heap, name: Tagged<Value>) -> Option<usize> {
        let capacity = self.capacity();
        if capacity == 0 {
            return None;
        }
        let hash = name.get_as::<DenseString>(heap)?.hash(heap) as usize;
        let mut index = hash & (capacity - 1);
        for _ in 0..capacity {
            let entry = self.entry(index);
            let key = entry.name.get(heap);
            if key == heap.known().the_hole.get(heap).erase() {
                return None;
            }
            if key == name {
                return Some(index);
            }
            index = (index + 1) & (capacity - 1);
        }
        None
    }

    pub fn lookup(&self, heap: &Heap, name: Tagged<Value>) -> Option<usize> {
        let index = self.slot(heap, name)?;
        Some(self.entry(index).index.to_smi_unchecked().value() as usize)
    }

    pub fn insert(&self, heap: &Heap, host: Tagged<Value>, name: Tagged<Value>, value: usize) {
        if self.slot(heap, name).is_some() {
            return;
        }
        let capacity = self.capacity();
        debug_assert!(capacity > self.count());
        let hash = name
            .get_as::<DenseString>(heap)
            .expect("script-context names are strings")
            .hash(heap) as usize;
        let mut index = hash & (capacity - 1);
        loop {
            let entry = self.entry(index);
            if entry.name.get(heap) == heap.known().the_hole.get(heap).erase() {
                entry.name.set(heap, host, name);
                entry.index.set(heap, host, Smi::new(value as i64));
                self.count
                    .set(heap, host, Smi::new(self.count() as i64 + 1));
                return;
            }
            index = (index + 1) & (capacity - 1);
        }
    }
}

impl HeapObject for NameToIndexHashTable {
    const KIND: ObjectKind = ObjectKind::NameToIndexHashTable;
    type Init<'a> = NameToIndexHashTableInit;

    fn layout_for(config: &Self::Init<'_>) -> Layout {
        Self::layout_for(config.capacity)
    }

    fn init(&mut self, heap: &Heap, config: &Self::Init<'_>) {
        let host = self.tagged(heap);
        self.header
            .map
            .init(heap.known().name_index_table_map.get(heap));
        self.capacity
            .set(heap, host, Smi::new(config.capacity as i64));
        self.count.set(heap, host, Smi::new(0));
        let hole = heap.known().the_hole.get(heap).erase();
        for i in 0..config.capacity {
            let entry = self.entry(i);
            entry.name.set(heap, host, hole);
            entry.index.set(heap, host, Smi::new(-1));
        }
    }

    fn header(&self) -> &Header {
        &self.header
    }

    fn layout(&self) -> Layout {
        Self::layout_for(self.capacity())
    }
}

impl EdgeVisitable for NameToIndexHashTable {
    fn visit_edges(&self, visitor: &mut dyn Visitor) {
        visitor.visit(self.header.map.as_raw());
        visitor.visit(self.capacity.as_raw());
        visitor.visit(self.count.as_raw());
        for i in 0..self.capacity() {
            let entry = self.entry(i);
            visitor.visit(entry.name.as_raw());
            visitor.visit(entry.index.as_raw());
        }
    }
}

#[repr(C)]
pub struct ScriptContextTable {
    pub header: Header,
    pub capacity: GcSlot<Smi>,
    pub length: GcSlot<Smi>,
    pub names: GcSlot<NameToIndexHashTable>,
    pub contexts: [GcSlot<Value>; 0],
}

pub struct ScriptContextTableInit<'a> {
    pub capacity: usize,
    pub names: Handle<'a, NameToIndexHashTable>,
}

#[derive(Clone, Copy)]
pub struct GlobalLexicalBinding<'a> {
    pub context: Tagged<'a, ContextObject>,
    pub slot_index: usize,
    pub info: i64,
}

#[derive(Clone, Copy)]
pub struct ScriptContextLookup {
    pub context_index: usize,
    pub slot_index: usize,
    pub info: i64,
}

impl ScriptContextTable {
    pub fn layout_for(capacity: usize) -> Layout {
        let contexts = Layout::array::<GcSlot<Value>>(capacity).expect("script contexts");
        Layout::new::<Self>()
            .extend(contexts)
            .expect("script context table layout")
            .0
    }

    pub fn capacity(&self) -> usize {
        self.capacity.to_smi_unchecked().value() as usize
    }

    pub fn length(&self) -> usize {
        self.length.to_smi_unchecked().value() as usize
    }

    fn context_slot(&self, index: usize) -> &GcSlot<Value> {
        debug_assert!(index < self.capacity());
        // Safety: index checked against the inline context array.
        unsafe { &*self.contexts.as_ptr().add(index) }
    }

    pub fn context<'h>(&self, heap: &'h Heap, index: usize) -> Tagged<'h, ContextObject> {
        self.context_slot(index)
            .get(heap)
            .get_as::<ContextObject>(heap)
            .expect("script context slot holds a context")
    }

    pub fn lookup(&self, heap: &Heap, name: Tagged<Value>) -> Option<ScriptContextLookup> {
        let context_index = self.names.get(heap).lookup(heap, name)?;
        let context = self.context(heap, context_index);
        let (slot_index, info) = context.scope_info.get(heap).global_slot_index(heap, name)?;
        Some(ScriptContextLookup {
            context_index,
            slot_index,
            info,
        })
    }

    pub fn add<'s>(
        heap: &mut Heap,
        scope: &'s HandleScope<'_>,
        old: Handle<'_, ScriptContextTable>,
        context: Handle<'_, ContextObject>,
    ) -> Handle<'s, ScriptContextTable> {
        let (old_len, old_capacity) = {
            let old_ref = old.as_tagged(heap);
            (old_ref.length(), old_ref.capacity())
        };
        let old_name_count = old.as_tagged(heap).names.get(heap).count();
        let lexical: Vec<usize> = {
            let scope_info = context.as_tagged(heap).scope_info.get(heap);
            (0..scope_info.local_count())
                .filter(|&i| {
                    let info = scope_info.info(i);
                    info_is_lexical(info) && info & INFO_GLOBAL != 0
                })
                .collect()
        };
        if lexical.is_empty() {
            return scope.handle(old.as_tagged(heap));
        }

        let total = old_name_count + lexical.len();
        let mut hash_capacity = INITIAL_HASH_CAPACITY;
        while hash_capacity < total * 2 {
            hash_capacity *= 2;
        }
        let names = heap.allocate_handle::<NameToIndexHashTable>(
            NameToIndexHashTableInit {
                capacity: hash_capacity,
            },
            scope,
        );
        let names_host = names.as_tagged(heap).erase();
        {
            let old_names = old.as_tagged(heap).names.get(heap);
            for i in 0..old_names.capacity() {
                let entry = old_names.entry(i);
                let name = entry.name.get(heap);
                if name != heap.known().the_hole.get(heap).erase() {
                    names.as_tagged(heap).insert(
                        heap,
                        names_host,
                        name,
                        entry.index.to_smi_unchecked().value() as usize,
                    );
                }
            }
        }
        {
            let scope_info = context.as_tagged(heap).scope_info.get(heap);
            for &slot in &lexical {
                names.as_tagged(heap).insert(
                    heap,
                    names_host,
                    scope_info.name(heap, slot).erase(),
                    old_len,
                );
            }
        }

        let capacity = if old_len == old_capacity {
            (old_capacity * 2).max(4)
        } else {
            old_capacity
        };
        let table = heap.allocate_handle::<ScriptContextTable>(
            ScriptContextTableInit { capacity, names },
            scope,
        );
        let table_ref = table.as_tagged(heap);
        let host = table_ref.erase();
        {
            let old_ref = old.as_tagged(heap);
            for i in 0..old_len {
                table_ref
                    .context_slot(i)
                    .set(heap, host, old_ref.context_slot(i).get(heap));
            }
        }
        table_ref
            .context_slot(old_len)
            .set(heap, host, context.as_tagged(heap).erase());
        table_ref
            .length
            .set(heap, host, Smi::new((old_len + 1) as i64));
        table
    }
}

impl HeapObject for ScriptContextTable {
    const KIND: ObjectKind = ObjectKind::ScriptContextTable;
    type Init<'a> = ScriptContextTableInit<'a>;

    fn layout_for(config: &Self::Init<'_>) -> Layout {
        Self::layout_for(config.capacity)
    }

    fn init(&mut self, heap: &Heap, config: &Self::Init<'_>) {
        let host = self.tagged(heap);
        self.header
            .map
            .init(heap.known().script_context_table_map.get(heap));
        self.capacity
            .set(heap, host, Smi::new(config.capacity as i64));
        self.length.set(heap, host, Smi::new(0));
        self.names.set(heap, host, config.names.as_tagged(heap));
        let hole = heap.known().the_hole.get(heap).erase();
        for i in 0..config.capacity {
            self.context_slot(i).set(heap, host, hole);
        }
    }

    fn header(&self) -> &Header {
        &self.header
    }

    fn layout(&self) -> Layout {
        Self::layout_for(self.capacity())
    }
}

impl EdgeVisitable for ScriptContextTable {
    fn visit_edges(&self, visitor: &mut dyn Visitor) {
        visitor.visit(self.header.map.as_raw());
        visitor.visit(self.capacity.as_raw());
        visitor.visit(self.length.as_raw());
        visitor.visit(self.names.as_raw());
        for i in 0..self.capacity() {
            visitor.visit(self.context_slot(i).as_raw());
        }
    }
}

pub fn global_lexical_get<'a>(
    heap: &'a Heap,
    name: Tagged<'_, Value>,
) -> Result<Option<(Tagged<'a, Value>, GlobalLexicalBinding<'a>)>, VmError> {
    let table = heap.known().script_context_table.get(heap);
    let Some(found) = table.lookup(heap, name) else {
        return Ok(None);
    };
    let context = table.context(heap, found.context_index);
    let value = context
        .slots
        .get(heap)
        .as_ref()
        .element_slot(found.slot_index)
        .get(heap);
    if value == heap.known().the_hole.get(heap).erase() {
        return Err(VmError::UninitializedVariable);
    }
    Ok(Some((
        value,
        GlobalLexicalBinding {
            context,
            slot_index: found.slot_index,
            info: found.info,
        },
    )))
}

pub fn global_lexical_set<'a>(
    heap: &'a Heap,
    name: Tagged<'_, Value>,
    value: Tagged<'_, Value>,
) -> Result<Option<GlobalLexicalBinding<'a>>, VmError> {
    let table = heap.known().script_context_table.get(heap);
    let Some(found) = table.lookup(heap, name) else {
        return Ok(None);
    };
    let context = table.context(heap, found.context_index);
    let slot = context
        .slots
        .get(heap)
        .as_ref()
        .element_slot(found.slot_index);
    if slot.get(heap) == heap.known().the_hole.get(heap).erase() {
        return Err(VmError::UninitializedVariable);
    }
    if info_is_immutable(found.info) {
        return Err(VmError::ConstAssign);
    }
    slot.set(heap, context.erase(), value);
    Ok(Some(GlobalLexicalBinding {
        context,
        slot_index: found.slot_index,
        info: found.info,
    }))
}
