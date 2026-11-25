use vm_core::{
    Args, BitArray, BitArrayInit, CallInfo, DenseString, FixedByteArray, Float, Handle, HostCtx,
    Smi, Tagged, Value, VmError, bit_program, raise,
};

/// A bit source for construction: a `BitArray` or raw byte storage.
#[derive(Clone, Copy)]
enum Source<'a> {
    Bits(Tagged<'a, BitArray>),
    Bytes(Tagged<'a, FixedByteArray>),
}

impl<'a> Source<'a> {
    fn bit_len(self, _heap: &'a vm_core::Heap) -> usize {
        match self {
            Self::Bits(bits) => bits.as_ref().bit_len(),
            Self::Bytes(bytes) => bytes.as_ref().len() * 8,
        }
    }

    fn bit_at(self, heap: &'a vm_core::Heap, index: usize) -> bool {
        match self {
            Self::Bits(bits) => bits.as_ref().bit_at(heap, index),
            Self::Bytes(bytes) => {
                let byte = bytes.as_ref().get(index / 8);
                byte & (0x80 >> (index % 8)) != 0
            }
        }
    }
}

#[derive(Default)]
struct BitBuilder {
    bytes: Vec<u8>,
    bits: usize,
}

impl BitBuilder {
    fn push_bit(&mut self, bit: bool) {
        if self.bits % 8 == 0 {
            self.bytes.push(0);
        }
        if bit {
            let index = self.bits / 8;
            self.bytes[index] |= 0x80 >> (self.bits % 8);
        }
        self.bits += 1;
    }

    fn push_int(&mut self, value: u64, bits: usize, little: bool) {
        debug_assert!(bits <= 64);
        let mask = if bits == 64 {
            u64::MAX
        } else {
            (1u64 << bits) - 1
        };
        let mut value = value & mask;
        if little && bits % 8 == 0 && bits > 0 {
            value = swap_bytes(value, bits);
        }
        for index in (0..bits).rev() {
            self.push_bit((value >> index) & 1 != 0);
        }
    }

    fn push_source(&mut self, heap: &vm_core::Heap, source: Source<'_>, bits: usize) {
        for index in 0..bits {
            self.push_bit(source.bit_at(heap, index));
        }
    }

    fn push_float(&mut self, value: f64, bits: usize, little: bool) -> bool {
        let Some(raw) = float_bits(value, bits) else {
            return false;
        };
        self.push_int(raw, bits, little);
        true
    }

    fn push_utf8(&mut self, codepoint: u32) -> bool {
        if codepoint > 0x10ffff || (0xd800..=0xdfff).contains(&codepoint) {
            return false;
        }
        match codepoint {
            0..=0x7f => self.push_int(codepoint as u64, 8, false),
            0x80..=0x7ff => {
                self.push_int(0xc0 | (codepoint >> 6) as u64, 8, false);
                self.push_int(0x80 | (codepoint & 0x3f) as u64, 8, false);
            }
            0x800..=0xffff => {
                self.push_int(0xe0 | (codepoint >> 12) as u64, 8, false);
                self.push_int(0x80 | ((codepoint >> 6) & 0x3f) as u64, 8, false);
                self.push_int(0x80 | (codepoint & 0x3f) as u64, 8, false);
            }
            _ => {
                self.push_int(0xf0 | (codepoint >> 18) as u64, 8, false);
                self.push_int(0x80 | ((codepoint >> 12) & 0x3f) as u64, 8, false);
                self.push_int(0x80 | ((codepoint >> 6) & 0x3f) as u64, 8, false);
                self.push_int(0x80 | (codepoint & 0x3f) as u64, 8, false);
            }
        }
        true
    }

    fn push_utf16(&mut self, codepoint: u32, little: bool) -> bool {
        if codepoint > 0x10ffff || (0xd800..=0xdfff).contains(&codepoint) {
            return false;
        }
        if codepoint <= 0xffff {
            self.push_int(codepoint as u64, 16, little);
        } else {
            let value = codepoint - 0x10000;
            self.push_int(0xd800 + (value >> 10) as u64, 16, little);
            self.push_int(0xdc00 + (value & 0x3ff) as u64, 16, little);
        }
        true
    }

    fn push_utf32(&mut self, codepoint: u32, little: bool) -> bool {
        if codepoint > 0x10ffff || (0xd800..=0xdfff).contains(&codepoint) {
            return false;
        }
        self.push_int(codepoint as u64, 32, little);
        true
    }
}

fn f64_to_f16_bits(value: f64) -> u16 {
    let bits = value.to_bits();
    let sign = ((bits >> 63) & 1) as u16;
    let exponent = ((bits >> 52) & 0x7ff) as i32;
    let fraction = bits & ((1u64 << 52) - 1);
    if exponent == 0x7ff {
        let payload = if fraction == 0 { 0 } else { 0x200 };
        return (sign << 15) | 0x7c00 | payload;
    }
    let unbiased = exponent - 1023;
    if unbiased > 15 {
        return (sign << 15) | 0x7c00;
    }
    if unbiased >= -14 {
        let half_exponent = (unbiased + 15) as u16;
        let mut mantissa = (fraction >> 42) as u16;
        let round = (fraction >> 41) & 1 != 0;
        let sticky = fraction & ((1u64 << 41) - 1) != 0;
        if round && (sticky || mantissa & 1 != 0) {
            mantissa += 1;
        }
        if mantissa == 0x400 {
            let next = half_exponent + 1;
            if next >= 0x1f {
                return (sign << 15) | 0x7c00;
            }
            return (sign << 15) | (next << 10);
        }
        return (sign << 15) | (half_exponent << 10) | mantissa;
    }
    if unbiased < -24 {
        return sign << 15;
    }
    let mantissa = fraction | (1u64 << 52);
    let shift = (unbiased + 24) as u32;
    let total_shift = 52 - shift;
    let mut half = (mantissa >> total_shift) as u16;
    let round = (mantissa >> (total_shift - 1)) & 1 != 0;
    let sticky = mantissa & ((1u64 << (total_shift - 1)) - 1) != 0;
    if round && (sticky || half & 1 != 0) {
        half += 1;
    }
    (sign << 15) | half
}

fn f16_bits_to_f64(bits: u16) -> f64 {
    let sign = if bits & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exponent = ((bits >> 10) & 0x1f) as i32;
    let fraction = (bits & 0x3ff) as f64;
    let value = if exponent == 0 {
        fraction * 2f64.powi(-24)
    } else if exponent == 0x1f {
        if fraction == 0.0 {
            f64::INFINITY
        } else {
            f64::NAN
        }
    } else {
        (1.0 + fraction / 1024.0) * 2f64.powi(exponent - 15)
    };
    sign * value
}

fn float_bits(value: f64, bits: usize) -> Option<u64> {
    match bits {
        16 => Some(f64_to_f16_bits(value) as u64),
        32 => Some((value as f32).to_bits() as u64),
        64 => Some(value.to_bits()),
        _ => None,
    }
}

fn float_from_bits(raw: u64, bits: usize) -> Option<f64> {
    match bits {
        16 => Some(f16_bits_to_f64(raw as u16)),
        32 => Some(f32::from_bits(raw as u32) as f64),
        64 => Some(f64::from_bits(raw)),
        _ => None,
    }
}

fn inline<'a>(heap: &'a mut vm_core::Heap, bytes: &[u8], bit_len: usize) -> Tagged<'a, Value> {
    heap.allocate::<BitArray>(BitArrayInit::Inline { bytes, bit_len })
        .erase()
}

