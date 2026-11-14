use crate::{
    EdgeVisitable, GcSlot, Handle, HandleScope, Header, Heap, HeapObject, Key, Map, MapKind,
    ObjectKind, OptionGcSlot, PropertyDescriptor, RawCell, SlotName, Smi, Tagged, Value, Visitor,
};
use core::{alloc::Layout, cell::UnsafeCell, cmp::Ordering};

/// A String exotic own key (ES 10.4.3.4 StringGetOwnProperty): `length` or
/// an in-range code-unit index. The one-code-unit value is materialized by
/// the caller that has an allocation scope.
#[derive(Copy, Clone)]
pub enum StringOwn {
    Length(usize),
    Index(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    Latin1,
    Utf16,
    Utf8,
}

#[derive(Clone, Copy)]
pub enum StringData<'a> {
    Latin1(&'a [u8]),
    Utf16(&'a [u16]),
}

/// ES WhiteSpace or LineTerminator (7.2, 7.3).
fn is_trim_whitespace(unit: u16) -> bool {
    if (0x2000..=0x200a).contains(&unit) {
        return true;
    }
    matches!(
        unit,
        0x09 | 0x0a
            | 0x0b
            | 0x0c
            | 0x0d
            | 0x20
            | 0xa0
            | 0x1680
            | 0x2028
            | 0x2029
            | 0x202f
            | 0x205f
            | 0x3000
            | 0xfeff
    )
}

fn is_ascii_digit(unit: u16) -> bool {
    (0x30..=0x39).contains(&unit)
}

impl StringData<'_> {
    pub fn len(&self) -> usize {
        match *self {
            StringData::Latin1(b) => b.len(),
            StringData::Utf16(u) => u.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The payload encoding.
    pub fn encoding(&self) -> Encoding {
        match *self {
            StringData::Latin1(_) => Encoding::Latin1,
            StringData::Utf16(_) => Encoding::Utf16,
        }
    }

    pub fn code_unit(&self, i: usize) -> u16 {
        match *self {
            StringData::Latin1(b) => b[i] as u16,
            StringData::Utf16(u) => u[i],
        }
    }

    pub fn is_compressed(&self) -> bool {
        match *self {
            StringData::Latin1(_) => true,
            StringData::Utf16(u) => u.iter().any(|&c| c > 0xFF),
        }
    }

    pub fn matches_ascii(&self, bytes: &[u8]) -> bool {
        if self.len() != bytes.len() {
            return false;
        }
        match *self {
            StringData::Latin1(b) => b == bytes,
            StringData::Utf16(u) => u.iter().zip(bytes).all(|(c, b)| *c == *b as u16),
        }
    }

    /// First index that is not ES WhiteSpace or LineTerminator.
    pub fn trim_start_index(&self) -> usize {
        let mut i = 0;
        while i < self.len() && is_trim_whitespace(self.code_unit(i)) {
            i += 1;
        }
        i
    }

    /// End index (exclusive) of the trailing ES WhiteSpace/LineTerminator run.
    pub fn trim_end_index(&self) -> usize {
        let mut end = self.len();
        while end > 0 && is_trim_whitespace(self.code_unit(end - 1)) {
            end -= 1;
        }
        end
    }

    /// End of the longest `StrDecimalLiteral` prefix starting at `from`, or
    /// `None` when no prefix matches (ES 7.1.4.1).
    pub fn decimal_prefix_end(&self, from: usize) -> Option<usize> {
        let mut i = from;
        if i < self.len() && matches!(self.code_unit(i), 0x2b | 0x2d) {
            i += 1;
        }
        if self.starts_with_at(i, b"Infinity") {
            return Some(i + "Infinity".len());
        }
        let int_start = i;
        while i < self.len() && is_ascii_digit(self.code_unit(i)) {
            i += 1;
        }
        let int_digits = i - int_start;
        let mut frac_digits = 0;
        if i < self.len() && self.code_unit(i) == 0x2e {
            i += 1;
            let frac_start = i;
            while i < self.len() && is_ascii_digit(self.code_unit(i)) {
                i += 1;
            }
            frac_digits = i - frac_start;
        }
        if int_digits == 0 && frac_digits == 0 {
            return None;
        }
        if i < self.len() && matches!(self.code_unit(i), 0x45 | 0x65) {
            let mut end = i + 1;
            if end < self.len() && matches!(self.code_unit(end), 0x2b | 0x2d) {
                end += 1;
            }
            let exp_start = end;
            while end < self.len() && is_ascii_digit(self.code_unit(end)) {
                end += 1;
            }
            // an empty exponent is not part of the literal
            if end > exp_start {
                i = end;
            }
        }
        Some(i)
    }

    pub fn starts_with_at(&self, at: usize, prefix: &[u8]) -> bool {
        at + prefix.len() <= self.len()
            && prefix
                .iter()
                .enumerate()
                .all(|(i, &b)| self.code_unit(at + i) == b as u16)
    }

    pub fn write_units(&self, out: &mut Vec<u16>) {
        match *self {
            StringData::Latin1(b) => out.extend(b.iter().map(|&l| l as u16)),
            StringData::Utf16(u) => out.extend_from_slice(u),
        }
    }

    pub fn to_rust_string(&self) -> String {
        match *self {
            StringData::Latin1(b) => b.iter().map(|&l| l as char).collect(),
            StringData::Utf16(u) => String::from_utf16_lossy(u),
        }
    }

    pub fn hash(&self) -> i64 {
        string_content_hash(*self)
    }
}

impl PartialEq for StringData<'_> {
    fn eq(&self, other: &Self) -> bool {
        if self.len() != other.len() {
            return false;
        }
        match (*self, *other) {
            (StringData::Latin1(a), StringData::Latin1(b)) => a == b,
            (StringData::Utf16(a), StringData::Utf16(b)) => a == b,
            (StringData::Latin1(a), StringData::Utf16(b)) => {
                b.iter().zip(a.iter()).all(|(u, l)| *u == *l as u16)
            }
            (StringData::Utf16(a), StringData::Latin1(b)) => {
                a.iter().zip(b.iter()).all(|(u, l)| *u == *l as u16)
            }
        }
    }
}

impl Eq for StringData<'_> {}

impl Ord for StringData<'_> {
    fn cmp(&self, other: &Self) -> Ordering {
        let n = self.len().min(other.len());
        for i in 0..n {
            let (a, b) = (self.code_unit(i), other.code_unit(i));
            if a != b {
                return a.cmp(&b);
            }
        }
        self.len().cmp(&other.len())
    }
}

impl PartialOrd for StringData<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

// TODO: decide on a hash algorithm
pub fn string_content_hash(data: StringData<'_>) -> i64 {
    match data {
        StringData::Latin1(bytes) => string_hash_units(bytes.iter().map(|&b| b as u16)),
        StringData::Utf16(units) => string_hash_units(units.iter().copied()),
    }
}

/// Content hash for an already-decoded code-unit sequence.
pub fn string_content_hash_units(units: &[u16]) -> i64 {
    string_hash_units(units.iter().copied())
}

fn string_hash_units(units: impl IntoIterator<Item = u16>) -> i64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for unit in units {
        h ^= unit as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    let masked = (h & ((1 << 62) - 1)) as i64;
    if masked == 0 { 1 } else { masked }
}

#[repr(C)]
pub struct DenseString {
    pub header: Header,
    /// Lazy content hash over code units; Smi 0 = not yet computed.
    pub hash: GcSlot<Smi>,
    /// Length in UTF-16 code units.
    pub length: GcSlot<Smi>,
    /// Inline payload: `length` Latin-1 bytes or UTF-16 code units. (written in map)
    pub data: [UnsafeCell<u8>; 0],
}

impl<'a> Tagged<'a, Value> {
    /// Whether the value is the empty string (a non-string is not empty).
    #[inline]
    pub fn is_empty_string(self, heap: &Heap) -> bool {
        StringRef::try_from(heap, self).is_some_and(|s| s.is_empty())
    }
}

#[derive(Clone, Copy)]
pub enum StringRef<'a> {
    Flat(Tagged<'a, DenseString>),
    Utf8(Tagged<'a, Utf8String>),
    External(Tagged<'a, ExternalString>),
}

impl<'a> StringRef<'a> {
    #[inline]
    pub fn try_from(heap: &'a Heap, value: Tagged<'a, Value>) -> Option<Self> {
        if let Some(flat) = value.get_as::<DenseString>(heap) {
            return Some(Self::Flat(flat));
        }
        if let Some(utf8) = value.get_as::<Utf8String>(heap) {
            return Some(Self::Utf8(utf8));
        }
        value.get_as::<ExternalString>(heap).map(Self::External)
    }

    pub fn from_receiver(heap: &'a Heap, receiver: Tagged<'a, Value>) -> Option<Self> {
        let obj = receiver.as_heap_object()?;
        let kind = obj.map(heap).kind();
        if kind.contains(MapKind::PRIMITIVE_WRAPPER) {
            return Self::try_from(heap, obj.slots.get(heap).at(heap, 0));
        }
        Self::try_from(heap, receiver)
    }

    pub fn len(&self) -> usize {
        match *self {
            Self::Flat(s) => s.len(),
            Self::Utf8(s) => s.unit_len(),
            Self::External(s) => s.unit_len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn encoding(&self, heap: &'a Heap) -> Encoding {
        match *self {
            Self::Flat(s) => s.encoding(heap),
            Self::Utf8(_) => Encoding::Utf8,
            Self::External(s) => s.encoding(heap),
        }
    }

    pub fn is_flat(&self) -> bool {
        matches!(self, Self::Flat(_))
    }

    pub fn flat(&self) -> Option<Tagged<'a, DenseString>> {
        match *self {
            Self::Flat(s) => Some(s),
            _ => None,
        }
    }

    pub fn data(&self, heap: &'a Heap) -> Option<StringData<'a>> {
        match *self {
            Self::Flat(s) => Some(s.as_ref().data(heap)),
            _ => None,
        }
    }

    pub fn code_unit(&self, heap: &'a Heap, i: usize) -> u16 {
        match *self {
            Self::Flat(s) => s.code_unit(heap, i),
            Self::Utf8(s) => s.code_unit(heap, i),
            Self::External(s) => s.code_unit(heap, i),
        }
    }

    pub fn to_units(&self, heap: &'a Heap) -> Vec<u16> {
        match *self {
            Self::Flat(s) => {
                let mut units = Vec::with_capacity(s.len());
                s.data(heap).write_units(&mut units);
                units
            }
            Self::Utf8(s) => s.to_units(heap),
            Self::External(s) => s.to_units(heap),
        }
    }

    pub fn to_rust_string(&self, heap: &'a Heap) -> String {
        match *self {
            Self::Flat(s) => s.to_rust_string(heap),
            Self::Utf8(s) => s.to_rust_string(heap),
            Self::External(s) => s.to_rust_string(heap),
        }
    }

    pub fn content_eq(&self, heap: &'a Heap, other: Self) -> bool {
        match (*self, other) {
            (Self::Flat(a), Self::Flat(b)) => a.content_eq(heap, b),
            _ => self.to_units(heap) == other.to_units(heap),
        }
    }

    pub fn cmp(&self, heap: &'a Heap, other: Self) -> Ordering {
        match (*self, other) {
            (Self::Flat(a), Self::Flat(b)) => a.data(heap).cmp(&b.data(heap)),
            _ => self.to_units(heap).cmp(&other.to_units(heap)),
        }
    }

    /// Canonical array-index form: decimal digits, no leading zero, no
    /// overflow past `u32::MAX`-worthy lengths.
    pub fn canonical_index(&self, heap: &'a Heap) -> Option<usize> {
        let len = self.len();
        if len == 0 || len > 10 {
            return None;
        }
        if self.code_unit(heap, 0) == b'0' as u16 {
            return (len == 1).then_some(0);
        }
        let mut n: usize = 0;
        for i in 0..len {
            let c = self.code_unit(heap, i);
            if !(b'0' as u16..=b'9' as u16).contains(&c) {
                return None;
            }
            n = n.checked_mul(10)?.checked_add((c - b'0' as u16) as usize)?;
        }
        Some(n)
    }

    pub fn utf8_bytes(&self, heap: &'a Heap) -> Option<&'a [u8]> {
        match *self {
            Self::Utf8(s) => Some(s.as_ref().bytes(heap)),
            Self::Flat(s) => match s.as_ref().data(heap) {
                StringData::Latin1(bytes) if bytes.is_ascii() => Some(bytes),
                _ => None,
            },
            Self::External(s) => s.as_ref().utf8_bytes(heap),
        }
    }

    /// Classify `name` as a String exotic own key (ES 10.4.3.4): `length` or
    /// an in-range code-unit index. The interned `"length"` name makes the
    /// common check a single pointer compare.
    pub fn own_key(&self, heap: &'a Heap, name: Tagged<'a, SlotName>) -> Option<StringOwn> {
        if name.erase().ptr_eq(heap.known().strings.length.get(heap)) {
            return Some(StringOwn::Length(self.len()));
        }
        match name.erase().classify_key(heap) {
            Some(Key::Element(i)) if i < self.len() => Some(StringOwn::Index(i)),
            _ => None,
        }
    }

    /// ES 10.4.3.4 StringGetOwnProperty for a String primitive or wrapper:
    /// `"length"` and in-range indices as full descriptors. The one-unit
    /// value allocation happens after the string borrow is dropped.
    pub fn own_descriptor<'s>(
        heap: &mut Heap,
        scope: &'s HandleScope<'_>,
        receiver: Handle<'_, Value>,
        key: Handle<'_, Value>,
    ) -> Option<PropertyDescriptor<'s>> {
        let (own, unit) = {
            let s = StringRef::from_receiver(heap, receiver.as_tagged(heap))?;
            let own = s.own_key(heap, key.as_tagged(heap).as_name())?;
            let unit = match own {
                StringOwn::Index(i) => Some(s.code_unit(heap, i)),
                StringOwn::Length(_) => None,
            };
            (own, unit)
        };
        match own {
            StringOwn::Length(len) => Some(PropertyDescriptor::Data {
                value: scope.handle(Smi::new(len as i64).into_tagged()),
                writable: false,
                enumerable: false,
                configurable: false,
            }),
            StringOwn::Index(_) => {
                let unit = unit.expect("index descriptors carry their code unit");
                let value = DenseString::from_units(heap, scope, &[unit]).erase();
                Some(PropertyDescriptor::Data {
                    value,
                    writable: false,
                    enumerable: true,
                    configurable: false,
                })
            }
        }
    }

    /// The one-unit string an index load on a String primitive or wrapper
    /// yields (ES 5.4.3.1): `"ab"[1]` is "b". `None` for out-of-range keys
    /// and non-string receivers, which fall through to the ordinary path.
    pub fn index_element<'x>(
        heap: &'x mut Heap,
        scope: &HandleScope<'_>,
        receiver: Handle<'_, Value>,
        key: Handle<'_, SlotName>,
    ) -> Option<Tagged<'x, Value>> {
        let unit = {
            let heap: &Heap = heap;
            let Some(Key::Element(i)) = key.as_tagged(heap).erase().classify_key(heap) else {
                return None;
            };
            let s = StringRef::from_receiver(heap, receiver.as_tagged(heap))?;
            (i < s.len()).then(|| s.code_unit(heap, i))?
        };
        Some(
            DenseString::from_units(heap, scope, &[unit])
                .as_tagged(heap)
                .erase(),
        )
    }
}

pub fn flatten<'s>(
    heap: &mut Heap,
    scope: &'s HandleScope<'_>,
    value: Handle<'_, Value>,
) -> Option<Handle<'s, DenseString>> {
    let units = {
        let heap: &Heap = heap;
        StringRef::try_from(heap, value.as_tagged(heap))?.to_units(heap)
    };
    Some(DenseString::from_units(heap, scope, &units))
}

