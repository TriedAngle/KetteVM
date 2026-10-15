// ES 10.5.8: a proxy anywhere in the prototype chain intercepts [[Get]]
// for every property; OrdinaryGet reaching the proxy dispatches to the
// get trap with the original receiver.
var p = new Proxy({}, { get: function (t, k) { return 42; } });
var o = Object.create(p);
if (o.x !== 42) {
    throw new TypeError("mid-chain proxy get trap: expected 42, got " + o.x);
}
