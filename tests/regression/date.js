// ES 21.4.2.1 Date ( ...values ).

// Call form: ToDateString(now), every argument ignored.
assert.sameValue(typeof Date(), "string", "call form returns a string");
assert.sameValue(
    isNaN(new Date(Date()).getTime()),
    false,
    "call form produces a parseable date string"
);
assert.sameValue(Date(1, 2, 3), Date(1, 2, 3), "arguments do not change a call");

// No arguments: the current time.
var before = Date.now();
var now = new Date().getTime();
assert.sameValue(now >= before, true, "no-argument constructor is not in the past");
assert.sameValue(now <= Date.now(), true, "no-argument constructor is not in the future");

// One value: [[DateValue]], String parse, or ToNumber(ToPrimitive(value)).
assert.sameValue(new Date(1000).getTime(), 1000, "number value");
assert.sameValue(new Date(new Date(5000)).getTime(), 5000, "Date value");
assert.sameValue(
    new Date("1970-01-01T00:00:01Z").getTime(),
    1000,
    "string value is parsed like Date.parse"
);
assert.sameValue(
    new Date({
        valueOf: function () {
            return 5;
        },
    }).getTime(),
    5,
    "object value goes through ToPrimitive"
);
assert.sameValue(
    new Date({
        toString: function () {
            return "1970-01-01T00:00:01Z";
        },
    }).getTime(),
    1000,
    "object value ToPrimitive may yield a string"
);
assert.sameValue(isNaN(new Date(undefined).getTime()), true, "undefined is NaN");
assert.sameValue(isNaN(new Date(null).getTime()), false, "null is +0");
assert.sameValue(new Date(null).getTime(), 0, "null time value");
assert.throws(TypeError, function () {
    new Date(Symbol("x"));
}, "a Symbol value throws");

// Component form: year, month, date=1, hours=minutes=seconds=ms=0.
assert.sameValue(new Date(2020, 0).getTime(), 1577836800000, "component defaults");
assert.sameValue(new Date(2020, 0).getTime(), new Date(2020, 0, 1).getTime(), "day defaults to 1");
assert.sameValue(new Date(2020, 0, 1, 0, 0, 0, 0).getTime(), 1577836800000, "explicit zeros");
assert.sameValue(new Date(99, 0).getTime(), new Date(1999, 0).getTime(), "two-digit year");
assert.sameValue(
    new Date(2020, 12, 1).getTime(),
    new Date(2021, 0, 1).getTime(),
    "month overflow normalises"
);
assert.sameValue(
    new Date(2020, 0, 0).getTime(),
    new Date(2019, 11, 31).getTime(),
    "day 0 normalises to the previous month"
);
assert.sameValue(
    new Date(2020, 0, 1, 0, 0, 0, 0, 999).getTime(),
    1577836800000,
    "arguments past the seventh are ignored"
);
// ToNumber runs left to right over the supplied components.
var order = [];
function number(value) {
    return {
        valueOf: function () {
            order[order.length] = value;
            return value;
        },
    };
}
new Date(number(1), number(2), number(3), number(4));
assert.sameValue(order.length, 4, "ToNumber count");
assert.sameValue(order[0], 1, "year first");
assert.sameValue(order[1], 2, "month second");
assert.sameValue(order[2], 3, "day third");
assert.sameValue(order[3], 4, "hour fourth");

// A millisecond component may reach the maximum time value.
assert.sameValue(
    new Date(1970, 0, 1, 0, 0, 0, 2e15).getTime(),
    2e15,
    "large ms component"
);
assert.sameValue(isNaN(new Date(1970, 0, 1, 0, 0, 0, 1e16).getTime()), true, "ms past the range");
assert.sameValue(isNaN(new Date(NaN).getTime()), true, "NaN value");
assert.sameValue(isNaN(new Date(Infinity).getTime()), true, "infinite value");

// OrdinaryCreateFromConstructor: subclasses keep their prototype.
class MyDate extends Date {}
var subclass = new MyDate(1234);
assert.sameValue(Object.getPrototypeOf(subclass), MyDate.prototype, "subclass prototype");
assert.sameValue(subclass instanceof MyDate, true, "subclass instanceof");
assert.sameValue(subclass instanceof Date, true, "subclass is a Date");
assert.sameValue(subclass.getTime(), 1234, "subclass [[DateValue]]");