impl DenseString {
    fn data_ptr(&self) -> *mut u8 {
        UnsafeCell::raw_get(self.data.as_ptr())
    }

    pub fn encoding(&self, heap: &Heap) -> Encoding {
        if self.header.map.get(heap).kind().contains(MapKind::LATIN1) {
            Encoding::Latin1
        } else {
            Encoding::Utf16
        }
    }

    pub fn data<'s>(&'s self, heap: &Heap) -> StringData<'s> {
        let ptr = self.data_ptr();
        let len = self.len();
        match self.encoding(heap) {
            Encoding::Latin1 => {
                StringData::Latin1(unsafe { core::slice::from_raw_parts(ptr, len) })
            }
            Encoding::Utf16 => {
                StringData::Utf16(unsafe { core::slice::from_raw_parts(ptr.cast::<u16>(), len) })
            }
            Encoding::Utf8 => unreachable!("dense strings never hold UTF-8"),
        }
    }

    /// Payload including the trailing NUL, as `len + 1` code units.
    pub fn terminated_data<'s>(&'s self, heap: &Heap) -> StringData<'s> {
        let ptr = self.data_ptr();
        let len = self.len();
        match self.encoding(heap) {
            Encoding::Latin1 => {
                StringData::Latin1(unsafe { core::slice::from_raw_parts(ptr, len + 1) })
            }
            Encoding::Utf16 => StringData::Utf16(unsafe {
                core::slice::from_raw_parts(ptr.cast::<u16>(), len + 1)
            }),
            Encoding::Utf8 => unreachable!("dense strings never hold UTF-8"),
        }
    }

    pub fn len(&self) -> usize {
        self.length.to_smi_unchecked().value() as usize
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn hash(&self, heap: &Heap) -> i64 {
        match self.cached_hash() {
            Some(h) => h,
            None => {
                let h = string_content_hash(self.data(heap));
                self.hash.set(heap, self.tagged(heap), Smi::new(h));
                h
            }
        }
    }

    fn cached_hash(&self) -> Option<i64> {
        let h = self.hash.to_smi().value();
        (h != 0).then_some(h)
    }

    pub fn content_eq(&self, heap: &Heap, other: Tagged<'_, DenseString>) -> bool {
        if let (Some(a), Some(b)) = (self.cached_hash(), other.cached_hash())
            && a != b
        {
            return false;
        }
        self.data(heap) == other.data(heap)
    }

    pub fn code_unit(&self, heap: &Heap, i: usize) -> u16 {
        debug_assert!(i < self.len());
        match self.encoding(heap) {
            Encoding::Latin1 => unsafe { *self.data_ptr().add(i) as u16 },
            Encoding::Utf16 => unsafe { *self.data_ptr().cast::<u16>().add(i) },
            Encoding::Utf8 => unreachable!("dense strings never hold UTF-8"),
        }
    }

    pub fn to_rust_string(&self, heap: &Heap) -> String {
        self.data(heap).to_rust_string()
    }

    pub fn from_units<'s>(
        heap: &mut Heap,
        scope: &'s HandleScope<'_>,
        units: &[u16],
    ) -> Handle<'s, DenseString> {
        if units.iter().all(|&c| c <= 0xFF) {
            let bytes: Vec<u8> = units.iter().map(|&c| c as u8).collect();
            return Self::from_latin1(heap, scope, &bytes);
        }
        Self::from_data(heap, scope, StringData::Utf16(units))
    }

    pub fn from_latin1<'s>(
        heap: &mut Heap,
        scope: &'s HandleScope<'_>,
        bytes: &[u8],
    ) -> Handle<'s, DenseString> {
        Self::from_data(heap, scope, StringData::Latin1(bytes))
    }

    pub fn from_utf8<'s>(
        heap: &mut Heap,
        scope: &'s HandleScope<'_>,
        s: &str,
    ) -> Handle<'s, DenseString> {
        if s.is_ascii() {
            return Self::from_latin1(heap, scope, s.as_bytes());
        }
        let units: Vec<u16> = s.encode_utf16().collect();
        Self::from_units(heap, scope, &units)
    }

    pub fn from_data<'s>(
        heap: &mut Heap,
        scope: &'s HandleScope<'_>,
        data: StringData<'_>,
    ) -> Handle<'s, DenseString> {
        debug_assert!(
            data.is_compressed(),
            "heap strings are always in the compressed encoding"
        );
        heap.allocate_handle::<DenseString>((data, 0), scope)
    }

    pub fn concat<'s>(
        heap: &mut Heap,
        scope: &'s HandleScope<'_>,
        a: Handle<'_, Value>,
        b: Handle<'_, Value>,
    ) -> Handle<'s, DenseString> {
        let sa =
            StringRef::try_from(heap, a.as_tagged(heap)).expect("concat operand must be a string");
        let sb =
            StringRef::try_from(heap, b.as_tagged(heap)).expect("concat operand must be a string");
        let mut units = Vec::with_capacity(sa.len() + sb.len());
        let mut push = |s: StringRef<'_>| {
            if let Some(data) = s.data(heap) {
                data.write_units(&mut units);
            } else {
                units.extend(s.to_units(heap));
            }
        };
        push(sa);
        push(sb);
        Self::from_units(heap, scope, &units)
    }
}

