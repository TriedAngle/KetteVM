// Defining an element must update every attribute, not just the value
// (ES 10.1.6.3: a compatible descriptor replaces the details). The dense
// store fast path only applies while both sides are plain `w,e,c` data.

function descriptor(o, key) {
    return Object.getOwnPropertyDescriptor(o, key);
}

function plain(writable, enumerable, configurable, value) {
    return {
        value: value,
        writable: writable,
        enumerable: enumerable,
        configurable: configurable,
    };
}

// writable but non-enumerable -> all-true
var a = [];
Object.defineProperty(a, "0", plain(true, false, true, 1));
Object.defineProperty(a, "0", plain(true, true, true, 2));
assert.sameValue(a[0], 2, "value updated");
assert.sameValue(descriptor(a, "0").enumerable, true, "enumerable updated");
assert.sameValue(descriptor(a, "0").configurable, true, "configurable kept");

// read-only -> writable
var b = [];
Object.defineProperty(b, "0", plain(false, true, true, 1));
Object.defineProperty(b, "0", plain(true, true, true, 2));
assert.sameValue(b[0], 2, "read-only value updated");
assert.sameValue(descriptor(b, "0").writable, true, "writable updated");

// accessor -> data
var c = [];
Object.defineProperty(c, "0", {
    get: function () {
        return 7;
    },
    enumerable: false,
    configurable: true,
});
Object.defineProperty(c, "0", plain(true, true, true, 3));
assert.sameValue(c[0], 3, "accessor replaced by data");
assert.sameValue(descriptor(c, "0").get, undefined, "getter removed");
assert.sameValue(descriptor(c, "0").enumerable, true, "accessor enumerable updated");

// partial descriptor keeps the untouched attributes
var d = [];
Object.defineProperty(d, "0", plain(true, false, true, 1));
Object.defineProperty(d, "0", { value: 4 });
assert.sameValue(d[0], 4, "partial value updated");
assert.sameValue(descriptor(d, "0").enumerable, false, "untouched enumerable kept");

// matching non-default attributes only rewrite the value
var g = [];
Object.defineProperty(g, "0", plain(true, false, false, 1));
Object.defineProperty(g, "0", { value: 5 });
assert.sameValue(g[0], 5, "non-default value updated");
assert.sameValue(descriptor(g, "0").enumerable, false, "non-default enumerable kept");
assert.sameValue(descriptor(g, "0").configurable, false, "non-default configurable kept");

// a non-writable element accepts the same value, rejects a new one
var h = [];
Object.defineProperty(h, "0", plain(false, true, false, 7));
Object.defineProperty(h, "0", { value: 7 });
assert.sameValue(h[0], 7, "same value accepted");
assert.throws(TypeError, function () {
    Object.defineProperty(h, "0", { value: 8 });
});

// incompatible redefinitions still throw
var e = [];
Object.defineProperty(e, "0", plain(true, true, false, 1));
assert.throws(TypeError, function () {
    Object.defineProperty(e, "0", plain(true, false, false, 2));
});

// arrays keep growing correctly through the dictionary path
var f = [];
Object.defineProperty(f, "2", plain(false, false, true, 9));
assert.sameValue(f.length, 3, "length tracks dictionary elements");
assert.sameValue(f[2], 9, "dictionary element readable");
assert.sameValue(descriptor(f, "2").writable, false, "writable false stored");
