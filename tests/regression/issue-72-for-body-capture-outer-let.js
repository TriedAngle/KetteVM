// #72: a function whose `for (let ...)` body declares a captured `let` pushes
// a function context; reads of an outer `let` from inside that function must
// still walk the right number of contexts.

let c = 5;

function f() {
    assert.sameValue(typeof c, "number", "outer let before the loop");
    assert.sameValue(c, 5, "outer let value before the loop");
    for (let i = 0; i < 1; i++) {
        let w = 0;
        (() => w);
        assert.sameValue(c, 5, "outer let inside the loop body");
    }
    return c;
}
assert.sameValue(f(), 5, "outer let after a for-body capture");

// a captured block binding inside the loop body works the same way
function g() {
    let outer = 7;
    let seen = "";
    for (let i = 0; i < 2; i++) {
        {
            let w = i;
            (() => w);
            seen += outer + ":" + w + ",";
        }
    }
    return seen;
}
assert.sameValue(g(), "7:0,7:1,", "outer let with a captured nested block");

// the captured bindings themselves stay per-iteration
function h() {
    let out = [];
    for (let i = 0; i < 3; i++) {
        let w = i;
        out.push((() => w)());
    }
    return out.join();
}
assert.sameValue(h(), "0,1,2", "per-iteration capture");
