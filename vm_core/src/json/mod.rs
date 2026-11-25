//! JSON.parse / JSON.stringify.
//!
//! The parser normalizes every guest string to UTF-8 first. UTF-16 and Latin-1
//! strings are copied through `to_rust_string`, which keeps one parser for all
//! Kette and JS string representations.

use crate::{
    Args, Atom, CallInfo, DenseString, Errors, Float, Handle, HandleScope, HandleSlice, Heap,
    HostCtx, Map, MapInit, MapKind, NativeIndex, Object, PropertyDescriptor, Smi, StringInterner,
    StringRef, Symbol, Tagged, ThreadState, VM, Value, VmError,
};

type ParseResult<T> = std::result::Result<T, String>;

/// `JSON.parse ( text [ , reviver ] )`
/// [ES 25.5.1](https://tc39.es/ecma262/#sec-json.parse)
/// Parses one JSON text; strings are normalized to UTF-8 first.
// TODO: the reviver argument is not implemented.
pub fn parse_native<'a>(ctx: HostCtx<'a>, _call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let value = args.get_handle(heap, 0);
    let Some(string) = StringRef::try_from(heap, value.as_tagged(heap)) else {
        return Errors::raise_with(
            vm,
            heap,
            state,
            VmError::InvalidOperandType,
            "JSON.parse expects a string",
        );
    };
    let text = string.to_rust_string(heap);
    state.handle_scope(|scope| {
        let parsed = {
            let mut parser = Parser {
                vm,
                heap,
                scope: &scope,
                input: &text,
                pos: 0,
                depth: 0,
            };
            parser.parse()
        };
        match parsed {
            Ok(value) => value.as_tagged(heap).erase(),
            Err(message) => Errors::raise_with(vm, heap, state, VmError::JsonSyntaxError, &message),
        }
    })
}

/// `JSON.stringify ( value [ , replacer [ , space ] ] )`
/// [ES 25.5.2](https://tc39.es/ecma262/#sec-json.stringify)
/// Serializes a value; output is compact.
// TODO: replacer and space are not implemented.
// TODO: toJSON and Proxy ownKeys semantics are not implemented.
pub fn stringify_native<'a>(
    ctx: HostCtx<'a>,
    _call: CallInfo<'_>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let value = args.get_handle(heap, 0);
    state.handle_scope(|scope| {
        let value = scope.handle(value.as_tagged(heap).erase());
        let mut stack = Vec::new();
        match stringify_value(vm, heap, state, &scope, value, &mut stack, 0) {
            Ok(None) => vm.known().undefined.get(heap).erase(),
            Ok(Some(text)) => {
                let text = vm.interner().intern_str(heap, &scope, &text);
                text.as_tagged(heap).erase()
            }
            Err(message) => {
                let error = if message.starts_with("circular") {
                    VmError::JsonCircular
                } else {
                    VmError::InvalidOperandType
                };
                Errors::raise_with(vm, heap, state, error, &message)
            }
        }
    })
}

struct Parser<'i, 'h, 's, 'd> {
    vm: &'i VM,
    heap: &'h mut Heap,
    scope: &'s HandleScope<'d>,
    input: &'i str,
    pos: usize,
    depth: usize,
}

