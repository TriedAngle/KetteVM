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

// ---- Array iterator next (23.1.5.2.1) ----

var iterator = [10, 20].values();
var step = iterator.next();
assert.sameValue(step.value, 10, "iterator first value");
assert.sameValue(step.done, false, "iterator first done");
step = iterator.next();
assert.sameValue(step.value, 20, "iterator second value");
assert.sameValue(step.done, false, "iterator second done");
step = iterator.next();
assert.sameValue(step.value, undefined, "iterator exhausted value");
assert.sameValue(step.done, true, "iterator exhausted done");
step = iterator.next();
assert.sameValue(step.value, undefined, "iterator stays exhausted");
assert.sameValue(step.done, true, "iterator stays done");

var growArray = [1];
var growIt = growArray.values();
assert.sameValue(growIt.next().value, 1, "iterator regrow first");
assert.sameValue(growIt.next().done, true, "iterator regrow exhausted");
growArray.push(2);
assert.sameValue(growIt.next().done, true, "iterator does not resume after done");

var holeIter = [1, , 3].values();
Array.prototype[1] = "proto";
assert.sameValue(holeIter.next().value, 1, "iterator hole before");
assert.sameValue(holeIter.next().value, "proto", "iterator hole reads prototype");
assert.sameValue(holeIter.next().value, 3, "iterator hole after");
delete Array.prototype[1];

var arrayLikeIter = Array.prototype.values.call({ length: 2, 0: "a", 1: "b" });
assert.sameValue(arrayLikeIter.next().value, "a", "iterator array-like first");
assert.sameValue(arrayLikeIter.next().value, "b", "iterator array-like second");
assert.sameValue(arrayLikeIter.next().done, true, "iterator array-like done");

var dynamicLike = { length: 1, 0: "a" };
var dynamicIter = Array.prototype.values.call(dynamicLike);
assert.sameValue(dynamicIter.next().value, "a", "iterator dynamic first");
dynamicLike.length = 2;
dynamicLike[1] = "b";
assert.sameValue(dynamicIter.next().value, "b", "iterator rereads length");
assert.sameValue(dynamicIter.next().done, true, "iterator dynamic done");

var stringIter = Array.prototype.values.call("ab");
assert.sameValue(stringIter.next().value, "a", "iterator string first");
assert.sameValue(stringIter.next().value, "b", "iterator string second");
assert.sameValue(stringIter.next().done, true, "iterator string done");

var numberIter = Array.prototype.values.call(42);
assert.sameValue(numberIter.next().done, true, "iterator number empty");

assert.throws(TypeError, function () { Array.prototype.values.call(null); }, "iterator values null");
assert.throws(TypeError, function () { Array.prototype.values.call(undefined); }, "iterator values undefined");

var realIter = [1].values();
var realNext = Object.getPrototypeOf(realIter).next;
assert.throws(TypeError, function () { realNext.call({}); }, "iterator next plain object");
assert.throws(TypeError, function () { realNext.call("abc"); }, "iterator next primitive string");

var throwingArray = [];
throwingArray.length = 1;
Object.defineProperty(throwingArray, "0", {
    get: function () { throw new Error("boom"); },
    configurable: true,
});
var throwingIter = throwingArray.values();
assert.throws(Error, function () { throwingIter.next(); }, "iterator element getter throws");
assert.sameValue(throwingIter.next().done, true, "iterator advances before element get");

// ---- Iterator @@iterator (27.1.3.3.18) ----

var iterSelf = [1].values();
assert.sameValue(iterSelf[Symbol.iterator](), iterSelf, "iterator returns itself");
var iterProto = Object.getPrototypeOf(iterSelf);
var iterBase = Object.getPrototypeOf(iterProto);
assert.sameValue(hasOwn(iterProto, Symbol.iterator), false, "array iterator does not own @@iterator");
assert.sameValue(hasOwn(iterBase, Symbol.iterator), true, "iterator prototype owns @@iterator");
assert.sameValue(iterBase[Symbol.iterator].call(5), 5, "iterator method returns primitive receiver");

// ---- Array.prototype.join (23.1.3.18) ----

