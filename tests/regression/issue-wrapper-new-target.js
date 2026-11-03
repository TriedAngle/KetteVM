// OrdinaryCreateFromConstructor for the primitive wrappers: `newTarget.prototype`
// must become the wrapper's [[Prototype]], falling back to %X.prototype%.

class MyBoolean extends Boolean {}
class MyNumber extends Number {}
class MyString extends String {}

var b = new MyBoolean(true);
assert.sameValue(Object.getPrototypeOf(b), MyBoolean.prototype, "Boolean proto");
assert.sameValue(b instanceof MyBoolean, true, "Boolean instanceof");
assert.sameValue(b.valueOf(), true, "Boolean valueOf");
assert.sameValue(b.toString(), "true", "Boolean toString");
assert.sameValue(new MyBoolean(false).toString(), "false", "Boolean toString false");
assert.sameValue(Object.prototype.toString.call(b), "[object Boolean]", "Boolean tag");

var n = new MyNumber(3.5);
assert.sameValue(Object.getPrototypeOf(n), MyNumber.prototype, "Number proto");
assert.sameValue(n instanceof MyNumber, true, "Number instanceof");
assert.sameValue(n.valueOf(), 3.5, "Number valueOf");
assert.sameValue(n + 1, 4.5, "Number coercion");

var s = new MyString("ab");
assert.sameValue(Object.getPrototypeOf(s), MyString.prototype, "String proto");
assert.sameValue(s instanceof MyString, true, "String instanceof");
assert.sameValue(s.valueOf(), "ab", "String valueOf");
assert.sameValue(s.length, 2, "String length");

// Instances of the base constructor keep the intrinsic prototype.
assert.sameValue(Object.getPrototypeOf(new Boolean(true)), Boolean.prototype, "base Boolean");
assert.sameValue(Object.getPrototypeOf(new Number(1)), Number.prototype, "base Number");
assert.sameValue(Object.getPrototypeOf(new String("a")), String.prototype, "base String");

// A non-object `newTarget.prototype` falls back to %Boolean.prototype% — not
// reachable from JS until `Reflect.construct` exists (TODO), so only the
// intrinsic fallback is covered here.
assert.sameValue(Object.getPrototypeOf(new Boolean(true)), Boolean.prototype, "base Boolean");

// 20.3.2.1 / 21.1.2.15 / 22.1.2.3: { writable: false, enumerable: false, configurable: false }
var wrappers = [Boolean, Number, String];
for (var i = 0; i < wrappers.length; i++) {
    var ctor = wrappers[i];
    var d = Object.getOwnPropertyDescriptor(ctor, "prototype");
    assert.sameValue(d.writable, false, ctor.name + ".prototype writable");
    assert.sameValue(d.enumerable, false, ctor.name + ".prototype enumerable");
    assert.sameValue(d.configurable, false, ctor.name + ".prototype configurable");
    // builtin function `name`: { writable: false, enumerable: false, configurable: true }
    var n = Object.getOwnPropertyDescriptor(ctor, "name");
    assert.sameValue(n.value, ctor.name, ctor.name + ".name value");
    assert.sameValue(n.writable, false, ctor.name + ".name writable");
    assert.sameValue(n.enumerable, false, ctor.name + ".name enumerable");
    assert.sameValue(n.configurable, true, ctor.name + ".name configurable");
}

// prototype.constructor and the prototype methods: { writable: true, enumerable: false, configurable: true }
var c = Object.getOwnPropertyDescriptor(Boolean.prototype, "constructor");
assert.sameValue(c.writable, true, "constructor writable");
assert.sameValue(c.enumerable, false, "constructor enumerable");
assert.sameValue(c.configurable, true, "constructor configurable");
var v = Object.getOwnPropertyDescriptor(Boolean.prototype, "valueOf");
assert.sameValue(v.writable, true, "valueOf writable");
assert.sameValue(v.enumerable, false, "valueOf enumerable");
assert.sameValue(v.configurable, true, "valueOf configurable");

// Receiver checks are unchanged by the derived maps.
assert.throws(TypeError, function () {
    Boolean.prototype.valueOf.call({});
}, "valueOf on plain object");
assert.throws(TypeError, function () {
    Boolean.prototype.valueOf.call(new Number(1));
}, "valueOf on Number wrapper");
assert.sameValue(Boolean.prototype.valueOf.call(false), false, "valueOf on primitive");
assert.throws(TypeError, function () {
    Boolean.prototype.toString.call(0);
}, "toString on a Number");
assert.sameValue(Boolean.prototype.toString.call(false), "false", "toString on primitive");
// A subclass prototype is an ordinary object: it has no [[BooleanData]].
assert.throws(TypeError, function () {
    MyBoolean.prototype.valueOf();
}, "valueOf on subclass prototype");

// Builtin constants are read-only; builtin functions are non-enumerable.
function attrs(object, key) {
    var d = Object.getOwnPropertyDescriptor(object, key);
    return d.writable + "," + d.enumerable + "," + d.configurable;
}
assert.sameValue(attrs(Number, "NaN"), "false,false,false", "Number.NaN attrs");
assert.sameValue(attrs(Number, "POSITIVE_INFINITY"), "false,false,false", "Number.POSITIVE_INFINITY attrs");
assert.sameValue(attrs(Math, "PI"), "false,false,false", "Math.PI attrs");
assert.sameValue(attrs(Symbol, "iterator"), "false,false,false", "Symbol.iterator attrs");
assert.sameValue(attrs(Object, "prototype"), "false,false,false", "Object.prototype attr");
assert.sameValue(attrs(Function, "prototype"), "false,false,false", "Function.prototype attr");
assert.sameValue(attrs(Date, "prototype"), "false,false,false", "Date.prototype attr");
assert.sameValue(attrs(Array, "isArray"), "true,false,true", "Array.isArray attrs");
assert.sameValue(attrs(Date, "now"), "true,false,true", "Date.now attrs");
assert.sameValue(attrs(Number.prototype, "toFixed"), "true,false,true", "Number.prototype.toFixed attrs");
assert.sameValue(Object.getOwnPropertyDescriptor(this, "Boolean").enumerable, false, "global Boolean");
assert.sameValue(Object.getOwnPropertyDescriptor(this, "Number").enumerable, false, "global Number");
assert.sameValue(Object.getOwnPropertyDescriptor(this, "String").enumerable, false, "global String");
assert.sameValue(Object.getOwnPropertyDescriptor(this, "Math").enumerable, false, "global Math");
assert.sameValue(Object.getOwnPropertyDescriptor(this, "Object").enumerable, false, "global Object");
assert.sameValue(Date.now.name, "now", "Date.now name");
assert.sameValue(Array.isArray.name, "isArray", "Array.isArray name");
assert.sameValue(Number.prototype.toFixed.name, "toFixed", "Number.prototype.toFixed name");