impl<'i, 'h, 's, 'd> Parser<'i, 'h, 's, 'd> {
    fn parse(&mut self) -> ParseResult<Handle<'s, Value>> {
        self.skip_whitespace();
        let value = self.value()?;
        self.skip_whitespace();
        if self.pos != self.input.len() {
            return self.fail("trailing characters");
        }
        Ok(value)
    }

    fn value(&mut self) -> ParseResult<Handle<'s, Value>> {
        self.skip_whitespace();
        let Some(byte) = self.peek() else {
            return self.fail("unexpected end of input");
        };
        match byte {
            b'{' => self.object(),
            b'[' => self.array(),
            b'"' => {
                let text = self.string()?;
                let string = DenseString::from_utf8(self.heap, self.scope, &text);
                Ok(self.scope.handle(string.as_tagged(self.heap).erase()))
            }
            b't' => {
                self.literal("true")?;
                Ok(self
                    .scope
                    .handle(self.heap.known().true_object.get(self.heap).erase()))
            }
            b'f' => {
                self.literal("false")?;
                Ok(self
                    .scope
                    .handle(self.heap.known().false_object.get(self.heap).erase()))
            }
            b'n' => {
                self.literal("null")?;
                Ok(self
                    .scope
                    .handle(self.heap.known().null.get(self.heap).erase()))
            }
            b'-' | b'0'..=b'9' => {
                let number = self.number()?;
                Ok(self.scope.handle(self.heap.new_number(number)))
            }
            _ => self.fail("unexpected character"),
        }
    }

    fn object(&mut self) -> ParseResult<Handle<'s, Value>> {
        self.depth += 1;
        if self.depth > 512 {
            return self.fail("nesting is too deep");
        }
        self.pos += 1;
        let object = self.new_object();
        self.skip_whitespace();
        if self.consume(b'}') {
            self.depth -= 1;
            return Ok(object);
        }
        loop {
            self.skip_whitespace();
            if self.peek() != Some(b'"') {
                return self.fail("expected an object key");
            }
            let key = self.string()?;
            self.skip_whitespace();
            if !self.consume(b':') {
                return self.fail("expected ':'");
            }
            let value = self.value()?;
            let key = self.vm.interner().intern_str(self.heap, self.scope, &key);
            let key = self
                .scope
                .handle(key.as_tagged(self.heap).erase().as_name());
            let object_value = self
                .scope
                .cast::<Object>(self.heap, object.as_tagged(self.heap).erase())
                .expect("JSON object is an object");
            Object::define_own_property_ordinary(
                self.heap,
                self.scope,
                object_value,
                key,
                PropertyDescriptor::data(value),
            )
            .map_err(|error| format!("cannot define property: {error:?}"))?;
            self.skip_whitespace();
            match self.peek() {
                Some(b',') => {
                    self.pos += 1;
                }
                Some(b'}') => {
                    self.pos += 1;
                    self.depth -= 1;
                    return Ok(object);
                }
                _ => return self.fail("expected ',' or '}'"),
            }
        }
    }

    fn array(&mut self) -> ParseResult<Handle<'s, Value>> {
        self.depth += 1;
        if self.depth > 512 {
            return self.fail("nesting is too deep");
        }
        self.pos += 1;
        let mut values: Vec<Handle<'s, Value>> = Vec::new();
        self.skip_whitespace();
        if self.consume(b']') {
            self.depth -= 1;
            let array = self.heap.new_array(self.scope, HandleSlice::EMPTY);
            return Ok(self.scope.handle(array.erase()));
        }
        loop {
            values.push(self.value()?);
            self.skip_whitespace();
            match self.peek() {
                Some(b',') => {
                    self.pos += 1;
                }
                Some(b']') => {
                    self.pos += 1;
                    self.depth -= 1;
                    let staged = self.scope.stage_handles(&values);
                    let array = self.heap.new_array(self.scope, staged);
                    return Ok(self.scope.handle(array.erase()));
                }
                _ => return self.fail("expected ',' or ']'"),
            }
        }
    }

    fn string(&mut self) -> ParseResult<String> {
        self.pos += 1;
        let mut out = String::new();
        loop {
            let Some(byte) = self.peek() else {
                return self.fail("unterminated string");
            };
            if byte == b'"' {
                self.pos += 1;
                return Ok(out);
            }
            if byte == b'\\' {
                self.pos += 1;
                let Some(escape) = self.peek() else {
                    return self.fail("unterminated escape");
                };
                self.pos += 1;
                match escape {
                    b'"' => out.push('"'),
                    b'\\' => out.push('\\'),
                    b'/' => out.push('/'),
                    b'b' => out.push('\u{0008}'),
                    b'f' => out.push('\u{000c}'),
                    b'n' => out.push('\n'),
                    b'r' => out.push('\r'),
                    b't' => out.push('\t'),
                    b'u' => {
                        let mut code = self.hex4()? as u32;
                        if (0xD800..=0xDBFF).contains(&code) {
                            if self.input.as_bytes().get(self.pos) == Some(&b'\\')
                                && self.input.as_bytes().get(self.pos + 1) == Some(&b'u')
                            {
                                self.pos += 2;
                                let low = self.hex4()? as u32;
                                if (0xDC00..=0xDFFF).contains(&low) {
                                    code = 0x10000 + ((code - 0xD800) << 10) + (low - 0xDC00);
                                } else {
                                    code = 0xFFFD;
                                }
                            } else {
                                code = 0xFFFD;
                            }
                        } else if (0xDC00..=0xDFFF).contains(&code) {
                            code = 0xFFFD;
                        }
                        out.push(char::from_u32(code).unwrap_or('\u{FFFD}'));
                    }
                    _ => return self.fail("invalid escape"),
                }
                continue;
            }
            if byte < 0x20 {
                return self.fail("unescaped control character");
            }
            let Some(character) = self.input[self.pos..].chars().next() else {
                return self.fail("unterminated string");
            };
            out.push(character);
            self.pos += character.len_utf8();
        }
    }

    fn number(&mut self) -> ParseResult<f64> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        match self.peek() {
            Some(b'0') => {
                self.pos += 1;
                if matches!(self.peek(), Some(b'0'..=b'9')) {
                    return self.fail("leading zero in number");
                }
            }
            Some(b'1'..=b'9') => {
                while matches!(self.peek(), Some(b'0'..=b'9')) {
                    self.pos += 1;
                }
            }
            _ => return self.fail("expected a number"),
        }
        if self.peek() == Some(b'.') {
            self.pos += 1;
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return self.fail("expected a fraction digit");
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.pos += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return self.fail("expected an exponent digit");
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
        }
        self.input[start..self.pos]
            .parse::<f64>()
            .map_err(|_| "invalid number".to_string())
    }

    fn hex4(&mut self) -> ParseResult<u16> {
        let mut value = 0u16;
        for _ in 0..4 {
            let Some(byte) = self.peek() else {
                return self.fail("truncated unicode escape");
            };
            let digit = match byte {
                b'0'..=b'9' => byte - b'0',
                b'a'..=b'f' => byte - b'a' + 10,
                b'A'..=b'F' => byte - b'A' + 10,
                _ => return self.fail("invalid unicode escape"),
            };
            value = value * 16 + digit as u16;
            self.pos += 1;
        }
        Ok(value)
    }

    fn literal(&mut self, word: &str) -> ParseResult<()> {
        if self.input[self.pos..].starts_with(word) {
            self.pos += word.len();
            Ok(())
        } else {
            self.fail("invalid literal")
        }
    }

    fn consume(&mut self, byte: u8) -> bool {
        if self.peek() == Some(byte) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn peek(&self) -> Option<u8> {
        self.input.as_bytes().get(self.pos).copied()
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\n' | b'\r' | b'\t')) {
            self.pos += 1;
        }
    }

    fn new_object(&mut self) -> Handle<'s, Value> {
        let prototype = self
            .heap
            .known()
            .object_prototype
            .get_handle(self.heap, self.scope);
        let map = self.heap.allocate_handle::<Map>(
            MapInit {
                kind: MapKind::OBJECT.union(MapKind::EXTENDABLE),
                value_slot_count: 0,
                descriptors: &[],
                prototype: prototype.erase(),
            },
            self.scope,
        );
        let object = self.heap.new_object(self.scope, map, HandleSlice::EMPTY);
        self.scope.handle(object.erase())
    }

    fn fail<T>(&self, message: &str) -> ParseResult<T> {
        Err(format!("{message} at byte {}", self.pos))
    }
}