assert.sameValue([1, 2, 3].join(), "1,2,3", "join default separator");
assert.sameValue([1, 2, 3].join("-"), "1-2-3", "join custom separator");
assert.sameValue([1, , 3].join("-"), "1--3", "join hole");
assert.sameValue([1, undefined, null].join("-"), "1--", "join undefined and null");
assert.sameValue([].join(), "", "join empty");

var joinSeparator = { toString: function () { return ":"; } };
assert.sameValue([1, 2].join(joinSeparator), "1:2", "join coerces separator");
assert.throws(TypeError, function () { [1].join(Symbol()); }, "join symbol separator");

var joinElement = { toString: function () { return "x"; } };
assert.sameValue([joinElement, 2].join("-"), "x-2", "join coerces element");
assert.throws(TypeError, function () { [Symbol()].join(); }, "join symbol element");

var joinGeneric = { length: 2, 0: "a", 1: "b" };
assert.sameValue(Array.prototype.join.call(joinGeneric, "-"), "a-b", "join generic object");
assert.sameValue(Array.prototype.join.call("abc", "-"), "a-b-c", "join string receiver");
assert.sameValue(Array.prototype.join.call(42), "", "join number receiver");
assert.throws(TypeError, function () { Array.prototype.join.call(null); }, "join null");
assert.throws(TypeError, function () { Array.prototype.join.call(undefined); }, "join undefined");

var joinHoley = [1, , 3];
Array.prototype[1] = "p";
assert.sameValue(joinHoley.join("-"), "1-p-3", "join reads prototype");
delete Array.prototype[1];

var joinGetterCalls = 0;
var joinGetter = { length: 2 };
Object.defineProperty(joinGetter, "0", {
    get: function () { joinGetterCalls++; return "g"; },
    configurable: true,
});
assert.sameValue(Array.prototype.join.call(joinGetter, "-"), "g-", "join getter");
assert.sameValue(joinGetterCalls, 1, "join getter called once");

// ---- Array.prototype.toString (23.1.3.36) ----

assert.sameValue([1, 2, 3].toString(), "1,2,3", "toString default join");
assert.sameValue([].toString(), "", "toString empty");
var toStringCustom = [1, 2];
toStringCustom.join = function () { return "custom"; };
assert.sameValue(toStringCustom.toString(), "custom", "toString uses join");
var toStringJoinable = {
    length: 2,
    0: "a",
    1: "b",
    join: function () { return "joined"; },
};
assert.sameValue(Array.prototype.toString.call(toStringJoinable), "joined", "toString calls join");
assert.sameValue(Array.prototype.toString.call({ length: 2, 0: "a", 1: "b" }), "[object Object]",
    "toString falls back to object tag");
assert.sameValue(Array.prototype.toString.call({ join: 5 }), "[object Object]",
    "toString non-callable join");
assert.sameValue(Array.prototype.toString.call(true), "[object Boolean]",
    "toString boolean receiver");
assert.sameValue(Array.prototype.toString.call("ab"), "[object String]",
    "toString string receiver");
assert.sameValue(Array.prototype.toString.call(42), "[object Number]",
    "toString number receiver");
assert.throws(TypeError, function () { Array.prototype.toString.call(null); }, "toString null");
assert.throws(TypeError, function () { Array.prototype.toString.call(undefined); }, "toString undefined");

// ---- Array.prototype.sort (23.1.3.30) ----

var sortDefault = [3, 1, 2];
assert.sameValue(sortDefault.sort(), sortDefault, "sort returns receiver");
assert.sameValue(sortDefault.join(","), "1,2,3", "sort default order");

var sortNumeric = [10, 2, 1];
sortNumeric.sort(function (a, b) { return a - b; });
assert.sameValue(sortNumeric.join(","), "1,2,10", "sort numeric comparator");

var sortHoles = [3, , 1, undefined, 2];
sortHoles.sort();
assert.sameValue(sortHoles.length, 5, "sort keeps length");
assert.sameValue(sortHoles.join(","), "1,2,3,,", "sort undefined before holes");
assert.sameValue(hasOwn(sortHoles, "1"), true, "sort fills defined slot");
assert.sameValue(hasOwn(sortHoles, "4"), false, "sort leaves tail hole");

var sortLike = { length: 3, 0: "c", 1: "a", 2: "b" };
Array.prototype.sort.call(sortLike);
assert.sameValue(sortLike.length, 3, "sort array-like length");
assert.sameValue(sortLike[0] + sortLike[1] + sortLike[2], "abc", "sort array-like values");

