// #68: with a direct `eval` in scope, a caught exception used to be stored
// into the `for (let ...)` loop variable (the catch parameter's declaration
// store went to the enclosing loop-head context).

eval("");
var seen = "";
for (let i = 0; i < 3; i++) {
    try {
        throw "x";
    } catch (e) {
        assert.sameValue(e, "x", "catch parameter receives the exception");
    }
    seen += i;
}
assert.sameValue(seen, "012", "loop variable survives the catch");

// the same without eval
var plain = "";
for (let i = 0; i < 3; i++) {
    try {
        throw i;
    } catch (e) {
        plain += e;
    }
}
assert.sameValue(plain, "012", "loop variable survives without eval");

// a captured catch parameter still sees its own binding
eval("");
var fromClosure;
try {
    throw 42;
} catch (e) {
    fromClosure = (() => e)();
}
assert.sameValue(fromClosure, 42, "captured catch parameter");

// a captured catch parameter inside a nested block, with eval in scope
function nested() {
    eval("");
    let acc = "";
    try {
        throw 1;
    } catch (e) {
        {
            let inner = () => e;
            acc += inner();
        }
    }
    return acc;
}
assert.sameValue(nested(), "1", "captured catch parameter in a nested block");
