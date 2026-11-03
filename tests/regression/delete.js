// ES 13.5.1: the delete operator.

// A nullish base throws from ToObject before the key's ToPropertyKey runs.
var coerced = false;
var key = {
    toString: function () {
        coerced = true;
        return "x";
    },
};
assert.throws(TypeError, function () {
    delete null[key];
}, "delete on a null base throws");
assert.sameValue(coerced, false, "the key is not coerced for a null base");
assert.throws(TypeError, function () {
    var undefinedBase;
    delete undefinedBase[key];
}, "delete on an undefined base throws");
assert.sameValue(coerced, false, "the key is not coerced for an undefined base");

// A computed key on a valid base is coerced.
var object = { x: 1 };
assert.sameValue(delete object[key], true, "computed delete coerces the key");
assert.sameValue(coerced, true, "the key was coerced");

// References that are not property references.
assert.sameValue(delete void 0, true, "delete of undefined");
assert.sameValue(delete 1, true, "delete of a number");
assert.sameValue(delete {}, true, "delete of a fresh object");

// Primitive bases go through ToObject.
assert.sameValue(delete (1).x, true, "primitive base deletes an absent property");
assert.sameValue(delete "abc"[0], false, "string indices are non-configurable");
assert.sameValue(delete "abc"[9], true, "absent string index deletes");

// Strict code throws on a refused delete, sloppy code returns false.
var frozen = {};
Object.defineProperty(frozen, "keep", { value: 1, configurable: false });
assert.sameValue(delete frozen.keep, false, "sloppy refused delete returns false");
assert.sameValue(frozen.keep, 1, "sloppy refused delete keeps the property");
assert.throws(TypeError, function () {
    "use strict";
    return delete frozen.keep;
}, "strict refused delete throws");

// `delete super.x` is a ReferenceError and never coerces the key.
var superKey = {
    toString: function () {
        throw new Error("ToPropertyKey performed");
    },
};
var derived = {
    method() {
        return delete super[superKey];
    },
};
assert.throws(ReferenceError, function () {
    derived.method();
}, "delete super throws a ReferenceError");

// Dynamic names (direct eval). Environment-record slots are never deletable;
// a var/function introduced by a sloppy eval is created deletable, so deleting
// it also removes the binding (ES 19.2.1.3 CreateMutableBinding(..., true)).
function localVar() {
    var x = 1;
    return eval("delete x");
}
assert.sameValue(localVar(), false, "eval delete of a local var");

function localLet() {
    let y = 1;
    return eval("delete y");
}
assert.sameValue(localLet(), false, "eval delete of a local let");

function parameter(p) {
    return eval("delete p");
}
assert.sameValue(parameter(1), false, "eval delete of a parameter");

function evalVar() {
    eval("var z = 1");
    var deleted = eval("delete z");
    return deleted + "/" + eval("typeof z");
}
assert.sameValue(evalVar(), "true/undefined", "eval-declared var is deletable");

function evalFunction() {
    eval("function inner() { return 1; }");
    var deleted = eval("delete inner");
    return deleted + "/" + eval("typeof inner");
}
assert.sameValue(evalFunction(), "true/undefined", "eval-declared function is deletable");

function evalGlobalVar() {
    eval("var evalGlobal = 1");
    return eval("delete evalGlobal");
}
assert.sameValue(evalGlobalVar(), true, "eval-declared global var is deletable");

assert.sameValue(eval("delete neverDeclaredName"), true, "eval delete of an unknown name");

var globalVar = 1;
assert.sameValue(eval("delete globalVar"), false, "eval delete of a global var");

let globalLet = 1;
assert.sameValue(eval("delete globalLet"), false, "eval delete of a global lexical binding");

implicitGlobalForDelete = 1;
assert.sameValue(delete implicitGlobalForDelete, true, "delete of an implicit global");
assert.sameValue(typeof implicitGlobalForDelete, "undefined", "the implicit global is gone");
