// ES 20.4: Symbol objects.

// Symbol ( [ description ] )
assert.sameValue(typeof Symbol("x"), "symbol", "Symbol returns a primitive");
assert.sameValue(Symbol().description, undefined, "absent description is undefined");
assert.sameValue(Symbol(undefined).description, undefined, "undefined description is undefined");
assert.sameValue(Symbol("x").description, "x", "description is the ToString'd argument");
assert.sameValue(Symbol(1).description, "1", "description is ToString'd");
assert.throws(TypeError, function () {
    return Symbol(Symbol());
}, "a Symbol description throws");
assert.throws(TypeError, function () {
    return new Symbol();
}, "Symbol is not a constructor");

// Symbols are unique, even with the same description.
assert.sameValue(Symbol("x") === Symbol("x"), false, "symbols are unique");

// Symbol.prototype.toString / valueOf / description.
var sym = Symbol("desc");
assert.sameValue(sym.toString(), "Symbol(desc)", "descriptive string");
assert.sameValue(Symbol().toString(), "Symbol()", "empty descriptive string");
assert.sameValue(sym.valueOf(), sym, "valueOf returns the primitive");
assert.sameValue(String(sym), "Symbol(desc)", "String(sym) describes the symbol");

var wrapped = Object(sym);
assert.sameValue(wrapped.valueOf(), sym, "wrapper valueOf unwraps");
assert.sameValue(wrapped.toString(), "Symbol(desc)", "wrapper toString describes");
assert.sameValue(wrapped.description, "desc", "wrapper description unwraps");
assert.throws(TypeError, function () {
    return Symbol.prototype.toString.call(1);
}, "toString rejects a non-symbol");
assert.throws(TypeError, function () {
    return Symbol.prototype.valueOf.call({});
}, "valueOf rejects a non-symbol");

// Symbol.prototype[@@toPrimitive].
assert.sameValue(
    Symbol.prototype[Symbol.toPrimitive].call(sym),
    sym,
    "toPrimitive returns the symbol"
);
assert.sameValue(
    wrapped[Symbol.toPrimitive]("string"),
    sym,
    "toPrimitive ignores the hint"
);
assert.sameValue(
    Symbol.prototype[Symbol.toPrimitive].name,
    "[Symbol.toPrimitive]",
    "toPrimitive is named"
);

// Symbol.for / Symbol.keyFor.
assert.sameValue(Symbol.for("key") === Symbol.for("key"), true, "for is idempotent");
assert.sameValue(Symbol.for(1) === Symbol.for("1"), true, "for ToStrings the key");
assert.sameValue(Symbol.for("key").description, "key", "for uses the key as description");
assert.sameValue(Symbol.keyFor(Symbol.for("key")), "key", "keyFor returns the key");
assert.sameValue(Symbol.keyFor(Symbol("key")), undefined, "keyFor misses local symbols");
assert.throws(TypeError, function () {
    return Symbol.keyFor("key");
}, "keyFor rejects a non-symbol");

// Well-known symbols.
assert.sameValue(typeof Symbol.iterator, "symbol", "Symbol.iterator exists");
assert.sameValue(Symbol.iterator.description, "Symbol.iterator", "iterator description");
assert.sameValue(typeof Symbol.asyncIterator, "symbol", "Symbol.asyncIterator exists");
assert.sameValue(typeof Symbol.hasInstance, "symbol", "Symbol.hasInstance exists");
assert.sameValue(typeof Symbol.isConcatSpreadable, "symbol", "isConcatSpreadable exists");
assert.sameValue(typeof Symbol.match, "symbol", "Symbol.match exists");
assert.sameValue(typeof Symbol.replace, "symbol", "Symbol.replace exists");
assert.sameValue(typeof Symbol.search, "symbol", "Symbol.search exists");
assert.sameValue(typeof Symbol.species, "symbol", "Symbol.species exists");
assert.sameValue(typeof Symbol.split, "symbol", "Symbol.split exists");
assert.sameValue(typeof Symbol.toPrimitive, "symbol", "Symbol.toPrimitive exists");
assert.sameValue(typeof Symbol.toStringTag, "symbol", "Symbol.toStringTag exists");
assert.sameValue(typeof Symbol.unscopables, "symbol", "Symbol.unscopables exists");
assert.sameValue(Symbol.prototype[Symbol.toStringTag], "Symbol", "prototype toStringTag");

// The well-known symbols are stable and unique.
assert.sameValue(Symbol.iterator === Symbol.iterator, true, "iterator is stable");
assert.sameValue(Symbol.iterator === Symbol.for("Symbol.iterator"), false, "iterator is not registered");
assert.sameValue(Symbol.keyFor(Symbol.iterator), undefined, "well-known symbols are unregistered");
assert.sameValue(
    Object.getOwnPropertyDescriptor(Symbol, "iterator").writable,
    false,
    "well-known symbols are read-only"
);
assert.sameValue(
    Object.getOwnPropertyDescriptor(Symbol.prototype, "description").set,
    undefined,
    "description has no setter"
);

// NamedEvaluation names symbol-keyed methods "[description]" (ES 10.2.9).
assert.sameValue(
    { [Symbol.iterator]() {} }[Symbol.iterator].name,
    "[Symbol.iterator]",
    "symbol-keyed methods are named after the description"
);
assert.sameValue(
    { b: function () {} }.b.name,
    "b",
    "anonymous function values are named after their key"
);