fn source_of<'a>(heap: &'a vm_core::Heap, value: Tagged<'a, Value>) -> Option<Source<'a>> {
    if let Some(bits) = value.get_as::<BitArray>(heap) {
        return Some(Source::Bits(bits));
    }
    value.get_as::<FixedByteArray>(heap).map(Source::Bytes)
}

fn size_arg(args: Args, heap: &vm_core::Heap, index: &mut usize) -> Option<usize> {
    let value = args.get_handle(heap, *index).as_tagged(heap).to_i64()?;
    *index += 1;
    usize::try_from(value).ok()
}

/// `BitArray.build(program, values...)`
pub fn bit_build<'a>(ctx: HostCtx<'a>, _call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let program = match args
        .get_handle(heap, 0)
        .as_tagged(heap)
        .get_as::<FixedByteArray>(heap)
    {
        Some(program) => program.as_ref().as_slice().to_vec(),
        None => raise!(vm, heap, state, VmError::InvalidOperandType),
    };
    let mut decoder = match bit_program::Decoder::new(&program) {
        Some(decoder) => decoder,
        None => raise!(vm, heap, state, VmError::InvalidOperandType),
    };
    let mut builder = BitBuilder::default();
    let mut arg = 1usize;
    while let Some(segment) = decoder.next() {
        let value = args.get_handle(heap, arg);
        arg += 1;
        if matches!(
            segment.kind,
            bit_program::KIND_UTF8 | bit_program::KIND_UTF16 | bit_program::KIND_UTF32
        ) {
            let Some(codepoint) = value.as_tagged(heap).to_i64() else {
                raise!(vm, heap, state, VmError::InvalidOperandType)
            };
            let Ok(codepoint) = u32::try_from(codepoint) else {
                raise!(vm, heap, state, VmError::InvalidOperandType)
            };
            let little = segment.flags & bit_program::FLAG_LITTLE != 0;
            let encoded = match segment.kind {
                bit_program::KIND_UTF8 => builder.push_utf8(codepoint),
                bit_program::KIND_UTF16 => builder.push_utf16(codepoint, little),
                _ => builder.push_utf32(codepoint, little),
            };
            if !encoded {
                raise!(vm, heap, state, VmError::InvalidOperandType)
            }
            continue;
        }
        let source = source_of(heap, value.as_tagged(heap));
        let bits = match segment.size_form {
            bit_program::SIZE_DEFAULT => match segment.kind {
                bit_program::KIND_INTEGER => 8 * segment.unit as usize,
                bit_program::KIND_FLOAT => 64,
                bit_program::KIND_BINARY | bit_program::KIND_BITSTRING => {
                    let Some(source) = source else {
                        raise!(vm, heap, state, VmError::InvalidOperandType)
                    };
                    let bits = source.bit_len(heap);
                    if bits % segment.unit as usize != 0 {
                        raise!(vm, heap, state, VmError::InvalidOperandType)
                    }
                    bits
                }
                _ => raise!(vm, heap, state, VmError::InvalidOperandType),
            },
            bit_program::SIZE_LITERAL => segment.size as usize * segment.unit as usize,
            bit_program::SIZE_ARG => {
                let Some(units) = size_arg(args, heap, &mut arg) else {
                    raise!(vm, heap, state, VmError::InvalidOperandType)
                };
                units * segment.unit as usize
            }
            _ => raise!(vm, heap, state, VmError::InvalidOperandType),
        };
        match segment.kind {
            bit_program::KIND_FLOAT => {
                let Some(value) = value.as_tagged(heap).as_number(heap) else {
                    raise!(vm, heap, state, VmError::InvalidOperandType)
                };
                if !builder.push_float(value, bits, segment.flags & bit_program::FLAG_LITTLE != 0) {
                    raise!(vm, heap, state, VmError::InvalidOperandType)
                }
            }
            bit_program::KIND_INTEGER => {
                let Some(value) = value.as_tagged(heap).to_i64() else {
                    raise!(vm, heap, state, VmError::InvalidOperandType)
                };
                if bits > 64 {
                    raise!(vm, heap, state, VmError::IntegerOverflow)
                }
                if segment.flags & bit_program::FLAG_LITTLE != 0 && bits % 8 != 0 {
                    raise!(vm, heap, state, VmError::InvalidOperandType)
                }
                builder.push_int(
                    value as u64,
                    bits,
                    segment.flags & bit_program::FLAG_LITTLE != 0,
                );
            }
            bit_program::KIND_BINARY | bit_program::KIND_BITSTRING => {
                let Some(source) = source else {
                    raise!(vm, heap, state, VmError::InvalidOperandType)
                };
                if bits > source.bit_len(heap) {
                    raise!(vm, heap, state, VmError::InvalidOperandType)
                }
                if segment.kind == bit_program::KIND_BINARY && bits % 8 != 0 {
                    raise!(vm, heap, state, VmError::InvalidOperandType)
                }
                builder.push_source(heap, source, bits);
            }
            _ => raise!(vm, heap, state, VmError::InvalidOperandType),
        }
    }
    inline(heap, &builder.bytes, builder.bits)
}

