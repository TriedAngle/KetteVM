// ES 20.5.1.1 Error(message) / 20.5.3.4 Error.prototype.toString.

// Step 2: OrdinaryCreateFromConstructor honours new.target.
class MyError extends Error {}
var e = new MyError("boom");
assert.sameValue(Object.getPrototypeOf(e), MyError.prototype, "subclass prototype");
assert.sameValue(e instanceof MyError, true, "subclass instanceof");
assert.sameValue(e instanceof Error, true, "subclass is an Error");
assert.sameValue(e.message, "boom", "subclass message");

// Step 3: only `message`, only when the argument is not undefined,
// non-enumerable, and ToString'd through ToPrimitive.
assert.sameValue(
    Object.getOwnPropertyDescriptor(new Error(), "message"),
    undefined,
    "no message for a missing argument"
);
assert.sameValue(
    Object.getOwnPropertyDescriptor(new Error(undefined), "message"),
    undefined,
    "no message for undefined"
);
assert.sameValue(
    Object.getOwnPropertyDescriptor(new Error("x"), "name"),
    undefined,
    "no own name"
);
var desc = Object.getOwnPropertyDescriptor(new Error("x"), "message");
assert.sameValue(desc.writable, true, "message writable");
assert.sameValue(desc.enumerable, false, "message enumerable");
assert.sameValue(desc.configurable, true, "message configurable");
assert.sameValue(
    new Error({
        toString: function () {
            return "obj";
        },
    }).message,
    "obj",
    "message ToString"
);
assert.throws(TypeError, function () {
    new Error(Symbol("x"));
}, "message ToString of a Symbol");

// Error.prototype.toString.
var ts = Error.prototype.toString;
assert.sameValue(ts.call({}), "Error", "defaults");
assert.sameValue(ts.call({ name: undefined, message: undefined }), "Error", "explicit undefined");
assert.sameValue(ts.call({ name: "", message: "m" }), "m", "empty name");
assert.sameValue(ts.call({ name: "E", message: "" }), "E", "empty message");
assert.sameValue(ts.call({ name: 1, message: true }), "1: true", "ToString of both parts");
assert.sameValue(
    ts.call({
        name: {
            toString: function () {
                return "N";
            },
        },
        message: "m",
    }),
    "N: m",
    "name ToString through ToPrimitive"
);
assert.sameValue(new Error("x").toString(), "Error: x", "instance toString");
assert.sameValue(new Error("").toString(), "Error", "empty message instance");
assert.sameValue(new Error().toString(), "Error", "missing message instance");
assert.sameValue(TypeError("x").toString(), "TypeError: x", "native error toString");
assert.sameValue(MyError.prototype.name, "Error", "subclass prototype inherits name");

// Step 2: a non-Object `this` is a TypeError.
assert.throws(TypeError, function () {
    ts.call("x");
}, "string receiver");
assert.throws(TypeError, function () {
    ts.call(1);
}, "number receiver");
assert.throws(TypeError, function () {
    ts.call(null);
}, "null receiver");
assert.throws(TypeError, function () {
    ts.call(undefined);
}, "undefined receiver");

assert.sameValue(Object.prototype.toString.call(new Error("x")), "[object Error]", "tag");

// InstallErrorCause (ES 20.5.9.1).
var withCause = new Error("x", { cause: 42 });
assert.sameValue(withCause.cause, 42, "cause value");
var causeDesc = Object.getOwnPropertyDescriptor(withCause, "cause");
assert.sameValue(causeDesc.writable, true, "cause writable");
assert.sameValue(causeDesc.enumerable, false, "cause enumerable");
assert.sameValue(causeDesc.configurable, true, "cause configurable");
assert.sameValue(
    new Error("x", Object.create({ cause: "inherited" })).cause,
    "inherited",
    "HasProperty walks the prototype chain"
);
assert.sameValue(
    Object.getOwnPropertyDescriptor(new Error("x", 1), "cause"),
    undefined,
    "a primitive options argument installs nothing"
);
assert.sameValue(
    Object.getOwnPropertyDescriptor(new Error("x", null), "cause"),
    undefined,
    "null options installs nothing"
);
assert.sameValue(new Error("x", { cause: undefined }).cause, undefined, "undefined cause stays defined");
assert.throws(RangeError, function () {
    new Error("x", {
        get cause() {
            throw new RangeError("nope");
        },
    });
}, "throwing cause getter propagates");
assert.throws(TypeError, function () {
    new Error("x", new Proxy({}, { has: function () { throw new TypeError("trap"); } }));
}, "throwing HasProperty trap propagates");

// Error.isError (ES 20.5.2.1).
assert.sameValue(Error.isError(new Error()), true, "Error.isError(Error)");
assert.sameValue(Error.isError(new TypeError()), true, "Error.isError(TypeError)");
assert.sameValue(Error.isError({}), false, "Error.isError(object)");
assert.sameValue(Error.isError([]), false, "Error.isError(array)");
assert.sameValue(Error.isError(Object.create(Error.prototype)), false, "Error.isError(no slot)");
assert.sameValue(Error.isError(1), false, "Error.isError(primitive)");
assert.sameValue(Error.isError(new Proxy(new Error(), {})), false, "Error.isError(proxy)");
assert.sameValue(
    Error.isError(new (class extends Error {})()),
    true,
    "Error.isError(subclass instance)"
);

// The two remaining NativeError classes.
var evalError = new EvalError("e");
assert.sameValue(evalError instanceof EvalError, true, "EvalError instanceof");
assert.sameValue(evalError instanceof Error, true, "EvalError is an Error");
assert.sameValue(evalError.name, "EvalError", "EvalError name");
assert.sameValue(String(evalError), "EvalError: e", "EvalError toString");
assert.sameValue(
    Object.getPrototypeOf(EvalError.prototype),
    Error.prototype,
    "EvalError.prototype chain"
);
var uriError = new URIError("u");
assert.sameValue(uriError instanceof URIError, true, "URIError instanceof");
assert.sameValue(uriError.name, "URIError", "URIError name");
assert.sameValue(String(uriError), "URIError: u", "URIError toString");
class MyUriError extends URIError {}
assert.sameValue(
    Object.getPrototypeOf(new MyUriError("x")),
    MyUriError.prototype,
    "URIError subclass"
);
