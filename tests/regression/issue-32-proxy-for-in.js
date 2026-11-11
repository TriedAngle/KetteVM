// for-in goes through the internal methods (ES 14.7.5.9
// EnumerateObjectProperties), so proxies run their ownKeys,
// getOwnPropertyDescriptor and getPrototypeOf traps.

function keys(o) {
    var result = [];
    for (var k in o) result.push(k);
    return result.join(",");
}

assert.sameValue(keys(new Proxy({a: 1, b: 2}, {})), "a,b", "forwarding proxy");

var trapped = new Proxy({}, {
    ownKeys: function () {
        return ["z"];
    },
    getOwnPropertyDescriptor: function () {
        return {value: 1, enumerable: true, configurable: true};
    },
});
assert.sameValue(keys(trapped), "z", "ownKeys trap");
assert.sameValue(
    Object.getOwnPropertyNames(trapped).join(","),
    "z",
    "getOwnPropertyNames uses the same trap"
);

var inherited = new Proxy({own: 1}, {
    getPrototypeOf: function () {
        return {inherited: 1};
    },
});
assert.sameValue(keys(inherited), "own,inherited", "getPrototypeOf trap");

var shadowed = new Proxy({}, {
    ownKeys: function () {
        return ["x"];
    },
    getOwnPropertyDescriptor: function () {
        return {value: 1, enumerable: false, configurable: true};
    },
    getPrototypeOf: function () {
        return {x: 1, y: 2};
    },
});
assert.sameValue(keys(shadowed), "y", "non-enumerable own key shadows the prototype");

var ordered = new Proxy({}, {
    ownKeys: function () {
        return ["b", Symbol("s"), "a"];
    },
    getOwnPropertyDescriptor: function () {
        return {value: 1, enumerable: true, configurable: true};
    },
});
assert.sameValue(keys(ordered), "b,a", "trap order kept, symbols skipped");

var indexed = new Proxy({10: 1, 2: 2, a: 3}, {});
assert.sameValue(keys(indexed), "2,10,a", "integer keys bind as strings, ascending");

var deleted = new Proxy({}, {
    ownKeys: function () {
        return ["gone", "kept"];
    },
    getOwnPropertyDescriptor: function (target, key) {
        if (key === "gone") return undefined;
        return {value: 1, enumerable: true, configurable: true};
    },
});
assert.sameValue(keys(deleted), "kept", "key without a descriptor is skipped");

var revoked = Proxy.revocable({a: 1}, {});
revoked.revoke();
assert.throws(TypeError, function () {
    for (var k in revoked.proxy);
});
