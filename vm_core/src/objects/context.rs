use crate::{
    DenseString, EdgeVisitable, FixedArray, GcSlot, Handle, HandleSlice, Header, Heap, HeapObject,
    Object, ObjectKind, OptionGcSlot, Smi, Tagged, Value, Visitor,
};
use core::alloc::Layout;

/// `ScopeInfo::flags` bit: the context is a variable-declaration
/// environment (`var` declarations hoist here).
pub const SCOPE_DECLARATION: i64 = 1 << 0;
/// `ScopeInfo::flags` bit: the declaration environment is a script/global
/// environment, so new top-level eval vars become global properties.
pub const SCOPE_SCRIPT: i64 = 1 << 1;
/// `ScopeInfo::flags` bit: the script context came from a REPL entry.
pub const SCOPE_REPL: i64 = 1 << 2;

pub const MODE_VAR: i64 = 0;
pub const MODE_LET: i64 = 1;
pub const MODE_CONST: i64 = 2;
pub const MODE_CLASS: i64 = 3;

pub const INFO_MODE_MASK: i64 = 0xf;
/// local belongs to the global declarative record
pub const INFO_GLOBAL: i64 = 1 << 4;

pub fn info_mode(info: i64) -> i64 {
    info & INFO_MODE_MASK
}

pub fn info_is_lexical(info: i64) -> bool {
    matches!(info_mode(info), MODE_LET | MODE_CONST | MODE_CLASS)
}

pub fn info_is_immutable(info: i64) -> bool {
    info_mode(info) == MODE_CONST
}

#[repr(C)]
pub struct ScopeInfo {
    pub header: Header,
    pub flags: GcSlot<Smi>,
    pub local_count: GcSlot<Smi>,
    // names[local_count], infos[local_count]
    pub data: [GcSlot<Value>; 0],
}

pub struct ScopeInfoInit<'a> {
    pub names: HandleSlice<'a>,
    pub infos: &'a [i64],
    pub flags: i64,
}

impl ScopeInfo {
    pub fn layout_for(local_count: usize) -> Layout {
        let data = Layout::array::<GcSlot<Value>>(local_count * 2).expect("scope info data");
        Layout::new::<Self>()
            .extend(data)
            .expect("scope info layout")
            .0
    }

    #[inline]
    fn name_slot(&self, index: usize) -> &GcSlot<DenseString> {
        debug_assert!(index < self.local_count());
        // Safety: index checked against the inline names section.
        unsafe { &*self.data.as_ptr().cast::<GcSlot<DenseString>>().add(index) }
    }

    #[inline]
    fn info_slot(&self, index: usize, count: usize) -> &GcSlot<Smi> {
        debug_assert!(index < count);
        // Safety: index checked against the inline infos section.
        unsafe { &*self.data.as_ptr().cast::<GcSlot<Smi>>().add(count + index) }
    }

    fn data_slots(&self) -> &[GcSlot] {
        let len = self.local_count() * 2;
        // Safety: the inline data holds exactly 2*local_count slots.
        unsafe { core::slice::from_raw_parts(self.data.as_ptr(), len) }
    }

    pub fn flags(&self) -> i64 {
        self.flags.to_smi_unchecked().value()
    }

    pub fn is_declaration_context(&self) -> bool {
        self.flags() & SCOPE_DECLARATION != 0
    }

    pub fn is_script_context(&self) -> bool {
        self.flags() & SCOPE_SCRIPT != 0
    }

    pub fn is_repl_context(&self) -> bool {
        self.flags() & SCOPE_REPL != 0
    }

    pub fn local_count(&self) -> usize {
        self.local_count.to_smi_unchecked().value() as usize
    }

