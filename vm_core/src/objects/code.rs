use core::{alloc::Layout, cell::UnsafeCell};

use crate::{EdgeVisitable, GcSlot, Header, Heap, HeapObject, ObjectKind, Smi, Visitor};

/// A function's instruction stream plus the dispatch facts the interpreter
/// needs when it enters the frame.
///
/// The frame header stores the `Code` object rather than a bare byte
/// array, so a frame switch resolves the pc origin and the handler mode
/// from one place. The bytes are inline (like `FixedByteArray`), which
/// keeps `code_ptr()` on the frame-switch path address-only — no second
/// object to dereference.
#[repr(C)]
pub struct Code {
    pub header: Header,
    pub size: GcSlot<Smi>,
    /// Dispatch-mode id (`MODE_SLOPPY`, `MODE_STRICT`, ...).
    pub mode: GcSlot<Smi>,
    pub values: [UnsafeCell<u8>; 0],
}

pub struct CodeInit<'a> {
    pub bytes: &'a [u8],
    pub mode: u8,
}

impl Code {
    pub fn layout_for(len: usize) -> Layout {
        let values_layout = Layout::array::<u8>(len).expect("code layout");
        Layout::new::<Self>()
            .extend(values_layout)
            .expect("code layout")
            .0
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.size.to_smi_unchecked().value() as usize
    }

    #[inline]
    fn data_ptr(&self) -> *mut u8 {
        UnsafeCell::raw_get(self.values.as_ptr())
    }

    /// The first instruction byte.
    #[inline(always)]
    pub fn as_ptr(&self) -> *const u8 {
        self.data_ptr()
    }

    #[inline]
    pub fn as_slice(&self) -> &[u8] {
        // Safety: `values` has `size` initialized bytes (write-once).
        unsafe { core::slice::from_raw_parts(self.data_ptr(), self.len()) }
    }

    /// The dispatch-mode id. Never negative: ids are written from `u8`.
    #[inline(always)]
    pub fn mode(&self) -> u8 {
        self.mode.to_smi_unchecked().value() as u8
    }
}

impl HeapObject for Code {
    const KIND: ObjectKind = ObjectKind::Code;
    type Init<'a> = CodeInit<'a>;

    fn layout_for(config: &Self::Init<'_>) -> Layout {
        Self::layout_for(config.bytes.len())
    }

    fn init(&mut self, heap: &Heap, config: &Self::Init<'_>) {
        let host = self.tagged(heap);
        self.header.map.init(heap.known().code_map.as_tagged(heap));
        self.size
            .set(heap, host, Smi::new(config.bytes.len() as i64));
        self.mode.set(heap, host, Smi::new(config.mode as i64));
        // Safety: `layout_for` reserved `bytes.len()` inline bytes.
        unsafe {
            core::ptr::copy_nonoverlapping(
                config.bytes.as_ptr(),
                self.data_ptr(),
                config.bytes.len(),
            )
        };
    }

    fn header(&self) -> &Header {
        &self.header
    }

    fn layout(&self) -> Layout {
        Self::layout_for(self.len())
    }
}

impl EdgeVisitable for Code {
    fn visit_edges(&self, visitor: &mut dyn Visitor) {
        visitor.visit(self.header.map.as_raw());
        visitor.visit(self.size.as_raw());
        visitor.visit(self.mode.as_raw());
    }
}