impl HeapObject for DenseString {
    const KIND: ObjectKind = ObjectKind::DenseString;
    type Init<'a> = (StringData<'a>, i64);

    fn layout_for(config: &Self::Init<'_>) -> Layout {
        let values = match config.0 {
            StringData::Latin1(b) => Layout::array::<u8>(b.len() + 1).expect("string layout"),
            StringData::Utf16(u) => Layout::array::<u16>(u.len() + 1).expect("string layout"),
        };
        Layout::new::<Self>()
            .extend(values)
            .expect("string layout")
            .0
    }

    fn init(&mut self, heap: &Heap, config: &Self::Init<'_>) {
        debug_assert!(
            config.0.is_compressed(),
            "heap strings are always in the compressed encoding"
        );
        let host = self.tagged(heap);
        let map = match config.0.encoding() {
            Encoding::Latin1 => heap.known().dense_latin1_string_map.get(heap),
            Encoding::Utf16 => heap.known().dense_utf16_string_map.get(heap),
            Encoding::Utf8 => unreachable!("dense strings never hold UTF-8"),
        };
        self.header.map.set(heap, host, map);
        self.hash.set(heap, host, Smi::new(config.1));
        self.length.set(heap, host, Smi::new(config.0.len() as i64));
        let ptr = self.data_ptr();
        match config.0 {
            StringData::Latin1(b) => unsafe {
                core::ptr::copy_nonoverlapping(b.as_ptr(), ptr, b.len());
                *ptr.add(b.len()) = 0;
            },
            StringData::Utf16(u) => unsafe {
                core::ptr::copy_nonoverlapping(
                    u.as_ptr().cast::<u8>(),
                    ptr,
                    core::mem::size_of_val(u),
                );
                *ptr.cast::<u16>().add(u.len()) = 0;
            },
        }
    }

    fn header(&self) -> &Header {
        &self.header
    }

    fn layout(&self) -> Layout {
        // Safety: layout callbacks run without a heap borrow; only the
        // map's kind bits are read and the map cannot move during the
        // callback.
        let map = unsafe { Tagged::<Map>::from_value_unchecked(self.header.map.raw()) };
        let values = if map.kind().contains(MapKind::LATIN1) {
            Layout::array::<u8>(self.len() + 1).expect("string layout")
        } else {
            Layout::array::<u16>(self.len() + 1).expect("string layout")
        };
        Layout::new::<Self>()
            .extend(values)
            .expect("string layout")
            .0
    }
}

