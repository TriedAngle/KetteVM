use crate::{
    DenseString, Float, Handle, HandleScope, Heap, Object, Result, StringData, Symbol, Tagged,
    ThreadState, VM, Value, VmError, WrapperKind, bail,
};

/// ToPrimitive hint (ES 7.1.1).
#[derive(Copy, Clone, PartialEq, Eq)]
pub enum Hint {
    Default,
    Number,
    String,
}

pub struct Convert;

/// No-GC coercions: methods on the value itself. Functions that can
/// allocate (and thus GC) stay in the `Convert` namespace above and
/// take `heap` first.
impl<'a> Tagged<'a, Value> {
    /// The numeric value of a value that already IS a number
    #[inline]
    pub fn as_number(self, heap: &Heap) -> Option<f64> {
        if let Some(i) = self.to_i64() {
            return Some(i as f64);
        }
        self.get_as::<Float>(heap).map(|f| f.value.get())
    }

    /// ES ToBoolean. Falsey: `false`, `undefined`, `null`, the hole, 0, -0, NaN,
    /// everything else is truthy.
    #[inline]
    pub fn is_truthy(self, heap: &Heap) -> bool {
        let known = heap.known();
        if let Some(i) = self.to_i64() {
            return i != 0;
        }
        if self == known.false_object.get(heap)
            || self == known.undefined.get(heap)
            || self == known.null.get(heap)
            || self == known.the_hole.get(heap)
        {
            return false;
        }
        if self == known.true_object.get(heap) {
            return true;
        }
        if let Some(f) = self.get_as::<Float>(heap) {
            let x = f.value.get();
            // -0.0 compares equal to 0.0; NaN compares unequal to everything
            return x != 0.0 && !x.is_nan();
        }
        // "" is the only falsey value left; everything else is truthy.
        !self.is_empty_string(heap)
    }

    pub fn to_number(self, heap: &Heap) -> Option<f64> {
        let known = heap.known();
        if let Some(i) = self.to_i64() {
            return Some(i as f64);
        }
        if self == known.undefined.get(heap) || self == known.the_hole.get(heap) {
            return Some(f64::NAN);
        }
        if self == known.null.get(heap) {
            return Some(0.0);
        }
        if self == known.false_object.get(heap) {
            return Some(0.0);
        }
        if self == known.true_object.get(heap) {
            return Some(1.0);
        }
        if let Some(f) = self.get_as::<Float>(heap) {
            return Some(f.value.get());
        }
        if let Some(s) = self.get_as::<DenseString>(heap) {
            return Some(Convert::string_to_number(s.data(heap)).unwrap_or(f64::NAN));
        }
        None
    }

    /// ES Type check: numbers, strings, symbols, booleans, null, undefined
    /// are primitives; everything else is an object.
    #[inline]
    pub fn is_primitive(self, heap: &Heap) -> bool {
        let known = heap.known();
        if self.is_smi() {
            return true;
        }
        self == known.undefined.get(heap)
            || self == known.null.get(heap)
            || self == known.true_object.get(heap)
            || self == known.false_object.get(heap)
            || self.get_as::<Float>(heap).is_some()
            || self.get_as::<DenseString>(heap).is_some()
            || self.get_as::<Symbol>(heap).is_some()
    }

    /// Whether the value is `null` or `undefined` (the spec's nullish
    /// predicate). No GC.
    #[inline(always)]
    pub fn is_nullish(self, heap: &Heap) -> bool {
        self == heap.known().undefined.get(heap) || self == heap.known().null.get(heap)
    }

