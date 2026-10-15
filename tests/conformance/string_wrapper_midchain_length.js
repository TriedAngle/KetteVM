// ES 10.4.3: a String wrapper's `length` is an own property, found by
// OrdinaryGet when the wrapper sits mid-chain. Object.create is used
// because the literal `__proto__:` form is currently a no-op (#7).
var o = Object.create(Object("ab"));
if (o.length !== 2) {
    throw new TypeError("mid-chain string wrapper length: expected 2, got " + o.length);
}