impl EdgeVisitable for DenseString {
    fn visit_edges(&self, visitor: &mut dyn Visitor) {
        visitor.visit(self.header.map.as_raw());
    }
}

#[repr(C)]
pub struct Utf8String {
    pub header: Header,
    /// Lazy content hash over code units; Smi 0 = not yet computed.
    pub hash: GcSlot<Smi>,
    /// Length in UTF-16 code units.
    pub unit_len: GcSlot<Smi>,
    /// Payload length in bytes, excluding the trailing NUL.
    pub byte_len: GcSlot<Smi>,
    /// Inline payload: `byte_len` UTF-8 bytes plus a NUL.
    pub data: [UnsafeCell<u8>; 0],
}

impl Utf8String {
    fn data_ptr(&self) -> *mut u8 {
        UnsafeCell::raw_get(self.data.as_ptr())
    }

    pub fn unit_len(&self) -> usize {
        self.unit_len.to_smi_unchecked().value() as usize
    }

    pub fn byte_len(&self) -> usize {
        self.byte_len.to_smi_unchecked().value() as usize
    }

    /// The payload bytes, excluding the trailing NUL.
    pub fn bytes<'s>(&'s self, _heap: &Heap) -> &'s [u8] {
        let len = self.byte_len();
        if len == 0 {
            return &[];
        }
        unsafe { core::slice::from_raw_parts(self.data_ptr(), len) }
    }

    /// The payload including the trailing NUL.
    pub fn terminated_bytes<'s>(&'s self, _heap: &Heap) -> &'s [u8] {
        unsafe { core::slice::from_raw_parts(self.data_ptr(), self.byte_len() + 1) }
    }

    pub fn code_unit(&self, heap: &Heap, i: usize) -> u16 {
        debug_assert!(i < self.unit_len());
        let text = core::str::from_utf8(self.bytes(heap)).expect("Utf8String holds valid UTF-8");
        let mut unit = 0;
        let mut buf = [0u16; 2];
        for ch in text.chars() {
            for &u in ch.encode_utf16(&mut buf).iter() {
                if unit == i {
                    return u;
                }
                unit += 1;
            }
        }
        unreachable!("code unit index out of range")
    }

    pub fn to_units(&self, heap: &Heap) -> Vec<u16> {
        core::str::from_utf8(self.bytes(heap))
            .expect("Utf8String holds valid UTF-8")
            .encode_utf16()
            .collect()
    }

    pub fn to_rust_string(&self, heap: &Heap) -> String {
        core::str::from_utf8(self.bytes(heap))
            .expect("Utf8String holds valid UTF-8")
            .to_string()
    }

    pub fn hash(&self, heap: &Heap) -> i64 {
        let cached = self.hash.to_smi().value();
        if cached != 0 {
            return cached;
        }
        let text = core::str::from_utf8(self.bytes(heap)).expect("Utf8String holds valid UTF-8");
        let hash = string_hash_units(text.encode_utf16());
        self.hash.set(heap, self.tagged(heap), Smi::new(hash));
        hash
    }

    pub fn from_str<'s>(
        heap: &mut Heap,
        scope: &'s HandleScope<'_>,
        text: &str,
    ) -> Handle<'s, Utf8String> {
        heap.allocate_handle::<Utf8String>((text.as_bytes(), 0), scope)
    }

    /// Build from bytes; `None` when they are not valid UTF-8.
    pub fn from_bytes<'s>(
        heap: &mut Heap,
        scope: &'s HandleScope<'_>,
        bytes: &[u8],
    ) -> Option<Handle<'s, Utf8String>> {
        core::str::from_utf8(bytes).ok()?;
        Some(heap.allocate_handle::<Utf8String>((bytes, 0), scope))
    }
}

