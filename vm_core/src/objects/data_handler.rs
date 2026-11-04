use core::alloc::Layout;

use crate::{
    EdgeVisitable, GcSlot, Header, Heap, HeapObject, Map, MaybeWeak, MaybeWeakGcSlot, ObjectKind,
    Smi, Tagged, Value, Visitor,
};

#[repr(C)]
pub struct DataHandler {
    pub header: Header,
    pub smi_handler: GcSlot<Smi>,
    pub validity_cell: MaybeWeakGcSlot,
    pub data: [MaybeWeakGcSlot; 0],
}

pub struct DataHandlerInit<'a> {
    pub smi_handler: Smi,
    pub validity_cell: Tagged<'a, MaybeWeak<Value>>,
    pub data: &'a [Tagged<'a, MaybeWeak<Value>>],
}

impl DataHandler {
    pub fn layout_for(data_count: usize) -> Layout {
        let data_layout = Layout::array::<MaybeWeakGcSlot>(data_count).expect("data layout");
        Layout::new::<Self>()
            .extend(data_layout)
            .expect("data handler layout")
            .0
    }

    pub fn data_len(&self) -> usize {
        // Safety: layout/visit callbacks run without a heap borrow, so
        // the map word is promoted unsafely; the map cannot move under
        // the GC callback.
        let map = unsafe { Tagged::<Map>::from_value_unchecked(self.header.map.raw()) };
        map.value_slot_count()
    }

    pub fn smi_handler<'a>(&self, heap: &'a Heap) -> Tagged<'a, Smi> {
        self.smi_handler.get(heap)
    }

    pub fn validity_cell<'a>(&self, heap: &'a Heap) -> Tagged<'a, MaybeWeak<Value>> {
        self.validity_cell.get(heap)
    }

    fn data_slots(&self) -> &[MaybeWeakGcSlot] {
        let len = self.data_len();
        // Safety: the inline data section holds exactly data_len slots.
        unsafe { core::slice::from_raw_parts(self.data.as_ptr(), len) }
    }

    fn data_slot(&self, i: usize) -> &MaybeWeakGcSlot {
        debug_assert!(i < self.data_len());
        // Safety: checked against the inline data section.
        unsafe { self.data_slots().get_unchecked(i) }
    }

    pub fn data<'a>(&self, heap: &'a Heap, i: usize) -> Tagged<'a, MaybeWeak<Value>> {
        self.data_slot(i).get(heap)
    }
}

impl HeapObject for DataHandler {
    const KIND: ObjectKind = ObjectKind::DataHandler;
    type Init<'a> = DataHandlerInit<'a>;

    fn layout_for(config: &Self::Init<'_>) -> Layout {
        Self::layout_for(config.data.len())
    }

    fn init(&mut self, heap: &Heap, config: &Self::Init<'_>) {
        let host = self.tagged(heap);
        let known = heap.known();
        let map = match config.data.len() {
            0 => known.handler_map_0.get(heap),
            1 => known.handler_map_1.get(heap),
            2 => known.handler_map_2.get(heap),
            3 => known.handler_map_3.get(heap),
            n => panic!("unsupported DataHandler data count {n}"),
        };
        self.header.map.init(map);
        self.smi_handler.set(heap, host, config.smi_handler);
        self.validity_cell.set(heap, host, config.validity_cell);
        for (i, value) in config.data.iter().enumerate() {
            self.data_slot(i).set(heap, host, *value);
        }
    }

    fn header(&self) -> &Header {
        &self.header
    }

    fn layout(&self) -> Layout {
        Self::layout_for(self.data_len())
    }
}

impl EdgeVisitable for DataHandler {
    fn visit_edges(&self, visitor: &mut dyn Visitor) {
        visitor.visit(self.header.map.as_raw());
        visitor.visit(self.validity_cell.as_raw());
        for slot in self.data_slots() {
            visitor.visit(slot.as_raw());
        }
    }
}
