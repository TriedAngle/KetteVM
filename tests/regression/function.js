// ES 20.2: Function.prototype toString/call/apply/bind and Function().

var toString = Function.prototype.toString;
var call = Function.prototype.call;
var apply = Function.prototype.apply;
var bind = Function.prototype.bind;

// Function.prototype.toString (ES 20.2.3.5): NativeFunction syntax
// "function [name]([params]) { [native code] }".
assert.sameValue(
    toString.call(function () {}),
    "function () { [native code] }",
    "anonymous function source"
);
assert.sameValue(
    toString.call(function named() {}),
    "function named() { [native code] }",
    "named function source"
);
assert.sameValue(
    toString.call(Math.max),
    "function max() { [native code] }",
    "builtin source"
);
assert.sameValue(
    toString.call(toString),
    "function toString() { [native code] }",
    "builtin method source"
);
assert.throws(TypeError, function () {
    toString.call({});
}, "toString of an ordinary object");
assert.throws(TypeError, function () {
    toString.call(1);
}, "toString of a number");
assert.throws(TypeError, function () {
    toString.call(undefined);
}, "toString of undefined");

// Function.prototype.call (ES 20.2.3.3).
assert.sameValue(
    (function (a, b) {
        return this.base + a + b;
    }).call({ base: 1 }, 2, 3),
    6,
    "call forwards this and arguments"
);
assert.sameValue(
    (function (a, b, c) {
        return a + b + c;
    }).call(null, 1, 2, 3),
    6,
    "call forwards every argument"
);
assert.sameValue(
    (function () {
        return this;
    }).call(42),
    42,
    "call passes thisArg unmodified"
);
assert.throws(TypeError, function () {
    call.call({}, null);
}, "call on a non-callable");

// Function.prototype.apply (ES 20.2.3.1).
assert.sameValue(Math.max.apply(null, [1, 5, 3]), 5, "apply with an array");
assert.sameValue(
    Math.max.apply(null, { length: 3, 0: 4, 1: 9, 2: 2 }),
    9,
    "apply with an array-like"
);
assert.sameValue(
    (function (a) {
        return a === undefined;
    }).apply(null, [, 1]),
    true,
    "apply reads holes as undefined"
);
assert.sameValue(
    (function (a) {
        return a === undefined;
    }).apply(null, null),
    true,
    "apply with null argArray"
);
assert.sameValue(
    (function (a) {
        return a === undefined;
    }).apply(null, undefined),
    true,
    "apply with undefined argArray"
);
var lengthReads = 0;
assert.sameValue(
    (function (a, b) {
        return a + b;
    }).apply(null, {
        get length() {
            lengthReads += 1;
            return 2;
        },
        0: 10,
        1: 20,
    }),
    30,
    "apply uses Get(length) and Get(index)"
);
assert.sameValue(lengthReads, 1, "apply reads length once");
assert.sameValue(
    Math.max.apply(null, new Proxy({ length: 2, 0: 4, 1: 8 }, {})),
    8,
    "apply through a proxy"
);
assert.throws(TypeError, function () {
    (function () {}).apply(null, 1);
}, "apply with a primitive argArray");
assert.throws(TypeError, function () {
    (function () {}).apply(null, "ab");
}, "apply with a string argArray");
assert.throws(TypeError, function () {
    apply.apply({}, null);
}, "apply on a non-callable");

// Function.prototype.bind (ES 20.2.3.2).
var bound = function target(a, b) {
    return this.base + a + b;
}.bind({ base: 1 }, 2);
assert.sameValue(bound(3), 6, "bind fixes thisArg and leading arguments");
assert.sameValue(bound.length, 1, "bound length counts the bound arguments");
assert.sameValue(bound.name, "bound target", "bound name");
assert.sameValue(
    (function target() {}).bind(null).bind(null).name,
    "bound bound target",
    "chained bound name"
);
assert.sameValue(
    Object.getOwnPropertyDescriptor(function () {}.bind(null), "prototype"),
    undefined,
    "a bound function has no prototype property"
);
var lengthDesc = Object.getOwnPropertyDescriptor(function (a) {}.bind(null), "length");
assert.sameValue(lengthDesc.writable, false, "bound length writable");
assert.sameValue(lengthDesc.enumerable, false, "bound length enumerable");
assert.sameValue(lengthDesc.configurable, true, "bound length configurable");
var nameDesc = Object.getOwnPropertyDescriptor(function target() {}.bind(null), "name");
assert.sameValue(nameDesc.writable, false, "bound name writable");
assert.sameValue(nameDesc.enumerable, false, "bound name enumerable");
assert.sameValue(nameDesc.configurable, true, "bound name configurable");