    pub fn name<'a>(&self, heap: &'a Heap, index: usize) -> Tagged<'a, DenseString> {
        self.name_slot(index).get(heap)
    }

    pub fn info(&self, index: usize) -> i64 {
        let count = self.local_count();
        self.info_slot(index, count).to_smi_unchecked().value()
    }

    // TODO: reuse a NameToIndexHashTable for non-inlined context-local names;
    // these lookups currently scan every local linearly.
    pub fn context_slot_index(&self, heap: &Heap, name: Tagged<'_, Value>) -> Option<(usize, i64)> {
        for i in 0..self.local_count() {
            if self.name(heap, i).erase() == name {
                return Some((i, self.info(i)));
            }
        }
        None
    }

    pub fn global_slot_index(&self, heap: &Heap, name: Tagged<'_, Value>) -> Option<(usize, i64)> {
        for i in 0..self.local_count() {
            let info = self.info(i);
            if info & INFO_GLOBAL != 0 && self.name(heap, i).erase() == name {
                return Some((i, info));
            }
        }
        None
    }
}

impl HeapObject for ScopeInfo {
    const KIND: ObjectKind = ObjectKind::ScopeInfo;
    type Init<'a> = ScopeInfoInit<'a>;

    fn layout_for(config: &Self::Init<'_>) -> Layout {
        Self::layout_for(config.names.len())
    }

    fn init(&mut self, heap: &Heap, config: &Self::Init<'_>) {
        debug_assert_eq!(config.names.len(), config.infos.len());
        let host = self.tagged(heap);
        self.header.map.init(heap.known().scope_info_map.get(heap));
        self.flags.set(heap, host, Smi::new(config.flags));
        let count = config.names.len();
        self.local_count.set(heap, host, Smi::new(count as i64));
        for (i, (name, &info)) in config.names.iter().zip(config.infos).enumerate() {
            let name = name.as_tagged(heap).get_as::<DenseString>(heap).unwrap();
            self.name_slot(i).set(heap, host, name);
            self.info_slot(i, count).set(heap, host, Smi::new(info));
        }
    }

    fn header(&self) -> &Header {
        &self.header
    }

    fn layout(&self) -> Layout {
        Self::layout_for(self.local_count())
    }
}

impl EdgeVisitable for ScopeInfo {
    fn visit_edges(&self, visitor: &mut dyn Visitor) {
        visitor.visit(self.header.map.as_raw());
        visitor.visit(self.flags.as_raw());
        visitor.visit(self.local_count.as_raw());
        for slot in self.data_slots() {
            visitor.visit(slot.as_raw());
        }
    }
}

#[repr(C)]
// TODO: inline the slot array in the context instead of pointing at a
// FixedArray; the indirection costs one extra load per slot access.
pub struct ContextObject {
    pub header: Header,
    pub outer: OptionGcSlot<ContextObject>,
    pub slots: GcSlot<FixedArray>,
    pub scope_info: GcSlot<ScopeInfo>,
    /// Lazily created context-extension object for dynamically declared
    /// bindings (sloppy direct eval). The hole means no extension yet.
    pub extension: OptionGcSlot<Object>,
}

pub struct ContextInit<'a> {
    pub outer: Option<Handle<'a, ContextObject>>,
    pub slots: Handle<'a, FixedArray>,
    pub scope_info: Handle<'a, ScopeInfo>,
}

impl HeapObject for ContextObject {
    const KIND: ObjectKind = ObjectKind::ContextObject;
    type Init<'a> = ContextInit<'a>;

    fn layout_for(_config: &Self::Init<'_>) -> Layout {
        Layout::new::<Self>()
    }

    fn init(&mut self, heap: &Heap, config: &Self::Init<'_>) {
        let host = self.tagged(heap);
        self.header.map.init(heap.known().context_map.get(heap));
        match config.outer {
            Some(outer) => self.outer.set(heap, host, outer.as_tagged(heap)),
            None => self.outer.clear(heap),
        }
        self.slots.set(heap, host, config.slots.as_tagged(heap));
        self.scope_info
            .set(heap, host, config.scope_info.as_tagged(heap));
        self.extension.clear(heap);
    }

    fn header(&self) -> &Header {
        &self.header
    }

    fn layout(&self) -> Layout {
        Layout::new::<Self>()
    }
}

