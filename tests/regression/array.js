// ES 23.1 array semantics: constructor, push, length, and element
// property descriptors.

function hasOwn(obj, key) {
    return Object.prototype.hasOwnProperty.call(obj, key);
}

function lengthDescriptor(obj) {
    return Object.getOwnPropertyDescriptor(obj, "length");
}

// ---- Array constructor (23.1.1.1) ----

var empty = Array();
assert.sameValue(Array.isArray(empty), true, "Array() is an array");
assert.sameValue(empty.length, 0, "Array() length");
assert.sameValue(Object.getPrototypeOf(empty), Array.prototype, "Array() prototype");

var object = {};
var one = Array(object);
assert.sameValue(one.length, 1, "Array(object) length");
assert.sameValue(one[0], object, "Array(object)[0]");

var sparse = new Array(3);
assert.sameValue(sparse.length, 3, "Array(3) length");
assert.sameValue(hasOwn(sparse, "0"), false, "Array(3) hole 0");
assert.sameValue(hasOwn(sparse, "2"), false, "Array(3) hole 2");
assert.sameValue(Array(-0).length, 0, "Array(-0) length");

function rangeError(n) {
    assert.throws(RangeError, function () { Array(n); }, "Array(" + n + ")");
}
rangeError(1.5);
rangeError(-1);
rangeError(4294967296);
rangeError(NaN);
rangeError(Infinity);

var many = new Array(1, 2, 3);
assert.sameValue(many.length, 3, "Array(1,2,3) length");
assert.sameValue(many[0], 1, "Array(1,2,3)[0]");
assert.sameValue(many[2], 3, "Array(1,2,3)[2]");

class Sub extends Array {
    first() { return this[0]; }
}
var sub = new Sub(1, 2);
assert.sameValue(Array.isArray(sub), true, "subclass is an array");
assert.sameValue(Object.getPrototypeOf(sub), Sub.prototype, "subclass prototype");
assert.sameValue(sub.length, 2, "subclass length");
assert.sameValue(sub[0], 1, "subclass [0]");
assert.sameValue(sub.first(), 1, "subclass method");
assert.sameValue(sub instanceof Sub, true, "instanceof Sub");
assert.sameValue(sub instanceof Array, true, "instanceof Array");

var subEmpty = new Sub();
assert.sameValue(subEmpty.length, 0, "empty subclass length");
assert.sameValue(Object.getPrototypeOf(subEmpty), Sub.prototype, "empty subclass prototype");

var subSparse = new Sub(2);
assert.sameValue(subSparse.length, 2, "sparse subclass length");
assert.sameValue(hasOwn(subSparse, "0"), false, "sparse subclass hole");
assert.sameValue(Object.getPrototypeOf(subSparse), Sub.prototype, "sparse subclass prototype");

var protoDescriptor = Object.getOwnPropertyDescriptor(Array, "prototype");
assert.sameValue(protoDescriptor.writable, false, "Array.prototype writable");
assert.sameValue(protoDescriptor.enumerable, false, "Array.prototype enumerable");
assert.sameValue(protoDescriptor.configurable, false, "Array.prototype configurable");

// ---- Array.prototype.push (23.1.3.23) ----

var generic = {};
assert.sameValue(Array.prototype.push.call(generic, "a"), 1, "push generic return");
assert.sameValue(generic[0], "a", "push generic index");
assert.sameValue(generic.length, 1, "push generic length");

var arrayLike = { length: 2, 0: "a", 1: "b" };
assert.sameValue(Array.prototype.push.call(arrayLike, "c", "d"), 4, "push multiple return");
assert.sameValue(arrayLike[2], "c", "push multiple [2]");
assert.sameValue(arrayLike[3], "d", "push multiple [3]");

var fraction = { length: 1.5 };
assert.sameValue(Array.prototype.push.call(fraction, "x"), 2, "push fractional length");
assert.sameValue(fraction[1], "x", "push fractional index");

var negative = { length: -1 };
assert.sameValue(Array.prototype.push.call(negative, "x"), 1, "push negative length");
assert.sameValue(negative[0], "x", "push negative index");

var notANumber = { length: NaN };
assert.sameValue(Array.prototype.push.call(notANumber, "x"), 1, "push NaN length");
assert.sameValue(notANumber[0], "x", "push NaN index");

