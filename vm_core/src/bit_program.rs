//! Shared varint segment encoding for the Kette compiler and `BitArray`
//! build/match natives.

pub const KIND_INTEGER: u8 = 0;
pub const KIND_FLOAT: u8 = 1;
pub const KIND_BINARY: u8 = 2;
pub const KIND_BITSTRING: u8 = 3;
pub const KIND_UTF8: u8 = 4;
pub const KIND_UTF16: u8 = 5;
pub const KIND_UTF32: u8 = 6;

pub const FLAG_CAPTURE: u8 = 0x08;
pub const FLAG_SIGNED: u8 = 0x10;
pub const FLAG_LITTLE: u8 = 0x20;
pub const FLAG_LITERAL: u8 = 0x40;
pub const FLAG_FLOAT_LITERAL: u8 = 0x80;

pub const SIZE_DEFAULT: u8 = 0;
pub const SIZE_LITERAL: u8 = 1;
pub const SIZE_ARG: u8 = 2;
pub const SIZE_CAPTURE: u8 = 3;

#[derive(Clone, Copy, Debug, Default)]
pub struct Segment {
    pub kind: u8,
    pub flags: u8,
    pub size_form: u8,
    pub size: u32,
    pub unit: u32,
    pub literal: u64,
}

pub fn write_varint(out: &mut Vec<u8>, mut value: u32) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

pub fn write_u64(out: &mut Vec<u8>, value: u64) {
    write_varint(out, value as u32);
    write_varint(out, (value >> 32) as u32);
}

pub fn encode(segments: &[Segment]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + segments.len() * 8);
    write_varint(&mut out, segments.len() as u32);
    for segment in segments {
        out.push(segment.kind | segment.flags);
        write_varint(&mut out, segment.size_form as u32);
        write_varint(&mut out, segment.size);
        write_varint(&mut out, segment.unit);
        if segment.flags & (FLAG_LITERAL | FLAG_FLOAT_LITERAL) != 0 {
            write_u64(&mut out, segment.literal);
        }
    }
    out
}

pub struct Decoder<'a> {
    bytes: &'a [u8],
    pos: usize,
    remaining: u32,
}

impl<'a> Decoder<'a> {
    pub fn new(bytes: &'a [u8]) -> Option<Self> {
        let mut decoder = Self {
            bytes,
            pos: 0,
            remaining: 0,
        };
        decoder.remaining = decoder.varint()?;
        Some(decoder)
    }

    pub fn remaining(&self) -> u32 {
        self.remaining
    }

    pub fn next(&mut self) -> Option<Segment> {
        if self.remaining == 0 {
            return None;
        }
        self.remaining -= 1;
        let kind_flags = *self.bytes.get(self.pos)?;
        self.pos += 1;
        let size_form = self.varint()? as u8;
        let size = self.varint()?;
        let unit = self.varint()?;
        let mut literal = 0;
        if kind_flags & (FLAG_LITERAL | FLAG_FLOAT_LITERAL) != 0 {
            let lo = self.varint()? as u64;
            let hi = self.varint()? as u64;
            literal = lo | (hi << 32);
        }
        Some(Segment {
            kind: kind_flags & 0x07,
            flags: kind_flags & !0x07,
            size_form,
            size,
            unit,
            literal,
        })
    }

    fn varint(&mut self) -> Option<u32> {
        let mut value = 0u32;
        let mut shift = 0;
        loop {
            let byte = *self.bytes.get(self.pos)?;
            self.pos += 1;
            value |= ((byte & 0x7f) as u32) << shift;
            if byte & 0x80 == 0 {
                return Some(value);
            }
            shift += 7;
            if shift >= 35 {
                return None;
            }
        }
    }
}
