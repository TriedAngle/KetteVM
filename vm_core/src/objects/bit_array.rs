use crate::{
    EdgeVisitable, GcSlot, Handle, Header, Heap, HeapObject, ObjectKind, Smi, Tagged, Value,
    Visitor,
};
use core::{alloc::Layout, cell::UnsafeCell, ptr::NonNull};

/// Immutable bit string: inline bytes or a view into an inline root.
/// Bits are MSB-first.
#[repr(C)]
pub struct BitArray {
    pub header: Header,
    pub bit_offset: GcSlot<Smi>,
    pub bit_len: GcSlot<Smi>,
    /// Smi sentinel for inline values, root for views.
    pub origin: GcSlot<Value>,
    pub values: [UnsafeCell<u8>; 0],
}

pub enum BitArrayInit<'a> {
    Inline {
        bytes: &'a [u8],
        bit_len: usize,
    },
    View {
        origin: Handle<'a, BitArray>,
        bit_offset: usize,
        bit_len: usize,
    },
}

impl BitArray {
    fn inline_layout(len: usize) -> Layout {
        let values = Layout::array::<u8>(len).expect("bit array layout");
        Layout::new::<Self>()
            .extend(values)
            .expect("bit array layout")
            .0
            .pad_to_align()
    }

    #[inline(always)]
    pub fn bit_len(&self) -> usize {
        self.bit_len.to_smi().value() as usize
    }

    #[inline(always)]
    pub fn bit_offset(&self) -> usize {
        self.bit_offset.to_smi().value() as usize
    }

    #[inline(always)]
    pub fn byte_len(&self) -> usize {
        self.bit_len().div_ceil(8)
    }

    #[inline(always)]
    pub fn is_binary(&self) -> bool {
        self.bit_len() % 8 == 0
    }

    #[inline(always)]
    pub fn origin<'a>(&self, heap: &'a Heap) -> Option<Tagged<'a, BitArray>> {
        self.origin.get(heap).get_as::<BitArray>(heap)
    }

    #[inline(always)]
    pub fn is_view(&self, heap: &Heap) -> bool {
        self.origin(heap).is_some()
    }

    /// The inline root this value selects bits from.
    pub fn root<'a>(&self, heap: &'a Heap) -> Tagged<'a, BitArray> {
        let mut current = self.tagged(heap);
        while let Some(origin) = current.as_ref().origin(heap) {
            current = origin;
        }
        current
    }

    #[inline(always)]
    pub fn tagged<'a>(&self, heap: &'a Heap) -> Tagged<'a, BitArray> {
        Tagged::from_ptr(heap, NonNull::from(self))
    }

    /// Inline payload of the root. The slice covers `ceil(bit_len/8)` bytes.
    pub fn root_bytes<'a>(&self, heap: &'a Heap) -> &'a [u8] {
        let root = self.root(heap).as_ref();
        let len = root.byte_len();
        // Safety: an inline root's values hold exactly `ceil(bit_len/8)`
        // initialized bytes, and the heap borrow keeps the object in place.
        unsafe { core::slice::from_raw_parts(UnsafeCell::raw_get(root.values.as_ptr()), len) }
    }

    /// Bit `index` relative to this value.
    #[inline]
    pub fn bit_at(&self, heap: &Heap, index: usize) -> bool {
        let absolute = self.bit_offset() + index;
        let bytes = self.root_bytes(heap);
        let byte = bytes[absolute / 8];
        byte & (0x80 >> (absolute % 8)) != 0
    }

    /// Read up to 64 bits, MSB-first, relative to this value.
    pub fn read_bits(&self, heap: &Heap, start: usize, count: usize) -> u64 {
        debug_assert!(count <= 64);
        let bytes = self.root_bytes(heap);
        let mut value = 0u64;
        let mut index = self.bit_offset() + start;
        for _ in 0..count {
            let bit = bytes[index / 8] & (0x80 >> (index % 8)) != 0;
            value = (value << 1) | bit as u64;
            index += 1;
        }
        value
    }

    /// Copy `count` bits starting at `start` (relative) into fresh bytes.
    pub fn copy_bits(&self, heap: &Heap, start: usize, count: usize) -> Vec<u8> {
        let mut out = vec![0u8; count.div_ceil(8)];
        for index in 0..count {
            if self.bit_at(heap, start + index) {
                out[index / 8] |= 0x80 >> (index % 8);
            }
        }
        out
    }
}

impl HeapObject for BitArray {
    const KIND: ObjectKind = ObjectKind::BitArray;
    type Init<'a> = BitArrayInit<'a>;

    fn layout_for(config: &Self::Init<'_>) -> Layout {
        match config {
            BitArrayInit::Inline { bytes, .. } => Self::inline_layout(bytes.len()),
            BitArrayInit::View { .. } => Layout::new::<Self>(),
        }
    }

    fn init(&mut self, heap: &Heap, config: &Self::Init<'_>) {
        self.header.map.init(heap.known().bit_array_map.get(heap));
        match config {
            BitArrayInit::Inline { bytes, bit_len } => {
                self.bit_offset
                    .set(heap, self.tagged(heap).erase(), Smi::new(0));
                self.bit_len
                    .set(heap, self.tagged(heap).erase(), Smi::new(*bit_len as i64));
                self.origin
                    .set(heap, self.tagged(heap).erase(), Smi::new(0));
                let target =
                    unsafe { core::slice::from_raw_parts_mut(self.data_ptr(), bytes.len()) };
                target.copy_from_slice(bytes);
            }
            BitArrayInit::View {
                origin,
                bit_offset,
                bit_len,
            } => {
                self.bit_offset.set(
                    heap,
                    self.tagged(heap).erase(),
                    Smi::new(*bit_offset as i64),
                );
                self.bit_len
                    .set(heap, self.tagged(heap).erase(), Smi::new(*bit_len as i64));
                self.origin.set(
                    heap,
                    self.tagged(heap).erase(),
                    origin.as_tagged(heap).erase(),
                );
            }
        }
    }

    fn header(&self) -> &Header {
        &self.header
    }

    fn layout(&self) -> Layout {
        if self.origin.raw().is_strong_ptr() {
            Layout::new::<Self>()
        } else {
            Self::inline_layout(self.byte_len())
        }
    }
}

impl BitArray {
    fn data_ptr(&self) -> *mut u8 {
        UnsafeCell::raw_get(self.values.as_ptr())
    }
}

impl EdgeVisitable for BitArray {
    fn visit_edges(&self, visitor: &mut dyn Visitor) {
        visitor.visit(self.header.map.as_raw());
        visitor.visit(self.origin.as_raw());
    }
}