impl EdgeVisitable for ContextObject {
    fn visit_edges(&self, visitor: &mut dyn Visitor) {
        visitor.visit(self.header.map.as_raw());
        visitor.visit(self.outer.as_raw());
        visitor.visit(self.slots.as_raw());
        visitor.visit(self.scope_info.as_raw());
        visitor.visit(self.extension.as_raw());
    }
}

/// layout `[range_start, range_end, handler_offset]`: a half-open bytecode region
#[repr(C)]
pub struct HandlerEntry {
    pub try_start: GcSlot<Smi>,
    pub try_end: GcSlot<Smi>,
    pub handler_pc: GcSlot<Smi>,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct HandlerEntryInit {
    pub try_start: usize,
    pub try_end: usize,
    pub handler_pc: usize,
}

impl HandlerEntryInit {
    pub const fn new(try_start: usize, try_end: usize, handler_pc: usize) -> Self {
        Self {
            try_start,
            try_end,
            handler_pc,
        }
    }
}

#[repr(C)]
pub struct HandlerTable {
    pub header: Header,
    pub size: GcSlot<Smi>,
    pub entries: [HandlerEntry; 0],
}

pub struct HandlerTableInit<'a> {
    pub entries: &'a [HandlerEntryInit],
}

impl HandlerTable {
    pub fn layout_for(entry_count: usize) -> Layout {
        let entries_layout =
            Layout::array::<HandlerEntry>(entry_count).expect("handler table layout");
        Layout::new::<Self>()
            .extend(entries_layout)
            .expect("handler table layout")
            .0
    }

    pub fn len(&self) -> usize {
        self.size.to_smi().value() as usize
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn entry_ptr(&self) -> *mut HandlerEntry {
        self.entries.as_ptr() as *mut HandlerEntry
    }

    pub fn entry(&self, i: usize) -> HandlerEntryInit {
        debug_assert!(i < self.len());
        let e = unsafe { &*self.entry_ptr().add(i) };
        HandlerEntryInit {
            try_start: e.try_start.to_smi().value() as usize,
            try_end: e.try_end.to_smi().value() as usize,
            handler_pc: e.handler_pc.to_smi().value() as usize,
        }
    }

    pub fn lookup(&self, pc: usize) -> Option<usize> {
        let mut best: Option<(usize, usize)> = None;
        for i in 0..self.len() {
            let e = self.entry(i);
            if e.try_start <= pc && pc < e.try_end {
                match best {
                    Some((start, _)) if start >= e.try_start => {}
                    _ => best = Some((e.try_start, e.handler_pc)),
                }
            }
        }
        best.map(|(_, handler_pc)| handler_pc)
    }
}

impl HeapObject for HandlerTable {
    const KIND: ObjectKind = ObjectKind::HandlerTable;
    type Init<'a> = HandlerTableInit<'a>;

    fn layout_for(config: &Self::Init<'_>) -> Layout {
        Self::layout_for(config.entries.len())
    }

    fn init(&mut self, heap: &Heap, config: &Self::Init<'_>) {
        let host = self.tagged(heap);
        self.header
            .map
            .init(heap.known().handler_table_map.get(heap));
        self.size
            .set(heap, host, Smi::new(config.entries.len() as i64));
        for (i, e) in config.entries.iter().enumerate() {
            let slot = unsafe { &*self.entry_ptr().add(i) };
            slot.try_start.set(heap, host, Smi::new(e.try_start as i64));
            slot.try_end.set(heap, host, Smi::new(e.try_end as i64));
            slot.handler_pc
                .set(heap, host, Smi::new(e.handler_pc as i64));
        }
    }

    fn header(&self) -> &Header {
        &self.header
    }

    fn layout(&self) -> Layout {
        Self::layout_for(self.len())
    }
}

impl EdgeVisitable for HandlerTable {
    fn visit_edges(&self, visitor: &mut dyn Visitor) {
        visitor.visit(self.header.map.as_raw());
    }
}