impl HeapObject for Utf8String {
    const KIND: ObjectKind = ObjectKind::Utf8String;
    type Init<'a> = (&'a [u8], i64);

    fn layout_for(config: &Self::Init<'_>) -> Layout {
        let values = Layout::array::<u8>(config.0.len() + 1).expect("string layout");
        Layout::new::<Self>()
            .extend(values)
            .expect("string layout")
            .0
    }

    fn init(&mut self, heap: &Heap, config: &Self::Init<'_>) {
        let text = core::str::from_utf8(config.0).expect("Utf8String init requires valid UTF-8");
        let host = self.tagged(heap);
        self.header
            .map
            .set(heap, host, heap.known().utf8_string_map.get(heap));
        self.hash.set(heap, host, Smi::new(config.1));
        self.unit_len
            .set(heap, host, Smi::new(text.encode_utf16().count() as i64));
        self.byte_len
            .set(heap, host, Smi::new(config.0.len() as i64));
        let ptr = self.data_ptr();
        unsafe {
            core::ptr::copy_nonoverlapping(config.0.as_ptr(), ptr, config.0.len());
            *ptr.add(config.0.len()) = 0;
        }
    }

    fn header(&self) -> &Header {
        &self.header
    }

    fn layout(&self) -> Layout {
        let values = Layout::array::<u8>(self.byte_len() + 1).expect("string layout");
        Layout::new::<Self>()
            .extend(values)
            .expect("string layout")
            .0
    }
}

