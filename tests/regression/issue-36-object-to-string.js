// #36: Object.prototype.toString reports the receiver's builtin tag
// (ES 20.1.3.6), not always "[object Object]".

var ts = Object.prototype.toString;

assert.sameValue(ts.call([]), "[object Array]", "array");
assert.sameValue(ts.call(null), "[object Null]", "null");
assert.sameValue(ts.call(undefined), "[object Undefined]", "undefined");
assert.sameValue(ts.call(function () {}), "[object Function]", "function");
assert.sameValue(ts.call(new Error("x")), "[object Error]", "error");
assert.sameValue(ts.call(1), "[object Number]", "number");
assert.sameValue(ts.call(true), "[object Boolean]", "boolean");
assert.sameValue(ts.call(""), "[object String]", "string");
assert.sameValue(ts.call(Symbol("x")), "[object Symbol]", "symbol");
assert.sameValue(ts.call(new Date(0)), "[object Date]", "date");
assert.sameValue(ts.call({}), "[object Object]", "object");

// ToObject(this): primitive wrappers keep their [[*Data]] internal slot.
assert.sameValue(ts.call(Object(1)), "[object Number]", "Number wrapper");
assert.sameValue(ts.call(Object(true)), "[object Boolean]", "Boolean wrapper");
assert.sameValue(ts.call(Object("")), "[object String]", "String wrapper");
assert.sameValue(ts.call(Number.prototype), "[object Number]", "Number.prototype");
assert.sameValue(ts.call(Boolean.prototype), "[object Boolean]", "Boolean.prototype");
assert.sameValue(ts.call(String.prototype), "[object String]", "String.prototype");
assert.sameValue(ts.call(Symbol.prototype), "[object Symbol]", "Symbol.prototype");
assert.sameValue(
    Object.getPrototypeOf(Object(Symbol("x"))),
    Symbol.prototype,
    "Symbol wrapper prototype"
);
assert.sameValue(
    Object.getPrototypeOf(Symbol("x")),
    Symbol.prototype,
    "Symbol primitive prototype"
);

// Error subclasses inherit [[ErrorData]].
assert.sameValue(ts.call(new TypeError("x")), "[object Error]", "TypeError");
assert.sameValue(ts.call(new ReferenceError("x")), "[object Error]", "ReferenceError");
assert.sameValue(ts.call(new RangeError("x")), "[object Error]", "RangeError");
assert.sameValue(ts.call(new SyntaxError("x")), "[object Error]", "SyntaxError");
assert.sameValue(new SyntaxError("x").name, "SyntaxError", "SyntaxError name");
assert.sameValue(new SyntaxError("x") instanceof Error, true, "SyntaxError instanceof Error");

// VM-materialized SyntaxErrors ride the same error map/flag.
var caughtSyntaxError;
try {
    eval("let duplicate; let duplicate;");
} catch (e) {
    caughtSyntaxError = e;
}
assert.sameValue(
    caughtSyntaxError instanceof SyntaxError,
    true,
    "VM-raised SyntaxError class"
);
assert.sameValue(ts.call(caughtSyntaxError), "[object Error]", "VM-raised SyntaxError tag");

// [[ErrorData]] belongs to instances, not to Error.prototype: an ordinary
// object inheriting from it is not an Error.
var notAnError = Object.create(Error.prototype);
assert.sameValue(ts.call(notAnError), "[object Object]", "Error.prototype child");

// A string-valued @@toStringTag overrides the builtin tag; a non-string
// value does not.
var custom = {};
custom[Symbol.toStringTag] = "Custom";
assert.sameValue(ts.call(custom), "[object Custom]", "@@toStringTag override");
var numeric = {};
numeric[Symbol.toStringTag] = 1;
assert.sameValue(ts.call(numeric), "[object Object]", "non-string @@toStringTag");

// The symbol tag comes from Symbol.prototype[@@toStringTag], so deleting
// it falls back to the builtin tag "Object" (symbols have no
// [[SymbolData]] step in the builtin-tag ladder).
delete Symbol.prototype[Symbol.toStringTag];
assert.sameValue(ts.call(Symbol("x")), "[object Object]", "symbol without tag");
assert.sameValue(ts.call(Symbol.prototype), "[object Object]", "Symbol.prototype without tag");

// Proxy of an array is an array for IsArray.
assert.sameValue(ts.call(new Proxy([], {})), "[object Array]", "array proxy");
assert.sameValue(
    ts.call(new Proxy(new Proxy([], {}), {})),
    "[object Array]",
    "proxy of array proxy"
);

// A throwing @@toStringTag getter propagates.
var poisoned = {};
Object.defineProperty(poisoned, Symbol.toStringTag, {
    get: function () {
        throw new Error("boom");
    },
});
assert.throws(Error, function () {
    ts.call(poisoned);
}, "throwing @@toStringTag getter");
