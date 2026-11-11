// #71: a `switch` whose case declares a captured `let` owns a block context,
// and the "no case matched" exit must pop it. Otherwise later reads of outer
// lexicals land in the case-block context (wrong values or a TDZ error).
//
// The discriminant case (references there see the environment before the
// case block) is covered by test262's
// `language/statements/switch/scope-lex-open-*.js`; the exit path is not.

let v = 1, u = 5;
switch (v) {
    case 2:
        let w = 0;
        (() => w);
}
assert.sameValue(u, 5, "outer let after a switch with no matching case");
assert.sameValue(v, 1, "discriminant value after the switch");

// inside a function, with the switch nested in a captured for-body context
function nested() {
    let outer = 7;
    let seen = "";
    for (let i = 0; i < 2; i++) {
        switch (i) {
            case 5:
                let w = i;
                (() => w);
        }
        seen += outer + ":" + i + ",";
    }
    return seen;
}
assert.sameValue(nested(), "7:0,7:1,", "outer let after a skipped case in a loop");

// nested switches that both take the no-match exit
let a = 1, b = 2;
switch (a) {
    case 9:
        switch (b) {
            case 9:
                let deep = 0;
                (() => deep);
        }
        let outer = 0;
        (() => outer);
}
assert.sameValue(a, 1, "outer discriminant after nested skipped switches");
assert.sameValue(b, 2, "inner discriminant after nested skipped switches");
