use crate::{
    DenseString, EdgeVisitable, FixedArray, GcSlot, Handle, HandleScope, Header, Heap, HeapObject,
    ObjectKind, Smi, Tagged, Visitor,
};
use core::alloc::Layout;

/// A globally interned symbolic name.
#[repr(C)]
pub struct Atom {
    pub header: Header,
    /// The canonical interned name, without the leading `:`.
    pub name: GcSlot<DenseString>,
    /// Stable intern order used by atom switches.
    pub id: Smi,
}

impl Atom {
    pub fn new<'s>(
        heap: &mut Heap,
        scope: &'s HandleScope<'_>,
        name: Handle<'_, DenseString>,
        id: u32,
    ) -> Handle<'s, Atom> {
        heap.allocate_handle::<Atom>((name, id), scope)
    }

    pub fn name<'a>(&self, heap: &'a Heap) -> Tagged<'a, DenseString> {
        self.name.get(heap)
    }

    pub fn id(&self) -> u32 {
        self.id.value() as u32
    }

    /// Return the canonical atom for `name`, registering it on first use.
    pub fn intern<'s>(
        heap: &mut Heap,
        scope: &'s HandleScope<'_>,
        name: Handle<'_, DenseString>,
    ) -> Handle<'s, Atom> {
        let registry = heap.known().atom_registry.get(heap);
        for index in (0..registry.as_ref().len()).step_by(2) {
            let Some(existing) = registry
                .as_ref()
                .at(heap, index)
                .get_as::<DenseString>(heap)
            else {
                continue;
            };
            if existing.as_ref().content_eq(heap, name.as_tagged(heap)) {
                return registry
                    .as_ref()
                    .at(heap, index + 1)
                    .get_as::<Atom>(heap)
                    .map(|atom| scope.handle(atom))
                    .expect("atom registry pair");
            }
        }
        let id = (registry.as_ref().len() / 2) as u32;
        let atom = Atom::new(heap, scope, name, id);
        register_atom(heap, scope, name, atom);
        atom
    }

    pub fn intern_str<'s>(
        heap: &mut Heap,
        scope: &'s HandleScope<'_>,
        name: &str,
    ) -> Handle<'s, Atom> {
        let name = DenseString::from_utf8(heap, scope, name);
        Self::intern(heap, scope, name)
    }
}

impl HeapObject for Atom {
    const KIND: ObjectKind = ObjectKind::Atom;
    type Init<'a> = (Handle<'a, DenseString>, u32);

    fn layout_for(_config: &Self::Init<'_>) -> Layout {
        Layout::new::<Self>()
    }

    fn init(&mut self, heap: &Heap, config: &Self::Init<'_>) {
        let host = self.tagged(heap);
        self.header
            .map
            .set(heap, host, heap.known().atom_map.get(heap));
        self.name.set(heap, host, config.0.as_tagged(heap));
        self.id = Smi::new(config.1 as i64);
    }

    fn header(&self) -> &Header {
        &self.header
    }

    fn layout(&self) -> Layout {
        Layout::new::<Self>()
    }
}

impl EdgeVisitable for Atom {
    fn visit_edges(&self, visitor: &mut dyn Visitor) {
        visitor.visit(self.header.map.as_raw());
        visitor.visit(self.name.as_raw());
    }
}

fn register_atom(
    heap: &mut Heap,
    scope: &HandleScope<'_>,
    name: Handle<'_, DenseString>,
    atom: Handle<'_, Atom>,
) {
    let length = heap.known().atom_registry.get(heap).as_ref().len();
    let table: Handle<'_, FixedArray> = heap.allocate_hole_array(length + 2).as_handle(scope);
    let registry = heap.known().atom_registry.get(heap);
    for index in 0..length {
        table
            .as_tagged(heap)
            .as_ref()
            .set(heap, index, registry.as_ref().at(heap, index));
    }
    table
        .as_tagged(heap)
        .as_ref()
        .set(heap, length, name.as_tagged(heap).erase());
    table
        .as_tagged(heap)
        .as_ref()
        .set(heap, length + 1, atom.as_tagged(heap).erase());
    heap.known().atom_registry.init(table.as_tagged(heap));
}
