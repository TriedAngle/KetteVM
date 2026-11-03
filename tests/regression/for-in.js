// ES 14.7.5 for-in enumeration order and own-property checks.

function collect(obj) {
    var out = [];
    for (var key in obj) {
        out.push(key);
    }
    return out.join(",");
}

// Ascending index keys, then string keys in insertion order.
assert.sameValue(collect({ 0: "a", b: 1 }), "0,b", "index key then string key");
assert.sameValue(collect({ b: 1, 0: "a" }), "0,b", "indices sort before strings");
assert.sameValue(collect({ 2: "c", 0: "a", 1: "b" }), "0,1,2", "indices ascend");

// Non-enumerable own keys are skipped.
var hidden = { 0: "a" };
Object.defineProperty(hidden, 1, { value: "b", enumerable: false });
assert.sameValue(collect(hidden), "0", "non-enumerable element is skipped");

// Inherited enumerable keys are visited, an own non-enumerable key shadows
// the inherited enumerable one of the same name.
var proto = { inherited: 1, shadowed: 2 };
var child = Object.create(proto);
Object.defineProperty(child, "shadowed", { value: 3, enumerable: false });
assert.sameValue(collect(child), "inherited", "shadowed inherited key is skipped");

// Arrays and strings enumerate their indices.
assert.sameValue(collect([10, 20]), "0,1", "array indices");
assert.sameValue(collect("ab"), "0,1", "string code units");

// A key deleted after the snapshot is skipped (lazy [[GetOwnProperty]]).
var mutating = { a: 1, b: 2 };
var seen = [];
for (var key in mutating) {
    seen.push(key);
    delete mutating.b;
}
assert.sameValue(seen.join(","), "a", "deleted key is not yielded");

// A key added after the snapshot is not yielded.
var growing = { a: 1 };
var grown = [];
for (var key in growing) {
    grown.push(key);
    growing.b = 2;
}
assert.sameValue(grown.join(","), "a", "added key is not yielded");
