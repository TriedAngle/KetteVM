// `arguments` objects (ES 10.4.4): unmapped for strict / non-simple
// parameter lists, mapped for non-strict simple lists.

function mapped(a, b) {
    assert.sameValue(arguments.length, 2, "mapped length");
    assert.sameValue(arguments[0], 1, "mapped arg0");
    a = 7;
    assert.sameValue(arguments[0], 7, "parameter write aliases");
    arguments[1] = 9;
    assert.sameValue(b, 9, "arguments write aliases");
    assert.sameValue(arguments.callee, mapped, "mapped callee");
    assert.sameValue(Object.prototype.toString.call(arguments), "[object Arguments]");
}
mapped(1, 2);

function unmapped(a = 0) {
    var args = arguments;
    a = 7;
    assert.sameValue(args[0], 3, "default parameter is unmapped");
    assert.throws(TypeError, function () {
        return args.callee;
    });
}
unmapped(3);

function strict() {
    "use strict";
    var args = arguments;
    assert.sameValue(args.length, 1, "strict length");
    assert.throws(TypeError, function () {
        return args.callee;
    });
}
strict(1);

function outer() {
    var arguments = 1;
    return function () {
        return arguments.length;
    };
}
assert.sameValue(outer()(), 0, "inner function has its own arguments");

function arrow() {
    return () => arguments[0];
}
assert.sameValue(arrow(5)(), 5, "arrow captures arguments");

function shadow(arguments) {
    return arguments;
}
assert.sameValue(shadow(4), 4, "parameter named arguments shadows");