fn capture_value(heap: &vm_core::Heap, captures: &[Handle<'_, Value>], index: u32) -> Option<i64> {
    captures
        .get(index as usize)
        .and_then(|value| value.as_tagged(heap).to_i64())
}

fn read_byte(subject: Handle<'_, BitArray>, heap: &vm_core::Heap, bit: usize) -> u8 {
    subject.as_tagged(heap).as_ref().read_bits(heap, bit, 8) as u8
}

fn decode_utf8(
    subject: Handle<'_, BitArray>,
    heap: &vm_core::Heap,
    cursor: usize,
    bit_len: usize,
) -> Option<(u32, usize)> {
    if cursor + 8 > bit_len {
        return None;
    }
    let first = read_byte(subject, heap, cursor);
    let (len, mut codepoint) = match first {
        0x00..=0x7f => (1, first as u32),
        0xc2..=0xdf => (2, (first & 0x1f) as u32),
        0xe0..=0xef => (3, (first & 0x0f) as u32),
        0xf0..=0xf4 => (4, (first & 0x07) as u32),
        _ => return None,
    };
    let consumed = len * 8;
    if cursor + consumed > bit_len {
        return None;
    }
    for index in 1..len {
        let byte = read_byte(subject, heap, cursor + index * 8);
        if byte & 0xc0 != 0x80 {
            return None;
        }
        codepoint = (codepoint << 6) | (byte & 0x3f) as u32;
    }
    if codepoint > 0x10ffff || (0xd800..=0xdfff).contains(&codepoint) {
        return None;
    }
    if (len == 2 && codepoint < 0x80)
        || (len == 3 && codepoint < 0x800)
        || (len == 4 && codepoint < 0x10000)
    {
        return None;
    }
    Some((codepoint, consumed))
}

fn decode_utf16(
    subject: Handle<'_, BitArray>,
    heap: &vm_core::Heap,
    cursor: usize,
    bit_len: usize,
    little: bool,
) -> Option<(u32, usize)> {
    if cursor + 16 > bit_len {
        return None;
    }
    let mut first = subject.as_tagged(heap).as_ref().read_bits(heap, cursor, 16) as u16;
    if little {
        first = first.swap_bytes();
    }
    if (0xd800..=0xdbff).contains(&first) {
        if cursor + 32 > bit_len {
            return None;
        }
        let mut second = subject
            .as_tagged(heap)
            .as_ref()
            .read_bits(heap, cursor + 16, 16) as u16;
        if little {
            second = second.swap_bytes();
        }
        if !(0xdc00..=0xdfff).contains(&second) {
            return None;
        }
        let codepoint = 0x10000 + (((first as u32 - 0xd800) << 10) | (second as u32 - 0xdc00));
        Some((codepoint, 32))
    } else if (0xdc00..=0xdfff).contains(&first) {
        None
    } else {
        Some((first as u32, 16))
    }
}

fn decode_utf32(
    subject: Handle<'_, BitArray>,
    heap: &vm_core::Heap,
    cursor: usize,
    bit_len: usize,
    little: bool,
) -> Option<(u32, usize)> {
    if cursor + 32 > bit_len {
        return None;
    }
    let mut codepoint = subject.as_tagged(heap).as_ref().read_bits(heap, cursor, 32) as u32;
    if little {
        codepoint = codepoint.swap_bytes();
    }
    if codepoint > 0x10ffff || (0xd800..=0xdfff).contains(&codepoint) {
        return None;
    }
    Some((codepoint, 32))
}

/// `BitArray.match(subject, program)` -> capture array or null.
pub fn bit_match<'a>(ctx: HostCtx<'a>, _call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let program = match args
        .get_handle(heap, 1)
        .as_tagged(heap)
        .get_as::<FixedByteArray>(heap)
    {
        Some(program) => program.as_ref().as_slice().to_vec(),
        None => raise!(vm, heap, state, VmError::InvalidOperandType),
    };
    let mut decoder = match bit_program::Decoder::new(&program) {
        Some(decoder) => decoder,
        None => raise!(vm, heap, state, VmError::InvalidOperandType),
    };
    state.handle_scope(move |scope| {
        let heap = &mut *heap;
        let handle = args.get_handle(heap, 0);
        let Some(subject) = handle.as_tagged(heap).get_as::<BitArray>(heap) else {
            return heap.known().null.get(heap).erase();
        };
        let subject = scope.handle(subject);
        let bit_len = subject.as_tagged(heap).as_ref().bit_len();
        let mut captures: Vec<Handle<'_, Value>> = Vec::new();
        let mut cursor = 0usize;
        while let Some(segment) = decoder.next() {
            if matches!(
                segment.kind,
                bit_program::KIND_UTF8 | bit_program::KIND_UTF16 | bit_program::KIND_UTF32
            ) {
                let little = segment.flags & bit_program::FLAG_LITTLE != 0;
                let decoded = match segment.kind {
                    bit_program::KIND_UTF8 => decode_utf8(subject, heap, cursor, bit_len),
                    bit_program::KIND_UTF16 => decode_utf16(subject, heap, cursor, bit_len, little),
                    _ => decode_utf32(subject, heap, cursor, bit_len, little),
                };
                let Some((codepoint, consumed)) = decoded else {
                    return heap.known().null.get(heap).erase();
                };
                if segment.flags & bit_program::FLAG_CAPTURE != 0 {
                    captures.push(scope.handle(Smi::new(codepoint as i64)));
                }
                cursor += consumed;
                continue;
            }
            let bits = match segment.size_form {
                bit_program::SIZE_DEFAULT => match segment.kind {
                    bit_program::KIND_INTEGER => 8 * segment.unit as usize,
                    bit_program::KIND_FLOAT => 64,
                    bit_program::KIND_BINARY | bit_program::KIND_BITSTRING => {
                        let remaining = bit_len.saturating_sub(cursor);
                        if remaining % segment.unit as usize != 0 {
                            return heap.known().null.get(heap).erase();
                        }
                        remaining
                    }
                    _ => return heap.known().null.get(heap).erase(),
                },
                bit_program::SIZE_LITERAL => segment.size as usize * segment.unit as usize,
                bit_program::SIZE_CAPTURE => {
                    let Some(value) = capture_value(heap, &captures, segment.size) else {
                        return heap.known().null.get(heap).erase();
                    };
                    if value < 0 {
                        return heap.known().null.get(heap).erase();
                    }
                    value as usize * segment.unit as usize
                }
                bit_program::SIZE_ARG => {
                    let Some(value) = args
                        .get_handle(heap, segment.size as usize)
                        .as_tagged(heap)
                        .to_i64()
                    else {
                        return heap.known().null.get(heap).erase();
                    };
                    if value < 0 {
                        return heap.known().null.get(heap).erase();
                    }
                    value as usize * segment.unit as usize
                }
                _ => return heap.known().null.get(heap).erase(),
            };
            if cursor + bits > bit_len {
                return heap.known().null.get(heap).erase();
            }
            match segment.kind {
                bit_program::KIND_FLOAT => {
                    if !matches!(bits, 16 | 32 | 64) {
                        return heap.known().null.get(heap).erase();
                    }
                    let mut raw = subject
                        .as_tagged(heap)
                        .as_ref()
                        .read_bits(heap, cursor, bits);
                    if segment.flags & bit_program::FLAG_LITTLE != 0 {
                        if bits % 8 != 0 {
                            return heap.known().null.get(heap).erase();
                        }
                        raw = swap_bytes(raw, bits);
                    }
                    if segment.flags & bit_program::FLAG_FLOAT_LITERAL != 0 {
                        let literal = f64::from_bits(segment.literal);
                        if float_bits(literal, bits) != Some(raw) {
                            return heap.known().null.get(heap).erase();
                        }
                    } else if segment.flags & bit_program::FLAG_LITERAL != 0
                        && raw != segment.literal
                    {
                        return heap.known().null.get(heap).erase();
                    }
                    if segment.flags & bit_program::FLAG_CAPTURE != 0 {
                        let Some(value) = float_from_bits(raw, bits) else {
                            return heap.known().null.get(heap).erase();
                        };
                        let value = heap.allocate::<Float>(value);
                        captures.push(scope.handle(value.erase()));
                    }
                }
                bit_program::KIND_INTEGER => {
                    if bits > 64 {
                        return heap.known().null.get(heap).erase();
                    }
                    let raw = subject
                        .as_tagged(heap)
                        .as_ref()
                        .read_bits(heap, cursor, bits);
                    let raw = if segment.flags & bit_program::FLAG_LITTLE != 0 {
                        if bits % 8 != 0 {
                            return heap.known().null.get(heap).erase();
                        }
                        swap_bytes(raw, bits)
                    } else {
                        raw
                    };
                    let mut value = raw;
                    if segment.flags & bit_program::FLAG_SIGNED != 0 && bits > 0 && bits < 64 {
                        let sign = 1u64 << (bits - 1);
                        if raw & sign != 0 {
                            value = raw | (!0u64 << bits);
                        }
                    }
                    let mask = if bits == 64 {
                        u64::MAX
                    } else {
                        (1u64 << bits) - 1
                    };
                    if segment.flags & bit_program::FLAG_LITERAL != 0
                        && (raw & mask) != (segment.literal & mask)
                    {
                        return heap.known().null.get(heap).erase();
                    }
                    if segment.flags & bit_program::FLAG_CAPTURE != 0 {
                        if !Smi::in_range(value as i64) {
                            return heap.known().null.get(heap).erase();
                        }
                        captures.push(scope.handle(Smi::new(value as i64)));
                    }
                }
                bit_program::KIND_BINARY | bit_program::KIND_BITSTRING => {
                    if segment.kind == bit_program::KIND_BINARY && bits % 8 != 0 {
                        return heap.known().null.get(heap).erase();
                    }
                    if segment.flags & bit_program::FLAG_CAPTURE != 0 {
                        let origin = {
                            let root = subject.as_tagged(heap).as_ref().root(heap);
                            scope.handle(root)
                        };
                        let offset = subject.as_tagged(heap).as_ref().bit_offset() + cursor;
                        let value = heap.allocate::<BitArray>(BitArrayInit::View {
                            origin,
                            bit_offset: offset,
                            bit_len: bits,
                        });
                        captures.push(scope.handle(value.erase()));
                    }
                }
                _ => return heap.known().null.get(heap).erase(),
            }
            cursor += bits;
        }
        if cursor != bit_len {
            return heap.known().null.get(heap).erase();
        }
        let staged = scope.stage_handles(&captures);
        heap.new_array(&scope, staged).erase()
    })
}

