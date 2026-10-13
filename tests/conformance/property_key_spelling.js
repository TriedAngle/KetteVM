// ES 6.1.7: -1 and "-1" denote the same property key; a store through
// one spelling must be visible through the other.
var o = {};
o[-1] = 1;
if (o["-1"] !== 1) {
    throw new TypeError('o[-1] = 1; o["-1"]: expected 1, got ' + o["-1"]);
}