var nanLength = function () {};
Object.defineProperty(nanLength, "length", { value: NaN });
assert.sameValue(nanLength.bind().length, 0, "NaN target length");
var infiniteLength = function () {};
Object.defineProperty(infiniteLength, "length", { value: Infinity });
assert.sameValue(infiniteLength.bind(0, 0).length, Infinity, "infinite target length");
var negativeLength = function () {};
Object.defineProperty(negativeLength, "length", { value: -Infinity });
assert.sameValue(negativeLength.bind().length, 0, "negative infinite target length");
var unnamed = function () {};
Object.defineProperty(unnamed, "name", { value: 1 });
assert.sameValue(unnamed.bind(null).name, "bound ", "non-string target name");

var throwingName = function () {};
Object.defineProperty(throwingName, "name", {
    get: function () {
        throw new RangeError("name");
    },
});
assert.throws(RangeError, function () {
    throwingName.bind(null);
}, "throwing target name propagates");

assert.throws(TypeError, function () {
    bind.call({}, null);
}, "bind on a non-callable");

// Function ( ...parameterArgs, bodyArg ) (ES 20.2.1.1).
assert.sameValue(Function("a", "return a + 1")(1), 2, "Function with parameters");
assert.sameValue(Function()(), undefined, "Function without a body");
assert.sameValue(
    Function(
        {
            toString: function () {
                return "a";
            },
        },
        "return a"
    )(5),
    5,
    "Function ToStrings its arguments"
);
assert.sameValue(Function("a", undefined)(1), undefined, "undefined bodyArg is empty");
assert.throws(SyntaxError, function () {
    Function("return )");
}, "invalid body is a SyntaxError");

// Bound functions are exotic objects (ES 10.4.2).
assert.sameValue(Function.prototype.__makeBound, undefined, "no bind prelude");
var boundTarget = function target(a, b) {
    return this.base + a + b;
};
var exotic = boundTarget.bind({ base: 1 }, 2);
assert.sameValue(typeof exotic, "function", "typeof bound");
assert.sameValue(Object.prototype.toString.call(exotic), "[object Function]", "bound tag");
assert.sameValue(
    Object.getPrototypeOf(exotic),
    Object.getPrototypeOf(boundTarget),
    "bound prototype follows the target"
);

// [[Call]] does not look up `apply` (or any other property).
var applySafe = function (a) {
    return a;
}.bind(null, 7);
var savedApply = Function.prototype.apply;
Function.prototype.apply = null;
assert.sameValue(applySafe(), 7, "bound call is immune to a patched apply");
Function.prototype.apply = savedApply;

// [[Construct]] forwards to the target and patches new.target.
var NewTarget;
function BoundTarget(value) {
    NewTarget = new.target;
    this.value = value;
}
var Bound = BoundTarget.bind(null, 5);
var instance = new Bound();
assert.sameValue(instance.value, 5, "bound construct uses the bound arguments");
assert.sameValue(NewTarget, BoundTarget, "new.target is patched to the target");
assert.sameValue(instance instanceof BoundTarget, true, "bound instance is a target instance");
assert.sameValue(
    Object.getPrototypeOf(instance),
    BoundTarget.prototype,
    "bound instance prototype"
);
var mixedArgs = new (function (a, b) {
    this.sum = a + b;
}.bind(null, 1))(2);
assert.sameValue(mixedArgs.sum, 3, "bound and call arguments are combined in order");
var nested = new (BoundTarget.bind().bind())();
assert.sameValue(nested instanceof BoundTarget, true, "chained bound construct");
assert.throws(TypeError, function () {
    new (Math.max.bind(null))();
}, "a bound non-constructor is not constructable");