impl EdgeVisitable for Utf8String {
    fn visit_edges(&self, visitor: &mut dyn Visitor) {
        visitor.visit(self.header.map.as_raw());
    }
}

pub struct ExternalStringInit<'a> {
    /// Absolute address of the payload; it must stay valid while `owner` is
    /// alive and be NUL-terminated.
    pub address: usize,
    /// Payload length: bytes for Latin1/UTF-8, code units for UTF-16.
    pub len: usize,
    /// Length in UTF-16 code units.
    pub unit_len: usize,
    pub encoding: Encoding,
    /// Object keeping the payload alive (pointer, value, library, ...).
    pub owner: Option<Tagged<'a, Value>>,
}

#[repr(C)]
pub struct ExternalString {
    pub header: Header,
    /// Lazy content hash over code units; Smi 0 = not yet computed.
    pub hash: GcSlot<Smi>,
    /// Length in UTF-16 code units.
    pub unit_len: GcSlot<Smi>,
    /// Payload length in `encoding` units, excluding the terminator.
    pub len: GcSlot<Smi>,
    pub address: RawCell,
    pub owner: OptionGcSlot<Value>,
}

impl ExternalString {
    pub fn unit_len(&self) -> usize {
        self.unit_len.to_smi_unchecked().value() as usize
    }

