// ES 20.2.3.5: Object.create accepts any object as prototype, including
// a String wrapper; the created object inherits the wrapper's own
// properties ("length", indices).
var proto = Object("ab");
var o = Object.create(proto);
if (o.length !== 2) {
    throw new TypeError("Object.create(String wrapper).length: expected 2, got " + o.length);
}