assert.throws(TypeError, function () {
    Array.prototype.push.call({ length: 9007199254740991 }, "x");
}, "push length overflow");
assert.throws(TypeError, function () { Array.prototype.push.call(null, 1); }, "push null");
assert.throws(TypeError, function () { Array.prototype.push.call(undefined, 1); }, "push undefined");
assert.throws(TypeError, function () { Array.prototype.push.call("abc", "d"); }, "push string");
assert.sameValue(Array.prototype.push.call(42, "x"), 1, "push number receiver");
assert.sameValue(Array.prototype.push.call(true), 0, "push boolean receiver");

var holey = [1, , 3];
assert.sameValue(holey.push(4), 4, "push holey return");
assert.sameValue(hasOwn(holey, "1"), false, "push preserves hole");
assert.sameValue(holey[3], 4, "push appends to holey");

var seen = 0;
Object.defineProperty(Array.prototype, 0, {
    set: function (value) { seen = value; },
    configurable: true,
});
var setter = [];
assert.sameValue(setter.push(7), 1, "push prototype setter return");
assert.sameValue(seen, 7, "push prototype setter called");
assert.sameValue(hasOwn(setter, "0"), false, "push prototype setter does not create own index");
delete Array.prototype[0];

var grown = [];
grown.push(1);
var seenFast = 0;
Object.defineProperty(Array.prototype, 1, {
    set: function (value) { seenFast = value; },
    configurable: true,
});
assert.sameValue(grown.push(2), 2, "push fast path return");
assert.sameValue(seenFast, 2, "push fast path setter called");
assert.sameValue(hasOwn(grown, "1"), false, "push fast path does not create own index");
delete Array.prototype[1];

// ---- Array.prototype.pop (23.1.3.22) ----

var popped = [1, 2, 3];
assert.sameValue(popped.pop(), 3, "pop return");
assert.sameValue(popped.length, 2, "pop length");
assert.sameValue(hasOwn(popped, "2"), false, "pop deletes last");
assert.sameValue(popped[1], 2, "pop leaves earlier elements");

var emptyPop = [];
assert.sameValue(emptyPop.pop(), undefined, "pop empty return");
assert.sameValue(emptyPop.length, 0, "pop empty length");

var popReuse = [1, 2];
assert.sameValue(popReuse.pop(), 2, "pop reuse return");
popReuse.push(3);
assert.sameValue(popReuse.length, 2, "pop then push length");
assert.sameValue(popReuse[1], 3, "pop then push value");

var popRegrow = [1, 2];
assert.sameValue(popRegrow.pop(), 2, "pop regrow return");
popRegrow.length = 2;
assert.sameValue(popRegrow.length, 2, "pop regrow length");
assert.sameValue(hasOwn(popRegrow, "1"), false, "pop regrow keeps hole");

var popGeneric = { length: 2, 0: "a", 1: "b" };
assert.sameValue(Array.prototype.pop.call(popGeneric), "b", "pop generic return");
assert.sameValue(popGeneric.length, 1, "pop generic length");
assert.sameValue(hasOwn(popGeneric, "1"), false, "pop generic deletes last");

var popFraction = { length: 1.5, 0: "a" };
assert.sameValue(Array.prototype.pop.call(popFraction), "a", "pop fractional length");
assert.sameValue(popFraction.length, 0, "pop fractional length write");

var popNegative = { length: -1 };
assert.sameValue(Array.prototype.pop.call(popNegative), undefined, "pop negative length");
assert.sameValue(popNegative.length, 0, "pop negative length write");

var popNaN = { length: NaN };
assert.sameValue(Array.prototype.pop.call(popNaN), undefined, "pop NaN length");
assert.sameValue(popNaN.length, 0, "pop NaN length write");

assert.throws(TypeError, function () { Array.prototype.pop.call(null); }, "pop null");
assert.throws(TypeError, function () { Array.prototype.pop.call(undefined); }, "pop undefined");
assert.throws(TypeError, function () { Array.prototype.pop.call("abc"); }, "pop string");
assert.sameValue(Array.prototype.pop.call(42), undefined, "pop number receiver");
assert.sameValue(Array.prototype.pop.call(true), undefined, "pop boolean receiver");

var popHoley = [1, , 3];
popHoley.length = 2;
Array.prototype[1] = "proto";
assert.sameValue(popHoley.pop(), "proto", "pop hole through prototype");
assert.sameValue(popHoley.length, 1, "pop hole length");
assert.sameValue(hasOwn(popHoley, "1"), false, "pop hole does not create own index");
delete Array.prototype[1];

