// #78: built-in functions and constructors expose their spec `length`
// (ES 10.2.10, 10.3), with { writable: false, enumerable: false,
// configurable: true }.

assert.sameValue(Array.length, 1, "Array.length");
assert.sameValue(Object.length, 1, "Object.length");
assert.sameValue(String.length, 1, "String.length");
assert.sameValue(Function.length, 1, "Function.length");
assert.sameValue(Math.max.length, 2, "Math.max.length");
assert.sameValue(Math.abs.length, 1, "Math.abs.length");
assert.sameValue(Function.prototype.bind.length, 1, "bind.length");
assert.sameValue(Function.prototype.apply.length, 2, "apply.length");
assert.sameValue(Array.prototype.push.length, 1, "push.length");
assert.sameValue(Array.prototype.map.length, 1, "map.length");
assert.sameValue(Object.defineProperty.length, 3, "defineProperty.length");
assert.sameValue(Function.prototype.length, 0, "Function.prototype.length");
assert.sameValue(Function.prototype.name, "", "Function.prototype.name");

var descriptor = Object.getOwnPropertyDescriptor(Math.abs, "length");
assert.sameValue(descriptor.value, 1, "length value");
assert.sameValue(descriptor.writable, false, "length writable");
assert.sameValue(descriptor.enumerable, false, "length enumerable");
assert.sameValue(descriptor.configurable, true, "length configurable");
