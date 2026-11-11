// #63: a `let` declared directly in a `try` block and captured by a closure
// needs the try block's own context (the compiler used to panic with
// "declaration host Block(...) is not on the active context chain").

var seen = 0;
try {
    let x = 1;
    seen = (() => x)();
} catch (e) {}
assert.sameValue(seen, 1, "captured try-block let");

// the context is fresh on every execution of the try block
var values = [];
for (var i = 0; i < 3; i++) {
    try {
        let y = i;
        values.push((() => y)());
    } catch (e) {}
}
assert.sameValue(values.join(), "0,1,2", "fresh try-block context per run");

// an exception unwinds the try context; the handler still sees its enclosing
// bindings
var caught;
try {
    let z = 1;
    throw (() => z)();
} catch (e) {
    caught = e;
}
assert.sameValue(caught, 1, "try context unwinds into the handler");
