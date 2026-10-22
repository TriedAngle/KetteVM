use interpreter_become::BecomeInterpreter;
use interpreter_match_loop::MatchLoopInterpreter;
use js_compiler::JavascriptCompiler;
use js_runtime::JSRuntime;
use mark_sweep::{MarkSweep, MarkSweepConfig};
use vm_core::{Interpreter, VM};

fn run<I: Interpreter>(label: &str, src: &str) {
    let vm = VM::new::<MarkSweep, I>(MarkSweepConfig::default())
        .unwrap()
        .add::<JSRuntime>()
        .unwrap();
    vm.arm_gc_stress();
    let mut thread = vm.attach();
    thread.eval::<JavascriptCompiler>(src).expect("runs");
    if let Some(ex) = thread.take_pending_exception() {
        panic!("{} {label}: pending exception: {ex:?}", std::any::type_name::<I>());
    }
}

/// Both interpreters must agree: dispatch uses one handler table, and
/// mode-sensitive handlers branch on the frame's callable-info mode in
/// the shared cold paths. Same scripts, same expectations.
fn run_both(label: &str, src: &str) {
    run::<BecomeInterpreter>(label, src);
    run::<MatchLoopInterpreter>(label, src);
}

const CHECK: &str = "function check(c, m) { if (!c) { throw new Error(m); } }\nvar declared = 0;\n";
const HELPERS: &str = r#"
function sloppySet(o, v) { o.x = v; return o.x; }
function strictSet(o, v) { "use strict"; o.x = v; return o.x; }
function sloppyNewProp(o, v) { o.y = v; return o.y; }
function strictNewProp(o, v) { "use strict"; o.y = v; return o.y; }
function sloppyDelete(o) { return delete o.x; }
function strictDelete(o) { "use strict"; return delete o.x; }
function sloppyUndeclared() { sloppy_leak = 1; return sloppy_leak; }
function strictUndeclared() { "use strict"; strict_leak = 1; }
function sloppyInner(o, v) { o.x = v; }
function strictInner(o, v) { "use strict"; o.x = v; }
function sloppyOuter(o, v) { strictInner(o, v); }
function strictOuterAfterSloppy(o, d) {
    "use strict";
    sloppyInner(o, 2);   // sloppy callee: ignores the read-only store
    return delete d.x;   // back in this strict frame: must throw
}
"#;

/// `delete obj.x` on a non-configurable property: false in sloppy code,
/// TypeError in strict code. One opcode, one handler reading frame mode.
#[test]
fn delete_refusal_follows_the_frame_mode() {
    run_both(
        "delete",
        &format!(
            "{CHECK}{HELPERS}
            var o = {{}};
            Object.defineProperty(o, 'x', {{ value: 1, configurable: false }});
            check(sloppyDelete(o) === false, 'sloppy delete returns false');
            var threw = false;
            try {{ strictDelete(o); }} catch (e) {{ threw = e instanceof TypeError; }}
            check(threw, 'strict delete throws TypeError');
            check('x' in o, 'property survives both');"
        ),
    );
}

/// Failed [[Set]]: silent in sloppy code, TypeError in strict.
#[test]
fn store_refusal_follows_the_frame_mode() {
    run_both(
        "read-only",
        &format!(
            "{CHECK}{HELPERS}
            var o = {{}};
            Object.defineProperty(o, 'x', {{ value: 1, writable: false }});
            check(sloppySet(o, 2) === 1, 'sloppy read-only ignored');
            var threw = false;
            try {{ strictSet(o, 2); }} catch (e) {{ threw = e instanceof TypeError; }}
            check(threw, 'strict read-only throws');
            check(o.x === 1, 'read-only value unchanged');"
        ),
    );
    run_both(
        "no-setter",
        &format!(
            "{CHECK}{HELPERS}
            var o = {{}};
            Object.defineProperty(o, 'x', {{ get: function() {{ return 5; }}, configurable: true }});
            check(sloppySet(o, 2) === 5, 'sloppy setter-less accessor ignored');
            var threw = false;
            try {{ strictSet(o, 2); }} catch (e) {{ threw = e instanceof TypeError; }}
            check(threw, 'strict setter-less accessor throws');"
        ),
    );
    run_both(
        "non-extensible",
        &format!(
            "{CHECK}{HELPERS}
            var o = {{}};
            Object.preventExtensions(o);
            check(sloppyNewProp(o, 1) === undefined, 'sloppy new prop ignored');
            var threw = false;
            try {{ strictNewProp(o, 1); }} catch (e) {{ threw = e instanceof TypeError; }}
            check(threw, 'strict new prop throws');"
        ),
    );
    run_both(
        "primitive",
        &format!(
            "{CHECK}{HELPERS}
            check(sloppySet('abc', 1) === undefined, 'sloppy primitive store ignored');
            var threw = false;
            try {{ strictSet('abc', 1); }} catch (e) {{ threw = e instanceof TypeError; }}
            check(threw, 'strict primitive store throws');"
        ),
    );
}

/// Assignment to an unresolvable reference: sloppy creates a global
/// property, strict throws ReferenceError (not TypeError).
#[test]
fn undeclared_global_follows_the_frame_mode() {
    run_both(
        "undeclared",
        &format!(
            "{CHECK}{HELPERS}
            check(sloppyUndeclared() === 1, 'sloppy creates the global');
            check(sloppy_leak === 1, 'sloppy global visible');
            var threw = false;
            try {{ strictUndeclared(); }}
            catch (e) {{ threw = e instanceof ReferenceError; }}
            check(threw, 'strict undeclared throws ReferenceError');
            // a declared global binding is not an unresolvable reference
            declared = 3;
            check(declared === 3, 'strict store to a declared global');"
        ),
    );
}

/// The mode is per frame, not per execution: a strict callee inside a
/// sloppy caller still throws, and a sloppy callee inside a strict caller
/// still ignores. The caller's table must be restored on return.
#[test]
fn mode_travels_with_the_frame() {
    run_both(
        "strict-callee",
        &format!(
            "{CHECK}{HELPERS}
            var o = {{}};
            Object.defineProperty(o, 'x', {{ value: 1, writable: false }});
            var threw = false;
            try {{ sloppyOuter(o, 2); }} catch (e) {{ threw = e instanceof TypeError; }}
            check(threw, 'strict callee throws under sloppy caller');"
        ),
    );
    run_both(
        "sloppy-callee",
        &format!(
            "{CHECK}{HELPERS}
            var o = {{}};
            Object.defineProperty(o, 'x', {{ value: 1, writable: false }});
            var d = {{}};
            Object.defineProperty(d, 'x', {{ value: 1, configurable: false }});
            var threw = false;
            try {{ strictOuterAfterSloppy(o, d); }}
            catch (e) {{ threw = e instanceof TypeError; }}
            check(o.x === 1, 'sloppy callee ignored under strict caller');
            check(threw, 'strict frame restored after the sloppy return');"
        ),
    );
}