var sortStable = [{ k: 1, v: "a" }, { k: 0, v: "b" }, { k: 1, v: "c" }, { k: 0, v: "d" }];
sortStable.sort(function (a, b) { return a.k - b.k; });
assert.sameValue(
    sortStable[0].v + sortStable[1].v + sortStable[2].v + sortStable[3].v,
    "bdac",
    "sort is stable"
);

var sortProto = [3, , 1];
Array.prototype[1] = 5;
sortProto.sort();
assert.sameValue(sortProto.join(","), "1,3,5", "sort reads prototype hole");
delete Array.prototype[1];

var sortThrows = [2, 1];
assert.throws(Error, function () {
    sortThrows.sort(function () { throw new Error("boom"); });
}, "sort propagates comparator exception");

var sortSymbol = [2, 1];
assert.throws(TypeError, function () {
    sortSymbol.sort(function () { return Symbol(); });
}, "sort comparator symbol");

assert.throws(TypeError, function () { Array.prototype.sort.call(null); }, "sort null");
assert.throws(TypeError, function () { Array.prototype.sort.call(undefined); }, "sort undefined");

// ---- Array.prototype.map (23.1.3.21) ----

var mapInput = [1, 2, 3];
var mapSeen = [];
var mapOutput = mapInput.map(function (value, index, array) {
    mapSeen.push(index + ":" + array.length);
    return value * 2;
});
assert.sameValue(mapOutput !== mapInput, true, "map creates a new array");
assert.sameValue(mapOutput.join(","), "2,4,6", "map values");
assert.sameValue(mapSeen.join(","), "0:3,1:3,2:3", "map callback arguments");

var mapThis = [1].map(function () { return this.x; }, { x: 7 });
assert.sameValue(mapThis[0], 7, "map thisArg");

var mapHoles = [1, , 3].map(function (value) { return value * 2; });
assert.sameValue(mapHoles.length, 3, "map hole length");
assert.sameValue(hasOwn(mapHoles, "1"), false, "map preserves holes");
assert.sameValue(mapHoles[0], 2, "map hole before");
assert.sameValue(mapHoles[2], 6, "map hole after");

var mapGeneric = { length: 2, 0: "a", 1: "b" };
var mapGenericResult = Array.prototype.map.call(mapGeneric, function (value) {
    return value + "!";
});
assert.sameValue(mapGenericResult.join(","), "a!,b!", "map generic object");

var mapString = Array.prototype.map.call("ab", function (value) { return value; });
assert.sameValue(mapString.join(","), "a,b", "map string receiver");

var mapMutation = [1, 2];
var mapMutationResult = mapMutation.map(function (value, index, array) {
    if (index === 0) {
        array[1] = 9;
    }
    return value;
});
assert.sameValue(mapMutationResult.join(","), "1,9", "map observes mutations when visited");

assert.throws(TypeError, function () { [1].map(5); }, "map non-callable callback");
assert.throws(TypeError, function () { Array.prototype.map.call(null, function () {}); }, "map null receiver");
assert.throws(Error, function () {
    [1].map(function () { throw new Error("map"); });
}, "map callback exception");

var hugeMapCalls = 0;
assert.throws(RangeError, function () {
    Array.prototype.map.call({ length: 4294967296 }, function () { hugeMapCalls += 1; });
}, "map rejects array length above 2^32 - 1");
assert.sameValue(hugeMapCalls, 0, "map length guard runs before callbacks");

var hugeSlice = { length: 4294967296 };
assert.throws(RangeError, function () {
    Array.prototype.slice.call(hugeSlice, 0, 4294967296);
}, "slice rejects result length above 2^32 - 1");

// ---- Array.prototype.reduce (23.1.3.24) ----

assert.sameValue([1, 2, 3].reduce(function (a, b) { return a + b; }), 6, "reduce sum");
assert.sameValue([1, 2, 3].reduce(function (a, b) { return a + b; }, 10), 16, "reduce initial");
assert.sameValue([, , 3].reduce(function (a, b) { return a + b; }), 3, "reduce skips holes");
assert.sameValue([].reduce(function () { return 1; }, 10), 10, "reduce empty initial");
assert.throws(TypeError, function () { [].reduce(function () {}); }, "reduce empty without initial");

