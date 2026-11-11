// #76: coercing a deeply nested array must not abort the process. The
// conversion re-enters the interpreter through native calls, so the Rust
// stack runs out before the register stack does; the guard throws a
// catchable RangeError instead.

var deep = 1;
for (var i = 0; i < 20000; i++) deep = [deep];

assert.throws(
    RangeError,
    function () {
        String(deep);
    },
    "deep nesting throws RangeError"
);

// the unwind leaves the VM usable
var shallow = 1;
for (var j = 0; j < 4; j++) shallow = [shallow];
assert.sameValue(String(shallow), "1", "shallow nesting still converts");
