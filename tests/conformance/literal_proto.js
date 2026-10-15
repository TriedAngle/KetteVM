// ES 14.6.13: the `__proto__:` form in an object literal sets the
// running object's [[Prototype]] (to null for `null`).
var p = { x: 1 };
var o = { __proto__: p };
if (Object.getPrototypeOf(o) !== p) {
    throw new TypeError("literal __proto__: prototype not set");
}
var n = { __proto__: null };
if (Object.getPrototypeOf(n) !== null) {
    throw new TypeError("literal __proto__: null not applied");
}
if (n.toString !== undefined) {
    throw new TypeError("literal __proto__: null-proto object still sees Object.prototype");
}