fn swap_bytes(value: u64, bits: usize) -> u64 {
    let bytes = bits / 8;
    let mut swapped = 0u64;
    for index in 0..bytes {
        swapped |= ((value >> (index * 8)) & 0xff) << ((bytes - 1 - index) * 8);
    }
    swapped
}

pub fn bit_size<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, _args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let Some(bits) = call.receiver.as_tagged(heap).get_as::<BitArray>(heap) else {
        raise!(vm, heap, state, VmError::InvalidOperandType)
    };
    Smi::new(bits.as_ref().bit_len() as i64).into_tagged()
}

pub fn byte_size<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, _args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let Some(bits) = call.receiver.as_tagged(heap).get_as::<BitArray>(heap) else {
        raise!(vm, heap, state, VmError::InvalidOperandType)
    };
    if !bits.as_ref().is_binary() {
        raise!(vm, heap, state, VmError::InvalidOperandType)
    }
    Smi::new(bits.as_ref().byte_len() as i64).into_tagged()
}

pub fn to_string<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, _args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    state.handle_scope(|scope| {
        let Some(bits) = call.receiver.as_tagged(heap).get_as::<BitArray>(heap) else {
            raise!(vm, heap, state, VmError::InvalidOperandType)
        };
        let bit_len = bits.as_ref().bit_len();
        let bytes = bits.as_ref().copy_bits(heap, 0, bit_len);
        let mut text = String::from("<<");
        let full = bit_len / 8;
        for index in 0..full {
            if index > 0 {
                text.push_str(", ");
            }
            text.push_str(&bytes[index].to_string());
        }
        let tail = bit_len % 8;
        if tail != 0 {
            if full > 0 {
                text.push_str(", ");
            }
            text.push_str(&(bytes[full] >> (8 - tail)).to_string());
            text.push(':');
            text.push_str(&tail.to_string());
        }
        text.push_str(">>");
        let string = DenseString::from_utf8(heap, &scope, &text);
        string.as_tagged(heap).erase()
    })
}