var reduceSeen = [];
var reduceValue = [1, 2].reduce(function (accumulator, value, index, array) {
    reduceSeen.push(accumulator + ":" + value + ":" + index + ":" + array.length);
    return accumulator + value;
}, 10);
assert.sameValue(reduceValue, 13, "reduce callback value");
assert.sameValue(reduceSeen.join(","), "10:1:0:2,11:2:1:2", "reduce callback arguments");

var reduceGeneric = { length: 2, 0: 2, 1: 3 };
assert.sameValue(Array.prototype.reduce.call(reduceGeneric, function (a, b) {
    return a * b;
}, 1), 6, "reduce generic object");

assert.throws(TypeError, function () { [1].reduce(5); }, "reduce non-callable callback");
assert.throws(TypeError, function () { Array.prototype.reduce.call(null, function () {}); }, "reduce null receiver");
assert.throws(Error, function () {
    [1, 2].reduce(function () { throw new Error("reduce"); });
}, "reduce callback exception");

var reduceMutation = [1, 2];
var reduceMutationResult = reduceMutation.reduce(function (accumulator, value, index, array) {
    if (index === 0) {
        array[1] = 9;
    }
    return accumulator + value;
}, 10);
assert.sameValue(reduceMutationResult, 20, "reduce observes mutations when visited");

// ---- Array.isArray (23.1.2.3) ----

assert.sameValue(Array.isArray([]), true, "isArray array");
assert.sameValue(Array.isArray({}), false, "isArray object");
assert.sameValue(Array.isArray("ab"), false, "isArray string");
assert.sameValue(Array.isArray(null), false, "isArray null");
assert.sameValue(Array.isArray(), false, "isArray missing");

var proxyArray = new Proxy([], {});
assert.sameValue(Array.isArray(proxyArray), true, "isArray proxy array");
var nestedProxyArray = new Proxy(proxyArray, {});
assert.sameValue(Array.isArray(nestedProxyArray), true, "isArray nested proxy array");
assert.sameValue(Array.isArray(new Proxy({}, {})), false, "isArray proxy object");

var revocableArray = Proxy.revocable([], {});
revocableArray.revoke();
assert.throws(TypeError, function () { Array.isArray(revocableArray.proxy); }, "isArray revoked proxy");

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

// ArraySetLength (ES 10.4.2.4) via the `length` setter: the value is coerced
// by ToUint32 and again by ToNumber, so an object runs valueOf twice.
var coercions = 0;
var coerced = [];
coerced.length = {
    valueOf: function () {
        coercions += 1;
        return 1;
    },
};
assert.sameValue(coercions, 2, "length setter coerces the value twice");
assert.sameValue(coerced.length, 1, "length setter applies the coerced value");

var invalid = [];
assert.throws(RangeError, function () {
    invalid.length = 1.5;
}, "fractional length is a RangeError");
assert.throws(RangeError, function () {
    invalid.length = -1;
}, "negative length is a RangeError");
assert.throws(RangeError, function () {
    invalid.length = 4294967296;
}, "length above 2^32-1 is a RangeError");

// A failed shrink rejects the store: strict code throws, sloppy stays silent.
function strictShrink(target) {
    "use strict";
    target.length = 0;
}
var blocked = [1, 2, 3];
Object.defineProperty(blocked, "1", { value: 2, configurable: false });
assert.throws(TypeError, function () {
    strictShrink(blocked);
}, "strict shrink past a non-configurable element throws");
assert.sameValue(blocked.length, 2, "failed shrink clamps the length");

var blockedSloppy = [1, 2, 3];
Object.defineProperty(blockedSloppy, "1", { value: 2, configurable: false });
blockedSloppy.length = 0;
assert.sameValue(blockedSloppy.length, 2, "sloppy failed shrink is silent");

// The length descriptor is read after the coercions, so valueOf making it
// read-only rejects the store (ES 10.4.2.4 steps 12-13).
var frozen = [1, 2, 3];
var frozenCoercions = 0;
frozen.length = {
    valueOf: function () {
        if (frozenCoercions++ === 0) {
            Object.defineProperty(frozen, "length", { writable: false });
        }
        return 1;
    },
};
assert.sameValue(frozen.length, 3, "read-only length set during coercion rejects");