fn stringify_value<'s, 'd>(
    vm: &VM,
    heap: &mut Heap,
    state: &ThreadState,
    scope: &'s HandleScope<'d>,
    value: Handle<'s, Value>,
    stack: &mut Vec<Handle<'s, Value>>,
    depth: usize,
) -> ParseResult<Option<String>> {
    if depth > 512 {
        return Err("nesting is too deep".to_string());
    }
    let raw = value.as_tagged(heap);
    let known = heap.known();
    if raw.ptr_eq(known.undefined.get(heap)) {
        return Ok(None);
    }
    if raw.ptr_eq(known.null.get(heap)) {
        return Ok(Some("null".to_string()));
    }
    if raw.ptr_eq(known.true_object.get(heap)) {
        return Ok(Some("true".to_string()));
    }
    if raw.ptr_eq(known.false_object.get(heap)) {
        return Ok(Some("false".to_string()));
    }
    if let Some(smi) = Smi::decode(raw.raw()) {
        return Ok(Some(smi.value().to_string()));
    }
    if let Some(float) = raw.get_as::<Float>(heap) {
        let number = float.value.get();
        if !number.is_finite() {
            return Ok(Some("null".to_string()));
        }
        if number == 0.0 {
            return Ok(Some("0".to_string()));
        }
        return Ok(Some(number.to_string()));
    }
    if let Some(atom) = raw.get_as::<Atom>(heap) {
        let name = atom.name(heap);
        let text = StringRef::try_from(heap, name.erase())
            .map(|s| s.to_rust_string(heap))
            .unwrap_or_default();
        return Ok(Some(quote(&text)));
    }
    if let Some(string) = StringRef::try_from(heap, raw) {
        let units = string.to_units(heap);
        return Ok(Some(quote_units(&units)));
    }
    if Object::is_array(heap, raw) == Some(true) {
        if stack.iter().any(|open| open.as_tagged(heap).ptr_eq(raw)) {
            return Err("circular structure".to_string());
        }
        stack.push(value);
        let object = raw.as_heap_object().expect("array is an object");
        let length = object.as_ref().length();
        let mut out = String::from("[");
        for index in 0..length {
            if index > 0 {
                out.push(',');
            }
            let key = scope.handle(Smi::new(index as i64).into_tagged());
            let child = Object::get_property(vm, heap, state, value, value, key, false)
                .map_err(|error| format!("cannot read array element: {error:?}"))?;
            let child = scope.handle(child);
            match stringify_value(vm, heap, state, scope, child, stack, depth + 1)? {
                Some(text) => out.push_str(&text),
                None => out.push_str("null"),
            }
        }
        out.push(']');
        stack.pop();
        return Ok(Some(out));
    }
    if raw.is_callable(heap) {
        return Ok(None);
    }
    if raw.as_heap_object().is_some() {
        if stack.iter().any(|open| open.as_tagged(heap).ptr_eq(raw)) {
            return Err("circular structure".to_string());
        }
        stack.push(value);
        let keys = Object::own_property_keys(vm, heap, state, scope, value)
            .map_err(|error| format!("cannot enumerate properties: {error:?}"))?;
        let mut members = Vec::new();
        for key in keys {
            let key_value = key.as_tagged(heap);
            if key_value.get_as::<Symbol>(heap).is_some() {
                continue;
            }
            let key_text = if let Some(smi) = Smi::decode(key_value.raw()) {
                smi.value().to_string()
            } else if let Some(atom) = key_value.get_as::<Atom>(heap) {
                StringRef::try_from(heap, atom.name(heap).erase())
                    .map(|s| s.to_rust_string(heap))
                    .unwrap_or_default()
            } else if let Some(string) = StringRef::try_from(heap, key_value) {
                string.to_rust_string(heap)
            } else {
                continue;
            };
            let own = Object::get_own_property(vm, heap, state, scope, value, key)
                .map_err(|error| format!("cannot read property: {error:?}"))?;
            if !own.is_some_and(|property| property.desc.is_enumerable()) {
                continue;
            }
            let child = Object::get_property(vm, heap, state, value, value, key, false)
                .map_err(|error| format!("cannot read property: {error:?}"))?;
            let child = scope.handle(child);
            if let Some(text) = stringify_value(vm, heap, state, scope, child, stack, depth + 1)? {
                members.push(format!("{}:{}", quote(&key_text), text));
            }
        }
        stack.pop();
        return Ok(Some(format!("{{{}}}", members.join(","))));
    }
    Ok(None)
}