    pub fn len(&self) -> usize {
        self.len.to_smi_unchecked().value() as usize
    }

    pub fn address(&self, _heap: &Heap) -> usize {
        self.address.load() as usize
    }

    pub fn set_address(&self, address: usize) {
        self.address.store_raw(address as u64);
    }

    pub fn owner<'a>(&self, heap: &'a Heap) -> Option<Tagged<'a, Value>> {
        self.owner.get(heap)
    }

    pub fn encoding(&self, heap: &Heap) -> Encoding {
        let kind = self.header.map.get(heap).kind();
        if kind.contains(MapKind::LATIN1) {
            Encoding::Latin1
        } else if kind.contains(MapKind::UTF8) {
            Encoding::Utf8
        } else {
            Encoding::Utf16
        }
    }

    /// The Latin-1/UTF-8 payload bytes, excluding the terminator.
    pub fn bytes<'s>(&'s self, heap: &Heap) -> &'s [u8] {
        let len = self.len();
        if len == 0 {
            return &[];
        }
        let address = self.address(heap);
        debug_assert!(address != 0, "external string without storage");
        // Safety: the owner keeps `address` live and the C string is
        // NUL-terminated; `len` was measured when the string was created.
        unsafe { core::slice::from_raw_parts(address as *const u8, len) }
    }

    /// The UTF-16 payload, excluding the terminator.
    pub fn units<'s>(&'s self, heap: &Heap) -> &'s [u16] {
        let len = self.len();
        if len == 0 {
            return &[];
        }
        let address = self.address(heap);
        debug_assert!(address != 0, "external string without storage");
        // Safety: same invariant as `bytes`; UTF-16 payloads are aligned.
        unsafe { core::slice::from_raw_parts(address as *const u16, len) }
    }

    pub fn code_unit(&self, heap: &Heap, i: usize) -> u16 {
        debug_assert!(i < self.unit_len());
        match self.encoding(heap) {
            Encoding::Latin1 => self.bytes(heap)[i] as u16,
            Encoding::Utf16 => self.units(heap)[i],
            Encoding::Utf8 => {
                let text = core::str::from_utf8(self.bytes(heap))
                    .expect("external UTF-8 string holds valid UTF-8");
                let mut unit = 0;
                let mut buf = [0u16; 2];
                for ch in text.chars() {
                    for &u in ch.encode_utf16(&mut buf).iter() {
                        if unit == i {
                            return u;
                        }
                        unit += 1;
                    }
                }
                unreachable!("code unit index out of range")
            }
        }
    }

    pub fn to_units(&self, heap: &Heap) -> Vec<u16> {
        match self.encoding(heap) {
            Encoding::Latin1 => self.bytes(heap).iter().map(|&b| b as u16).collect(),
            Encoding::Utf16 => self.units(heap).to_vec(),
            Encoding::Utf8 => core::str::from_utf8(self.bytes(heap))
                .expect("external UTF-8 string holds valid UTF-8")
                .encode_utf16()
                .collect(),
        }
    }

    pub fn to_rust_string(&self, heap: &Heap) -> String {
        match self.encoding(heap) {
            Encoding::Latin1 => self.bytes(heap).iter().map(|&b| b as char).collect(),
            Encoding::Utf16 => String::from_utf16_lossy(self.units(heap)),
            Encoding::Utf8 => String::from_utf8_lossy(self.bytes(heap)).into_owned(),
        }
    }

    /// Payload bytes when the encoding is UTF-8 or ASCII-only Latin-1.
    pub fn utf8_bytes<'s>(&'s self, heap: &Heap) -> Option<&'s [u8]> {
        match self.encoding(heap) {
            Encoding::Utf8 => Some(self.bytes(heap)),
            Encoding::Latin1 => {
                let bytes = self.bytes(heap);
                bytes.is_ascii().then_some(bytes)
            }
            Encoding::Utf16 => None,
        }
    }

    pub fn hash(&self, heap: &Heap) -> i64 {
        let cached = self.hash.to_smi().value();
        if cached != 0 {
            return cached;
        }
        let hash = string_content_hash_units(&self.to_units(heap));
        self.hash.set(heap, self.tagged(heap), Smi::new(hash));
        hash
    }
}