var popNonConfig = [1];
Object.defineProperty(popNonConfig, "0", { configurable: false });
assert.throws(TypeError, function () { popNonConfig.pop(); }, "pop non-configurable element");
assert.sameValue(popNonConfig.length, 1, "pop non-configurable keeps length");
assert.sameValue(popNonConfig[0], 1, "pop non-configurable keeps element");

var popReadonlyLength = [1, 2];
Object.defineProperty(popReadonlyLength, "length", { writable: false });
assert.throws(TypeError, function () { popReadonlyLength.pop(); }, "pop read-only length");
assert.sameValue(popReadonlyLength.length, 2, "pop read-only length unchanged");
assert.sameValue(hasOwn(popReadonlyLength, "1"), false, "pop deletes before length write");

var popEmptyReadonly = [];
Object.defineProperty(popEmptyReadonly, "length", { writable: false });
assert.throws(TypeError, function () { popEmptyReadonly.pop(); }, "pop empty read-only length");
assert.sameValue(popEmptyReadonly.length, 0, "pop empty read-only length unchanged");

var popGetterCalls = 0;
var popGetter = { length: 2 };
Object.defineProperty(popGetter, "1", {
    get: function () { popGetterCalls++; return 7; },
    configurable: true,
});
assert.sameValue(Array.prototype.pop.call(popGetter), 7, "pop element getter");
assert.sameValue(popGetterCalls, 1, "pop element getter called once");
assert.sameValue(popGetter.length, 1, "pop element getter length");

var popLengthSeen = -1;
var popSetter = { length: 1, 0: "x" };
Object.defineProperty(popSetter, "length", {
    get: function () { return 1; },
    set: function (value) { popLengthSeen = value; },
    configurable: true,
});
assert.sameValue(Array.prototype.pop.call(popSetter), "x", "pop length setter return");
assert.sameValue(popLengthSeen, 0, "pop length setter receives new length");

var popGetterOnlyLength = { length: 1, 0: "x" };
Object.defineProperty(popGetterOnlyLength, "length", {
    get: function () { return 1; },
    configurable: true,
});
assert.throws(TypeError, function () {
    Array.prototype.pop.call(popGetterOnlyLength);
}, "pop set on getter-only length");

var popBig = { length: 4294967296, 4294967295: "last" };
assert.sameValue(Array.prototype.pop.call(popBig), "last", "pop large length return");
assert.sameValue(popBig.length, 4294967295, "pop large length update");
assert.sameValue(hasOwn(popBig, "4294967295"), false, "pop large length delete");

var popProxyLog = [];
var popProxyTarget = { length: 2, 0: "a", 1: "b" };
var popProxy = new Proxy(popProxyTarget, {
    get: function (target, key) {
        popProxyLog.push("get " + key);
        return target[key];
    },
    deleteProperty: function (target, key) {
        popProxyLog.push("delete " + key);
        return delete target[key];
    },
    set: function (target, key, value) {
        popProxyLog.push("set " + key + "=" + value);
        target[key] = value;
        return true;
    },
});
assert.sameValue(Array.prototype.pop.call(popProxy), "b", "pop proxy return");
assert.sameValue(popProxyTarget.length, 1, "pop proxy length");
assert.sameValue(hasOwn(popProxyTarget, "1"), false, "pop proxy deletes index");
assert.sameValue(popProxyLog.join("|"), "get length|get 1|delete 1|set length=1", "pop proxy order");

// ---- non-extensible receivers (10.1.6.1) ----

var sealed = [];
Object.preventExtensions(sealed);
assert.sameValue(Object.isExtensible(sealed), false, "array is non-extensible");
assert.throws(TypeError, function () { sealed.push(1); }, "push non-extensible array");
assert.throws(TypeError, function () {
    Object.defineProperty(sealed, "0", { value: 1 });
}, "define on non-extensible array");
assert.throws(TypeError, function () {
    "use strict";
    sealed[0] = 1;
}, "strict assign on non-extensible array");
sealed[0] = 1;
assert.sameValue(sealed.length, 0, "sloppy assign does not grow non-extensible array");
assert.sameValue(hasOwn(sealed, "0"), false, "sloppy assign does not create element");

var sealedObject = {};
Object.preventExtensions(sealedObject);
assert.throws(TypeError, function () {
    Object.defineProperty(sealedObject, "0", { value: 1 });
}, "define on non-extensible object");
sealedObject[0] = 1;
assert.sameValue(hasOwn(sealedObject, "0"), false, "sloppy assign on non-extensible object");