pub fn is_binary<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, _args: Args) -> Tagged<'a, Value> {
    let HostCtx { heap, .. } = ctx;
    let binary = call
        .receiver
        .as_tagged(heap)
        .get_as::<BitArray>(heap)
        .is_some_and(|bits| bits.as_ref().is_binary());
    if binary {
        heap.known().true_object.get(heap).erase()
    } else {
        heap.known().false_object.get(heap).erase()
    }
}

pub fn bit_at<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let Some(bits) = call.receiver.as_tagged(heap).get_as::<BitArray>(heap) else {
        raise!(vm, heap, state, VmError::InvalidOperandType)
    };
    let Some(index) = args.get(heap, 0).to_i64() else {
        raise!(vm, heap, state, VmError::InvalidOperandType)
    };
    if index < 0 || index as usize >= bits.as_ref().bit_len() {
        raise!(vm, heap, state, VmError::InvalidOperandType)
    }
    if bits.as_ref().bit_at(heap, index as usize) {
        heap.known().true_object.get(heap).erase()
    } else {
        heap.known().false_object.get(heap).erase()
    }
}

pub fn slice<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    state.handle_scope(move |scope| {
        let heap = &mut *heap;
        let Some(bits) = call.receiver.as_tagged(heap).get_as::<BitArray>(heap) else {
            raise!(vm, heap, state, VmError::InvalidOperandType);
        };
        let bits = scope.handle(bits);
        let (Some(start), Some(end)) = (args.get(heap, 0).to_i64(), args.get(heap, 1).to_i64())
        else {
            raise!(vm, heap, state, VmError::InvalidOperandType);
        };
        if start < 0 || end < start || end as usize > bits.as_tagged(heap).as_ref().bit_len() {
            raise!(vm, heap, state, VmError::InvalidOperandType);
        }
        let origin = {
            let root = bits.as_tagged(heap).as_ref().root(heap);
            scope.handle(root)
        };
        let offset = bits.as_tagged(heap).as_ref().bit_offset() + start as usize;
        heap.allocate::<BitArray>(BitArrayInit::View {
            origin,
            bit_offset: offset,
            bit_len: (end - start) as usize,
        })
        .erase()
    })
}