impl HeapObject for ExternalString {
    const KIND: ObjectKind = ObjectKind::ExternalString;
    type Init<'a> = ExternalStringInit<'a>;

    fn layout_for(_config: &Self::Init<'_>) -> Layout {
        Layout::new::<Self>()
    }

    fn init(&mut self, heap: &Heap, config: &Self::Init<'_>) {
        let host = self.tagged(heap);
        let map = match config.encoding {
            Encoding::Latin1 => heap.known().external_latin1_string_map.get(heap),
            Encoding::Utf16 => heap.known().external_utf16_string_map.get(heap),
            Encoding::Utf8 => heap.known().external_utf8_string_map.get(heap),
        };
        self.header.map.set(heap, host, map);
        self.hash.set(heap, host, Smi::new(0));
        self.unit_len
            .set(heap, host, Smi::new(config.unit_len as i64));
        self.len.set(heap, host, Smi::new(config.len as i64));
        self.address.store_raw(config.address as u64);
        match config.owner {
            Some(owner) => self.owner.set(heap, host, owner),
            None => self.owner.clear(heap),
        }
    }

    fn header(&self) -> &Header {
        &self.header
    }

    fn layout(&self) -> Layout {
        Layout::new::<Self>()
    }
}

impl EdgeVisitable for ExternalString {
    fn visit_edges(&self, visitor: &mut dyn Visitor) {
        visitor.visit(self.header.map.as_raw());
        visitor.visit(self.owner.as_raw());
    }
}

/// Decode WTF-8 (UTF-8 plus the 3-byte lone-surrogate pattern) into code units.
pub fn decode_wtf8(bytes: &[u8]) -> Option<Vec<u16>> {
    let mut units = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        let (width, mut cp): (usize, u32) = match b {
            0x00..=0x7F => (1, b as u32),
            0xC0..=0xDF => (2, (b & 0x1F) as u32),
            0xE0..=0xEF => (3, (b & 0x0F) as u32),
            0xF0..=0xF7 => (4, (b & 0x07) as u32),
            _ => return None,
        };
        if i + width > bytes.len() {
            return None;
        }
        for &cont in &bytes[i + 1..i + width] {
            if cont & 0xC0 != 0x80 {
                return None;
            }
            cp = (cp << 6) | (cont & 0x3F) as u32;
        }
        i += width;
        match width {
            // BMP scalar or a lone surrogate half: one code unit as-is
            1..=3 => units.push(cp as u16),
            // supplementary scalar: split into its surrogate pair
            _ => {
                let v = cp.checked_sub(0x1_0000)?;
                units.push((0xD800 + (v >> 10)) as u16);
                units.push((0xDC00 + (v & 0x3FF)) as u16);
            }
        }
    }
    Some(units)
}

impl StringData<'_> {
    /// Canonical array-index form: decimal digits, no leading zero, no
    /// overflow past `u32::MAX`-worthy lengths.
    pub fn canonical_index(&self) -> Option<usize> {
        if self.is_empty() || self.len() > 10 {
            return None;
        }
        if self.code_unit(0) == b'0' as u16 {
            return (self.len() == 1).then_some(0);
        }
        let mut n: usize = 0;
        for i in 0..self.len() {
            let c = self.code_unit(i);
            if !(b'0' as u16..=b'9' as u16).contains(&c) {
                return None;
            }
            n = n.checked_mul(10)?.checked_add((c - b'0' as u16) as usize)?;
        }
        Some(n)
    }
}
