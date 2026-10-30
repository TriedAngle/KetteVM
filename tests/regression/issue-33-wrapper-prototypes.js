// #33: Number.prototype/String.prototype/Boolean.prototype are wrapper
// objects carrying 0/""/false in their internal primitive slot.

assert.sameValue(Number.prototype.valueOf(), 0, "Number.prototype.valueOf()");
assert.sameValue(String.prototype.valueOf(), "", "String.prototype.valueOf()");
assert.sameValue(Boolean.prototype.valueOf(), false, "Boolean.prototype.valueOf()");

assert.sameValue(1 - Number.prototype, 1, "Number.prototype coerces to 0");
assert.sameValue(Number.prototype == 0, true, "Number.prototype == 0");
assert.sameValue(String.prototype == "", true, "String.prototype == ''");
assert.sameValue(Boolean.prototype == false, true, "Boolean.prototype == false");

assert.sameValue(Number.prototype.toString(), "0", "Number.prototype.toString()");
assert.sameValue(String.prototype.toString(), "", "String.prototype.toString()");
assert.sameValue(Boolean.prototype.toString(), "false", "Boolean.prototype.toString()");

// String.prototype is a String exotic object: length is an own,
// non-writable, non-enumerable, non-configurable 0.
assert.sameValue(String.prototype.length, 0, "String.prototype.length");
assert.sameValue(String.prototype[0], undefined, "String.prototype[0]");

var length = Object.getOwnPropertyDescriptor(String.prototype, "length");
assert.assert(length !== undefined, "String.prototype has own length");
assert.sameValue(length.value, 0, "String.prototype.length value");
assert.sameValue(length.writable, false, "String.prototype.length writable");
assert.sameValue(length.enumerable, false, "String.prototype.length enumerable");
assert.sameValue(length.configurable, false, "String.prototype.length configurable");

// String wrappers may also carry ordinary own properties; those must not be
// shadowed by the String exotic descriptor branch.
var wrapper = new String("ab");
wrapper.foo = 1;
assert.assert(wrapper.hasOwnProperty("foo"), "wrapper hasOwnProperty foo");
assert.assert(wrapper.propertyIsEnumerable("foo"), "wrapper propertyIsEnumerable foo");
assert.sameValue(
    Object.getOwnPropertyDescriptor(wrapper, "foo").value,
    1,
    "wrapper getOwnPropertyDescriptor foo"
);
assert.assert(
    String.prototype.hasOwnProperty("constructor"),
    "String.prototype hasOwnProperty constructor"
);
assert.assert(
    Object.getOwnPropertyDescriptor(String.prototype, "constructor") !== undefined,
    "String.prototype getOwnPropertyDescriptor constructor"
);

// The prototype is still a normal prototype object: its own prototype is
// Object.prototype, it is not its own instance, and constructor links hold.
assert.sameValue(
    Object.getPrototypeOf(Number.prototype),
    Object.prototype,
    "Number.prototype proto"
);
assert.sameValue(Number.prototype instanceof Number, false, "Number.prototype instanceof Number");
assert.sameValue(String.prototype instanceof String, false, "String.prototype instanceof String");
assert.sameValue(
    Boolean.prototype instanceof Boolean,
    false,
    "Boolean.prototype instanceof Boolean"
);
assert.sameValue(Number.prototype.constructor, Number, "Number.prototype.constructor");
assert.sameValue(String.prototype.constructor, String, "String.prototype.constructor");
assert.sameValue(Boolean.prototype.constructor, Boolean, "Boolean.prototype.constructor");

// Wrapper instances keep their own slots and still unbox correctly.
var number = new Number(3);
number.bar = 1;
assert.sameValue(number.valueOf(), 3, "new Number(3).valueOf()");
assert.sameValue(number.bar, 1, "Number wrapper own property");
assert.sameValue(Object(7).valueOf(), 7, "Object(7).valueOf()");