// Component getters (ES 21.4.4.2-21.4.4.19): local and UTC coincide here.
var parts = new Date(2020, 0, 2, 3, 4, 5, 6); // 2020-01-02T03:04:05.006Z
assert.sameValue(parts.getFullYear(), 2020, "getFullYear");
assert.sameValue(parts.getMonth(), 0, "getMonth");
assert.sameValue(parts.getDate(), 2, "getDate");
assert.sameValue(parts.getDay(), 4, "getDay (Thursday)");
assert.sameValue(parts.getHours(), 3, "getHours");
assert.sameValue(parts.getMinutes(), 4, "getMinutes");
assert.sameValue(parts.getSeconds(), 5, "getSeconds");
assert.sameValue(parts.getMilliseconds(), 6, "getMilliseconds");
assert.sameValue(parts.getUTCFullYear(), 2020, "getUTCFullYear");
assert.sameValue(parts.getUTCMonth(), 0, "getUTCMonth");
assert.sameValue(parts.getUTCDate(), 2, "getUTCDate");
assert.sameValue(parts.getUTCDay(), 4, "getUTCDay");
assert.sameValue(parts.getUTCHours(), 3, "getUTCHours");
assert.sameValue(parts.getUTCMinutes(), 4, "getUTCMinutes");
assert.sameValue(parts.getUTCSeconds(), 5, "getUTCSeconds");
assert.sameValue(parts.getUTCMilliseconds(), 6, "getUTCMilliseconds");
assert.sameValue(parts.getTimezoneOffset(), 0, "getTimezoneOffset on a UTC host");
assert.sameValue(parts.getYear(), 120, "getYear (Annex B)");
assert.sameValue(new Date(NaN).getFullYear() !== new Date(NaN).getFullYear(), true, "invalid getter is NaN");
assert.sameValue(new Date(NaN).getTimezoneOffset() !== new Date(NaN).getTimezoneOffset(), true, "invalid offset is NaN");
assert.throws(TypeError, function () {
    Date.prototype.getFullYear.call({});
}, "getter needs a Date receiver");

// Component setters (ES 21.4.4.20-21.4.4.34): unchanged fields keep their
// current value and the new time value is returned.
var setter = new Date(2020, 0, 2, 3, 4, 5, 6);
assert.sameValue(setter.setHours(10), setter.getTime(), "setter returns the time value");
assert.sameValue(setter.getHours(), 10, "setHours");
assert.sameValue(setter.getMinutes(), 4, "setHours keeps the minute");
assert.sameValue(setter.getDate(), 2, "setHours keeps the day");
setter.setMinutes(30, 45);
assert.sameValue(setter.getMinutes(), 30, "setMinutes");
assert.sameValue(setter.getSeconds(), 45, "setMinutes sets the second");
assert.sameValue(setter.getMilliseconds(), 6, "setMinutes keeps the millisecond");
setter.setDate(15);
assert.sameValue(setter.getDate(), 15, "setDate");
assert.sameValue(setter.getMonth(), 0, "setDate keeps the month");
setter.setMonth(5);
assert.sameValue(setter.getMonth(), 5, "setMonth");
assert.sameValue(setter.getDate(), 15, "setMonth keeps the day");
setter.setFullYear(1999);
assert.sameValue(setter.getFullYear(), 1999, "setFullYear");
assert.sameValue(setter.getMonth(), 5, "setFullYear keeps the month");
assert.sameValue(
    setter.setUTCFullYear(2020, 0, 1),
    new Date(2020, 0, 1, 10, 30, 45, 6).getTime(),
    "setUTCFullYear keeps the time of day"
);
assert.sameValue(new Date(0).setTime(1234), 1234, "setTime");
assert.sameValue(new Date(1000).setMilliseconds(1500), 2500, "setMilliseconds");
assert.sameValue(new Date(1000).setSeconds(61), 61000, "setSeconds normalises");
assert.sameValue(new Date(86400000).setDate(2, 5, 6), 86400000, "extra setter arguments are ignored");
assert.sameValue(new Date(NaN).setFullYear(2000), new Date(2000, 0, 1).getTime(), "setFullYear recovers an invalid Date");
var invalidSetter = new Date(NaN);
assert.sameValue(invalidSetter.setDate(1) !== invalidSetter.setDate(1), true, "other setters leave an invalid Date NaN");
assert.sameValue(new Date(0).setHours(undefined) !== new Date(0).setHours(undefined), true, "a NaN argument gives NaN");
assert.throws(TypeError, function () {
    Date.prototype.setTime.call({}, 0);
}, "setter needs a Date receiver");
// The first component of a setter is mandatory in the signature: calling
// without it passes `undefined`, which ToNumber turns into NaN.
var noArgs = [
    "setDate", "setFullYear", "setHours", "setMilliseconds", "setMinutes",
    "setMonth", "setSeconds", "setUTCDate", "setUTCFullYear", "setUTCHours",
    "setUTCMilliseconds", "setUTCMinutes", "setUTCMonth", "setUTCSeconds",
    "setTime", "setYear",
];
for (var n = 0; n < noArgs.length; n++) {
    var method = Date.prototype[noArgs[n]];
    var target = new Date(0);
    var result = method.call(target);
    assert.sameValue(result !== result, true, noArgs[n] + "() without arguments is NaN");
    assert.sameValue(target.getTime() !== target.getTime(), true, noArgs[n] + "() writes NaN");
}

// Annex B year accessors.
var legacy = new Date(2000, 5, 15);
legacy.setYear(99);
assert.sameValue(legacy.getFullYear(), 1999, "setYear maps 0..99 to 19xx");
assert.sameValue(legacy.getMonth(), 5, "setYear keeps the month");
assert.sameValue(legacy.getDate(), 15, "setYear keeps the day");