var existing = [1, 2];
Object.preventExtensions(existing);
existing[0] = 10;
assert.sameValue(existing[0], 10, "update existing element on non-extensible array");
Object.defineProperty(existing, "1", {
    value: 20,
    writable: true,
    enumerable: true,
    configurable: true,
});
assert.sameValue(existing[1], 20, "redefine existing element on non-extensible array");

// ---- element property descriptors (10.1.6.3) ----

var element = [1];
Object.defineProperty(element, "0", { configurable: false });
assert.throws(TypeError, function () {
    Object.defineProperty(element, "0", { configurable: true });
}, "element cannot become configurable");
assert.throws(TypeError, function () {
    Object.defineProperty(element, "0", { enumerable: false });
}, "element enumerable is fixed");
Object.defineProperty(element, "0", { value: 2 });
assert.sameValue(element[0], 2, "element value changes while writable");
Object.defineProperty(element, "0", { writable: false });
assert.sameValue(Object.getOwnPropertyDescriptor(element, "0").writable, false,
    "element writable true -> false");
assert.throws(TypeError, function () {
    Object.defineProperty(element, "0", { value: 3 });
}, "element value fixed when non-writable");
assert.throws(TypeError, function () {
    Object.defineProperty(element, "0", { writable: true });
}, "element writable false -> true");

var accessorElement = [1];
function getSeven() { return 7; }
Object.defineProperty(accessorElement, "0", { get: getSeven, configurable: false });
assert.sameValue(accessorElement[0], 7, "accessor element getter");
assert.throws(TypeError, function () {
    Object.defineProperty(accessorElement, "0", { configurable: true });
}, "accessor element cannot become configurable");
assert.throws(TypeError, function () {
    Object.defineProperty(accessorElement, "0", { get: function () { return 8; } });
}, "accessor element getter fixed");

// ---- Array length writability (10.4.2.4) ----

var lengthArray = [1, 2, 3];
assert.sameValue(lengthDescriptor(lengthArray).writable, true, "length starts writable");
Object.defineProperty(lengthArray, "length", { writable: false });
assert.sameValue(lengthDescriptor(lengthArray).writable, false, "length becomes read-only");
assert.throws(TypeError, function () {
    Object.defineProperty(lengthArray, "length", { writable: true });
}, "length cannot become writable");
assert.throws(TypeError, function () {
    "use strict";
    lengthArray.length = 1;
}, "strict assign read-only length");
lengthArray.length = 1;
assert.sameValue(lengthArray.length, 3, "sloppy assign read-only length");
assert.throws(TypeError, function () {
    Object.defineProperty(lengthArray, "length", { value: 1 });
}, "shrink read-only length");
assert.throws(TypeError, function () {
    Object.defineProperty(lengthArray, "length", { value: 4 });
}, "grow read-only length");
Object.defineProperty(lengthArray, "length", { value: 3 });
assert.sameValue(lengthArray.length, 3, "same-value length define");
assert.throws(TypeError, function () { lengthArray.push(4); }, "push read-only length");
assert.throws(TypeError, function () {
    Object.defineProperty(lengthArray, "3", { value: 4 });
}, "define element with read-only length");
assert.throws(TypeError, function () {
    "use strict";
    lengthArray[3] = 4;
}, "assign element with read-only length");
lengthArray[0] = 10;
assert.sameValue(lengthArray[0], 10, "existing element with read-only length");

var shrink = [1, 2, 3];
Object.defineProperty(shrink, "length", { value: 2, writable: false });
assert.sameValue(shrink.length, 2, "shrink with writable false");
assert.sameValue(lengthDescriptor(shrink).writable, false, "shrink leaves read-only");

var grow = [1, 2, 3];
Object.defineProperty(grow, "length", { value: 5, writable: false });
assert.sameValue(grow.length, 5, "grow with writable false");
assert.sameValue(lengthDescriptor(grow).writable, false, "grow leaves read-only");

var clamped = [1, 2, 3];
Object.defineProperty(clamped, "0", { configurable: false });
assert.throws(TypeError, function () {
    Object.defineProperty(clamped, "length", { value: 0 });
}, "non-configurable element blocks shrink");
assert.sameValue(clamped.length, 1, "length clamps to non-configurable element");
