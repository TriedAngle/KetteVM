// ES B.2.2.1: the Object.prototype __proto__ setter installs the new
// prototype (throwing TypeError when [[SetPrototypeOf]] returns false).
var p = { x: 1 };
var o = {};
o.__proto__ = p;
if (Object.getPrototypeOf(o) !== p) {
    throw new TypeError("__proto__ setter: prototype not set");
}
if (o.x !== 1) {
    throw new TypeError("__proto__ setter: inherited property not visible");
}