pub fn install(
    heap: &mut Heap,
    scope: &HandleScope<'_>,
    interner: &StringInterner,
    parse: NativeIndex,
    stringify: NativeIndex,
) {
    let prototype = heap.known().object_prototype.get_handle(heap, scope);
    let map = heap.allocate_handle::<Map>(
        MapInit {
            kind: MapKind::OBJECT.union(MapKind::EXTENDABLE),
            value_slot_count: 0,
            descriptors: &[],
            prototype: prototype.erase(),
        },
        scope,
    );
    let json = scope.handle(heap.new_object(scope, map, HandleSlice::EMPTY));
    for (name, index, length) in [("parse", parse, 2u32), ("stringify", stringify, 3)] {
        let function = Object::native_function(heap, scope, index, length);
        let key = interner.intern_str(heap, scope, name);
        let key = scope.handle(key.as_tagged(heap).erase().as_name());
        Object::define_own_property_ordinary(
            heap,
            scope,
            json,
            key,
            PropertyDescriptor::method(scope.handle(function.as_tagged(heap).erase())),
        )
        .expect("install JSON method");
    }
    let global = heap.known().global_object.get_handle(heap, scope);
    let key = interner.intern_str(heap, scope, "JSON");
    let key = scope.handle(key.as_tagged(heap).erase().as_name());
    Object::define_own_property_ordinary(
        heap,
        scope,
        global,
        key,
        PropertyDescriptor::Data {
            value: scope.handle(json.as_tagged(heap).erase()),
            writable: true,
            enumerable: false,
            configurable: true,
        },
    )
    .expect("install JSON global");
}

fn quote(text: &str) -> String {
    let units: Vec<u16> = text.encode_utf16().collect();
    quote_units(&units)
}

fn quote_units(units: &[u16]) -> String {
    let mut out = String::from("\"");
    let mut index = 0;
    while index < units.len() {
        let unit = units[index];
        match unit {
            0x22 => out.push_str("\\\""),
            0x5c => out.push_str("\\\\"),
            0x08 => out.push_str("\\b"),
            0x0c => out.push_str("\\f"),
            0x0a => out.push_str("\\n"),
            0x0d => out.push_str("\\r"),
            0x09 => out.push_str("\\t"),
            0x00..=0x1f => out.push_str(&format!("\\u{unit:04x}")),
            0xd800..=0xdbff
                if index + 1 < units.len() && (0xdc00..=0xdfff).contains(&units[index + 1]) =>
            {
                let code =
                    0x10000 + ((unit as u32 - 0xd800) << 10) + (units[index + 1] as u32 - 0xdc00);
                out.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                index += 1;
            }
            0xd800..=0xdfff => out.push_str(&format!("\\u{unit:04x}")),
            _ => out.push(char::from_u32(unit as u32).unwrap_or('\u{fffd}')),
        }
        index += 1;
    }
    out.push('"');
    out
}