    /// ES 7.1.13 ToObject for the primitive kinds that expose a distinct
    /// prototype: the prototype of a number/string/boolean/symbol primitive
    /// is the matching builtin prototype. `None` for objects and for the
    /// no-prototype primitives.
    #[inline]
    pub fn primitive_prototype(self, heap: &'a Heap) -> Option<Tagged<'a, Value>> {
        let known = heap.known();
        if self.get_as::<Symbol>(heap).is_some() {
            return Some(known.symbol_prototype.get(heap).erase());
        }
        Some(match WrapperKind::classify(heap, known, self)? {
            WrapperKind::Number => known.number_prototype.get(heap).erase(),
            WrapperKind::Boolean => known.boolean_prototype.get(heap).erase(),
            WrapperKind::String => known.string_prototype.get(heap).erase(),
        })
    }
}

impl Convert {
    /// ES 7.1.20 ToObject: wrap primitives, return objects unchanged.
    pub fn to_object<'s>(
        vm: &VM,
        heap: &mut Heap,
        state: &ThreadState,
        scope: &'s HandleScope<'_>,
        value: Handle<'_, Value>,
    ) -> Result<Handle<'s, Object>> {
        if value.as_tagged(heap).is_nullish(heap) {
            bail!(vm, heap, state, VmError::CannotConvertNullishToObject);
        }
        if !value.as_tagged(heap).is_primitive(heap) {
            let obj = value
                .as_tagged(heap)
                .as_heap_object()
                .expect("non-primitive values are objects");
            return Ok(scope.handle(obj));
        }
        let known = vm.known();
        let map = if let Some(kind) = WrapperKind::classify(heap, known, value.as_tagged(heap)) {
            kind.wrapper_map(heap, known)
        } else if value.as_tagged(heap).get_as::<Symbol>(heap).is_some() {
            known.symbol_wrapper_map.get(heap)
        } else {
            // TODO: BigInt and any future primitive wrapper kinds.
            bail!(vm, heap, state, VmError::CannotConvertToObject);
        };
        let map = scope.handle(map);
        let value = scope.handle(value.as_tagged(heap));
        let object = heap.new_object(scope, map, scope.stage(&[value.as_tagged(heap)]));
        Ok(scope.handle(object))
    }

    /// ES 7.1.5 ToIntegerOrInfinity: NaN is +0, otherwise truncated.
    pub fn to_integer_or_infinity(
        vm: &VM,
        heap: &mut Heap,
        state: &ThreadState,
        value: Handle<'_, Value>,
    ) -> Result<f64> {
        let n = Object::to_numeric(vm, heap, state, value)?;
        Ok(if n.is_nan() { 0.0 } else { n.trunc() })
    }

    /// ES 7.1.25 ToAbsoluteIndex: negative integers count from `length`.
    #[inline(always)]
    pub fn to_absolute_index(
        vm: &VM,
        heap: &mut Heap,
        state: &ThreadState,
        value: Handle<'_, Value>,
        length: u64,
    ) -> Result<f64> {
        let int = Self::to_integer_or_infinity(vm, heap, state, value)?;
        Ok(if int.is_finite() && int < 0.0 {
            length as f64 + int
        } else {
            int
        })
    }

    /// ES 7.1.26 ToClampedIndex: ToAbsoluteIndex clamped to `[0, length]`.
    #[inline(always)]
    pub fn to_clamped_index(
        vm: &VM,
        heap: &mut Heap,
        state: &ThreadState,
        value: Handle<'_, Value>,
        length: u64,
    ) -> Result<f64> {
        let index = Self::to_absolute_index(vm, heap, state, value, length)?;
        Ok(index.clamp(0.0, length as f64))
    }

    /// ES 7.1.6 ToInt32 applied to a number: truncate, reduce mod 2^32,
    /// map the upper half to negatives. NaN and ±∞ are +0.
    pub fn number_to_int32(n: f64) -> i32 {
        if !n.is_finite() {
            return 0;
        }
        let n = n.trunc().rem_euclid(4_294_967_296.0);
        if n >= 2_147_483_648.0 {
            (n - 4_294_967_296.0) as i32
        } else {
            n as i32
        }
    }

    /// ES 7.1.7 ToUint32 applied to a number. NaN and ±∞ are +0.
    pub fn number_to_uint32(n: f64) -> u32 {
        if !n.is_finite() {
            return 0;
        }
        n.trunc().rem_euclid(4_294_967_296.0) as u32
    }

    /// StringNumericLiteral → f64 (ES 7.1.4.1). `None` means NaN (invalid
    /// numeric content); empty or all-whitespace input is +0.
    fn string_to_number(data: StringData<'_>) -> Option<f64> {
        // ES trims the same whitespace set as before the re-encoding;
        // the numeric grammar itself is pure ASCII
        let unit = |i: usize| data.code_unit(i);
        let lo = data.trim_start_index();
        let hi = data.trim_end_index();
        if lo >= hi {
            return Some(0.0);
        }
        let matches = |lit: &[u8]| data.starts_with_at(lo, lit) && lo + lit.len() == hi;
        if matches(b"Infinity") || matches(b"+Infinity") {
            return Some(f64::INFINITY);
        }
        if matches(b"-Infinity") {
            return Some(f64::NEG_INFINITY);
        }
        if matches(b"NaN") {
            return Some(f64::NAN);
        }
        // NonDecimalIntegerLiteral (ES 7.1.4.1): 0x/0o/0b radix prefixes,
        // no sign allowed before them
        if hi - lo >= 2 && unit(lo) == b'0' as u16 {
            let radix = match unit(lo + 1) as u8 {
                b'x' | b'X' => Some(16),
                b'o' | b'O' => Some(8),
                b'b' | b'B' => Some(2),
                _ => None,
            };
            if let Some(radix) = radix {
                let mut acc = 0.0f64;
                for i in lo + 2..hi {
                    let c = char::from_u32(unit(i) as u32)?;
                    let digit = c.to_digit(radix)?;
                    acc = acc * radix as f64 + digit as f64;
                }
                // "0x" with no digits is NaN (empty digit run)
                return (hi - lo > 2).then_some(acc);
            }
        }
        // The whole trimmed text must be one StrDecimalLiteral; Rust's f64
        // parser is more lenient (`inf`, `nan`), so validate the grammar.
        if data.decimal_prefix_end(lo) != Some(hi) {
            return None;
        }
        // any unit above 0x7F cannot participate in a numeric literal
        let bytes: Vec<u8> = (lo..hi)
            .map(|i| u8::try_from(unit(i)).ok())
            .collect::<Option<Vec<u8>>>()?;
        let text = core::str::from_utf8(&bytes).ok()?;
        text.parse::<f64>().ok()
    }

    /// The true/false singleton for a Rust bool.
    #[inline]
    pub fn boolean<'a>(heap: &'a Heap, b: bool) -> Tagged<'a, Value> {
        let known = heap.known();
        if b {
            known.true_object.get(heap).erase()
        } else {
            known.false_object.get(heap).erase()
        }
    }

    /// ES Type check: numbers, strings, symbols, booleans, null, undefined
    /// are primitives; everything else is an object.
    #[inline]
    pub fn is_primitive(heap: &Heap, v: Tagged<'_, Value>) -> bool {
        let known = heap.known();
        if v.is_smi() {
            return true;
        }
        v == known.undefined.get(heap)
            || v == known.null.get(heap)
            || v == known.true_object.get(heap)
            || v == known.false_object.get(heap)
            || v.get_as::<Float>(heap).is_some()
            || v.get_as::<DenseString>(heap).is_some()
            || v.get_as::<Symbol>(heap).is_some()
    }

    /// ES ToString on a primitive (no ToPrimitive recursion: the input is
    /// already primitive). Numbers allocate a fresh string, symbols are a
    /// TypeError. The oddball identity strings come from the string table.
    /// The input is rooted first; the result is anchored at the `&mut Heap`
    /// borrow.
    pub fn to_string<'a>(
        vm: &VM,
        heap: &'a mut Heap,
        state: &ThreadState,
        scope: &HandleScope<'_>,
        v: Handle<'_, Value>,
    ) -> Result<Tagged<'a, Value>> {
        // Smis allocate a fresh string.
        if let Some(i) = v.as_tagged(heap).to_i64() {
            let s = DenseString::from_utf8(heap, scope, &i.to_string());
            return Ok(s.as_tagged(heap).erase());
        }

        let known = heap.known();
        let val = v.as_tagged(heap);
        if val == known.undefined.get(heap) {
            return Ok(known.strings.undefined.get(heap).erase());
        }
        if val == known.null.get(heap) {
            return Ok(known.strings.null.get(heap).erase());
        }
        if val == known.true_object.get(heap) {
            return Ok(known.strings.true_.get(heap).erase());
        }
        if val == known.false_object.get(heap) {
            return Ok(known.strings.false_.get(heap).erase());
        }
        // Strings are their own stringification.
        if v.as_tagged(heap).get_as::<DenseString>(heap).is_some() {
            return Ok(v.as_tagged(heap));
        }
        // Floats allocate a fresh string; the tagged view is copied out
        // before the allocation so its heap borrow is released.
        let float = v.as_tagged(heap).get_as::<Float>(heap);
        if let Some(f) = float {
            let text = Self::number_to_string(f.value.get());
            let s = DenseString::from_utf8(heap, scope, &text);
            return Ok(s.as_tagged(heap).erase());
        }
        // Symbols (and anything else reaching this point) are a TypeError.
        bail!(vm, heap, state, VmError::CannotConvertSymbolToString)
    }

    /// ES 6.1.6.1.20 `Number::toString`. Rust's `Display` for `f64` stays
    /// in fixed notation, while ECMAScript switches to exponential form at
    /// `1e21` and below `1e-6`; `LowerExp` supplies the shortest round-trip
    /// digits, which are then laid out using the spec's `n`/`k` rules.
    pub fn number_to_string(x: f64) -> String {
        if x.is_nan() {
            return "NaN".to_string();
        }
        if x == f64::INFINITY {
            return "Infinity".to_string();
        }
        if x == f64::NEG_INFINITY {
            return "-Infinity".to_string();
        }
        // Both +0 and -0 stringify as "0".
        if x == 0.0 {
            return "0".to_string();
        }

        let negative = x.is_sign_negative();
        let scientific = format!("{:e}", x.abs());
        let (mantissa, exponent) = scientific
            .split_once('e')
            .expect("LowerExp always includes an exponent");
        let exponent: i32 = exponent
            .parse()
            .expect("LowerExp exponent is a base-10 integer");
        let mut digits = String::with_capacity(mantissa.len());
        for c in mantissa.chars() {
            if c != '.' {
                digits.push(c);
            }
        }

        let k = digits.len() as i32;
        let n = exponent + 1;
        let mut out = String::with_capacity(digits.len() + 8);
        if negative {
            out.push('-');
        }
        if k <= n && n <= 21 {
            out.push_str(&digits);
            for _ in 0..(n - k) {
                out.push('0');
            }
        } else if 0 < n && n <= 21 {
            let split = n as usize;
            out.push_str(&digits[..split]);
            out.push('.');
            out.push_str(&digits[split..]);
        } else if -6 < n && n <= 0 {
            out.push_str("0.");
            for _ in 0..(-n) {
                out.push('0');
            }
            out.push_str(&digits);
        } else {
            out.push_str(&digits[..1]);
            if k > 1 {
                out.push('.');
                out.push_str(&digits[1..]);
            }
            out.push('e');
            let exponent = n - 1;
            if exponent >= 0 {
                out.push('+');
            }
            out.push_str(&exponent.to_string());
        }
        out
    }
}
