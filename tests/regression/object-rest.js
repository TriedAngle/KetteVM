// ES 7.3.25 CopyDataProperties: object rest (`{ ...rest }`).

function keys(obj) {
    return Object.getOwnPropertyNames(obj).join(",");
}

// [[GetOwnProperty]] is re-read for every key, after the earlier Gets.
var deleted = {
    get a() {
        delete deleted.b;
        return 1;
    },
    b: 2,
};
var { ...afterDelete } = deleted;
assert.sameValue(afterDelete.a, 1, "earlier getter value is copied");
assert.sameValue(keys(afterDelete), "a", "a key deleted mid-copy is skipped");

var hidden = {
    get a() {
        Object.defineProperty(hidden, "b", { enumerable: false });
        return 1;
    },
    b: 2,
};
var { ...afterHide } = hidden;
assert.sameValue(keys(afterHide), "a", "a key hidden mid-copy is skipped");

// Element keys are ordinary own enumerable properties.
var { ...fromArray } = [10, 20];
assert.sameValue(keys(fromArray), "0,1", "array indices are copied");
assert.sameValue(fromArray[0], 10, "first element");
assert.sameValue(fromArray[1], 20, "second element");
assert.sameValue("5" in fromArray, false, "trailing capacity is not an own index");
assert.sameValue(fromArray[5], undefined, "trailing capacity reads undefined");

var { 0: first, ...restAfterFirst } = [1, 2];
assert.sameValue(first, 1, "numeric pattern target");
assert.sameValue(keys(restAfterFirst), "1", "numeric key is excluded");

var { ...fromString } = "ab";
assert.sameValue(keys(fromString), "0,1", "string code units are own indices");
assert.sameValue(fromString[1], "b", "string code unit value");

var { ...fromNumber } = 5;
assert.sameValue(keys(fromNumber), "", "number primitives have no own properties");

var symbol = Symbol("key");
var { ...fromSymbols } = { [symbol]: 1, named: 2 };
assert.sameValue(fromSymbols[symbol], 1, "symbol keys are copied");
assert.sameValue(fromSymbols.named, 2, "string keys are copied next to symbols");

var { ...inherited } = Object.create({ inherited: 1 });
assert.sameValue(keys(inherited), "", "inherited properties are not copied");

var nonEnumerable = {};
Object.defineProperty(nonEnumerable, 0, { value: "v", enumerable: false });
Object.defineProperty(nonEnumerable, "hidden", { value: "v", enumerable: false });
var { ...fromNonEnumerable } = nonEnumerable;
assert.sameValue(keys(fromNonEnumerable), "", "non-enumerable own keys are skipped");

// The excluded names keep the pattern's evaluation order.
var { a = 1, ...restDefaults } = { a: undefined, b: 2 };
assert.sameValue(restDefaults.a, undefined, "defaulted target still excludes its name");
assert.sameValue(keys(restDefaults), "b", "only unexcluded names are copied");
