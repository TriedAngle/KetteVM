//! The C pointer object.
//!
//! A `CPointer` is a typed handle: a C `TypeId`, a raw address, and flags.
//! It shares the `Object` prefix so ordinary property and prototype machinery
//! works on it, but the payload lives outside the GC-scanned slots:
//!
//! - `type_id` and `flags` are Smi slots (pointer-free)
//! - `address` is a raw word and is never visited
//! - `library` is a strong reference to the owning `ForeignLibrary`; it keeps
//!   the DLL object alive while the pointer is alive
//!
//! `NULL` is `address == 0`; it is a value, not a flag. `DEAD` is a separate
//! flag and makes the pointer unusable even if its bits are non-null.

use crate::ffi::TypeId;
use crate::{
    EdgeVisitable, FixedArray, ForeignLibrary, GcSlot, Header, Heap, HeapObject, ObjectKind,
    OptionGcSlot, RawCell, Smi, Tagged, Value, Visitor,
};
use core::alloc::Layout;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CPointerFlags(u8);

impl CPointerFlags {
    pub const NONE: Self = Self(0);
    pub const DEAD: Self = Self(1 << 0);
    pub const OWNED_EXTERNAL: Self = Self(1 << 1);
    pub const BORROWED: Self = Self(1 << 2);
    pub const VM_BACKED: Self = Self(1 << 3);
    pub const LIBRARY: Self = Self(1 << 4);
    pub const NUL_TERMINATED: Self = Self(1 << 5);
    /// A library handle for the current process; it is never closed.
    pub const PROCESS: Self = Self(1 << 6);

    pub const fn from_bits(bits: u8) -> Self {
        Self(bits)
    }

    pub const fn bits(self) -> u8 {
        self.0
    }

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

pub struct CPointerInit<'a> {
    pub type_id: TypeId,
    pub address: usize,
    pub flags: CPointerFlags,
    /// Owning library, or `None`.
    pub library: Option<Tagged<'a, ForeignLibrary>>,
}

#[repr(C)]
pub struct CPointer {
    pub header: Header,
    pub slots: GcSlot<FixedArray>,
    pub elements: GcSlot<Value>,
    pub length: GcSlot<Smi>,

    pub type_id: GcSlot<Smi>,
    pub address: RawCell,
    pub flags: GcSlot<Smi>,
    /// Strong reference to the owning library.
    pub library: OptionGcSlot<ForeignLibrary>,
}

impl CPointer {
    /// The owning library, if this pointer came from a loaded library.
    pub fn library<'a>(&self, heap: &'a Heap) -> Option<Tagged<'a, ForeignLibrary>> {
        self.library.get(heap)
    }

    pub fn set_library(&self, heap: &Heap, library: Tagged<'_, ForeignLibrary>) {
        self.library.set(heap, self.tagged(heap), library);
    }

    pub fn c_type(&self) -> TypeId {
        TypeId::from_i64(self.type_id.to_smi_unchecked().value())
    }

    pub fn address(&self) -> usize {
        self.address.load() as usize
    }

    pub fn set_address(&self, address: usize) {
        self.address.store_raw(address as u64);
    }

    pub fn flags(&self) -> CPointerFlags {
        CPointerFlags::from_bits(self.flags.to_smi_unchecked().value() as u8)
    }

    pub fn set_flags(&self, heap: &Heap, flags: CPointerFlags) {
        let host = self.tagged(heap);
        self.flags.set(heap, host, Smi::new(flags.bits() as i64));
    }

    pub fn is_null(&self) -> bool {
        self.address() == 0
    }

    pub fn is_dead(&self) -> bool {
        self.flags().contains(CPointerFlags::DEAD)
    }

    pub fn is_live(&self) -> bool {
        !self.is_dead()
    }

    /// Mark the pointer dead; its address is no longer usable.
    pub fn kill(&self, heap: &Heap) {
        let flags = self.flags().union(CPointerFlags::DEAD);
        self.set_flags(heap, flags);
    }
}

impl HeapObject for CPointer {
    const KIND: ObjectKind = ObjectKind::CPointer;
    type Init<'a> = CPointerInit<'a>;

    fn layout_for(_config: &Self::Init<'_>) -> Layout {
        Layout::new::<Self>()
    }

    fn init(&mut self, heap: &Heap, config: &Self::Init<'_>) {
        self.header.map.init(heap.known().c_pointer_map.get(heap));
        let empty = heap.known().empty_fixed_array.get(heap);
        self.slots.init(empty);
        self.elements.init(empty.erase());
        self.length.init_tagged(Smi::new(0).into());
        self.type_id
            .init_tagged(Smi::new(config.type_id.as_i64()).into());
        self.flags
            .init_tagged(Smi::new(config.flags.bits() as i64).into());
        self.address.store_raw(config.address as u64);
        match config.library {
            Some(library) => self.library.set(heap, self.tagged(heap), library),
            None => self.library.clear(heap),
        }
    }

    fn header(&self) -> &Header {
        &self.header
    }

    fn layout(&self) -> Layout {
        Layout::new::<Self>()
    }
}

impl EdgeVisitable for CPointer {
    fn visit_edges(&self, visitor: &mut dyn Visitor) {
        visitor.visit(self.header.map.as_raw());
        visitor.visit(self.slots.as_raw());
        visitor.visit(self.elements.as_raw());
        visitor.visit(self.library.as_raw());
    }
}
