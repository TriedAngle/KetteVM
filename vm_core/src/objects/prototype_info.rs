use crate::{
    AtomicGcSlot, AtomicOptionGcSlot, EdgeVisitable, GcSlot, Header, Heap, HeapObject, ObjectKind,
    Smi, Tagged, Visitor, WeakFixedArray,
};
use core::alloc::Layout;

#[repr(C)]
pub struct PrototypeInfo {
    pub header: Header,
    /// Weak dependent maps; RCU-published, `users_len` entries are claimed.
    pub prototype_users: AtomicOptionGcSlot<WeakFixedArray>,
    /// Claimed entries in `prototype_users`, written under the registry lock.
    pub users_len: GcSlot<Smi>,
    /// This map's slot in its prototype's registry, or `UNREGISTERED`.
    /// A hint only: compaction renumbers slots, so it is identity-checked.
    pub registry_slot: AtomicGcSlot<Smi>,
}

impl PrototypeInfo {
    pub const UNREGISTERED: i64 = -1;

    pub fn prototype_users<'a>(&self, heap: &'a Heap) -> Option<Tagged<'a, WeakFixedArray>> {
        self.prototype_users.load(heap)
    }

    pub fn users_len(&self) -> usize {
        self.users_len.to_smi_unchecked().value() as usize
    }

    pub fn set_users_len(&self, heap: &Heap, len: usize) {
        self.users_len
            .set(heap, self.tagged(heap), Smi::new(len as i64));
    }

    pub fn registry_slot(&self) -> i64 {
        self.registry_slot.load_smi_relaxed().value()
    }

    pub fn set_registry_slot(&self, slot: usize) {
        self.registry_slot.store_smi(Smi::new(slot as i64));
    }
}

impl HeapObject for PrototypeInfo {
    const KIND: ObjectKind = ObjectKind::PrototypeInfo;
    type Init<'a> = ();

    fn layout_for(_config: &Self::Init<'_>) -> Layout {
        Layout::new::<Self>()
    }

    fn init(&mut self, heap: &Heap, _config: &Self::Init<'_>) {
        self.header
            .map
            .init(heap.known().prototype_info_map.get(heap));
        self.prototype_users.clear(heap);
        self.users_len.set(heap, self.tagged(heap), Smi::new(0));
        self.registry_slot.store_smi(Smi::new(Self::UNREGISTERED));
    }

    fn header(&self) -> &Header {
        &self.header
    }

    fn layout(&self) -> Layout {
        Layout::new::<Self>()
    }
}

impl EdgeVisitable for PrototypeInfo {
    fn visit_edges(&self, visitor: &mut dyn Visitor) {
        visitor.visit(self.header.map.as_raw());
        visitor.visit(self.prototype_users.as_raw());
        visitor.visit(self.users_len.as_raw());
        visitor.visit(self.registry_slot.as_raw());
    }
}