pub fn copy<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, _args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let Some(bits) = call.receiver.as_tagged(heap).get_as::<BitArray>(heap) else {
        raise!(vm, heap, state, VmError::InvalidOperandType)
    };
    let bit_len = bits.as_ref().bit_len();
    let bytes = bits.as_ref().copy_bits(heap, 0, bit_len);
    inline(heap, &bytes, bit_len)
}

pub fn concat<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let Some(left) = call.receiver.as_tagged(heap).get_as::<BitArray>(heap) else {
        raise!(vm, heap, state, VmError::InvalidOperandType)
    };
    let Some(right) = args.get(heap, 0).get_as::<BitArray>(heap) else {
        raise!(vm, heap, state, VmError::InvalidOperandType)
    };
    let mut builder = BitBuilder::default();
    builder.push_source(heap, Source::Bits(left), left.as_ref().bit_len());
    builder.push_source(heap, Source::Bits(right), right.as_ref().bit_len());
    inline(heap, &builder.bytes, builder.bits)
}

pub fn xor<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let Some(left) = call.receiver.as_tagged(heap).get_as::<BitArray>(heap) else {
        raise!(vm, heap, state, VmError::InvalidOperandType)
    };
    let Some(right) = args.get(heap, 0).get_as::<BitArray>(heap) else {
        raise!(vm, heap, state, VmError::InvalidOperandType)
    };
    let bit_len = left.as_ref().bit_len();
    if bit_len != right.as_ref().bit_len() {
        raise!(vm, heap, state, VmError::InvalidOperandType)
    }
    let mut builder = BitBuilder::default();
    for index in 0..bit_len {
        builder.push_bit(left.as_ref().bit_at(heap, index) ^ right.as_ref().bit_at(heap, index));
    }
    inline(heap, &builder.bytes, builder.bits)
}

/// Bitwise equality used by the `==` operator.
pub fn equals(heap: &vm_core::Heap, a: Tagged<'_, BitArray>, b: Tagged<'_, BitArray>) -> bool {
    let len = a.as_ref().bit_len();
    if len != b.as_ref().bit_len() {
        return false;
    }
    (0..len).all(|index| a.as_ref().bit_at(heap, index) == b.as_ref().bit_at(heap, index))
}
