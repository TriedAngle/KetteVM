# Safety
never use unsafe unless explicetely allowed, if you think unsafe is solving a problem or helps maximizing performance, please do not hesitate to ask/discuss this case.

## GC Safety
`Tagged<'h, T>` takes `h` to statically proof no directly borrowed value to the heap is crossing a gc safepoint.
purpesfully violating this by passing around plain `Value` or `Word` and using `unsafe` destroys this proof and may kill values across gc-safepoints if the heap moves the value. Functions that are allocation free but require heap access (for example to reference more heap values) should thus always use an immutable Heap borrow. If a scope needs to call a function that requires a mutable heap borrow, it must most often root these values to handles or use all the Tagged<'h, T> before (preferred if possible).

# Breaking Changes & Backwards Compatibility
this is a fresh codebase, legacy code is no concern, do not keep old/dead code around or try to integrate or keep backwards compatibility on broken things.
we strive for perfection not for legacy.

# Code Conventions
## Helper Functions
avoid writing helper functions with only 1-2 line bodies.

## Visibility
Never use `pub(crate)`, always plain `pub` for anything that needs to be accessible across module, or just plain "private" fn for module local functions.

## Imports and cross module references
never use `crate::module::Type/function` inside code, always import Type/function instead.

## Comments
- do not use special characters outside of ascii (unless if it **really** makes sense)
- keep comments short and minimal, only write them when they are actually useful, do not write comments that are "thinking or discussion traces" and do not make sense outside of conversations with a user.

## Method vs namespace function
Can the function trigger a GC (does it need `&mut Heap`, or take `Handle`s because it allocates)?

- **No GC** (only `&Heap` or nothing besides the receiver): make it a method.
  `obj.map(heap)`, `v.is_truthy(heap)`, `key.classify_key(heap)`
- **Can GC**: keep it a namespace function. `heap` always comes first, then the
  objects it operates on.
  `Transition::define_own_property(heap, scope, receiver, name, desc)`,
  `Convert::to_string(heap, scope, v)`

Cross-cutting notifications keep their domain namespace even when GC-free
(e.g. `Prototype::shape_changed(heap, map)` — it notifies the prototype
registry, it is not an operation on the map).

Only `Tagged` auto-derefs to its pointee. `Handle` has no `Deref` on purpose: a
`&self` behind a handle deref could dangle once the GC moves the object, so
GC-able operations take handles explicitly.

## ECMA function documentation
for intrinsic or native functions that implement specific ECMA standard functions always document them like this:
```
/// function name
/// [ecma id](link)
/// very short description
```

for example:
```
/// `Array ( ...values )`
/// [ES 23.1.1.1](https://tc39.es/ecma262/#sec-array-constructor-array)
/// Creates an array from `values`; the prototype comes from `new.target`.
pub fn array_constructor<'a>(...) { ... }
```

if some steps in the ecma script are currently not supported by the VM and we only follow the spec partially, 
always note with a `// TODO: <missing feature description>` at exactly the place that missmatches the spec.

## Test PRs: prove the tests can fail (negative control)

Every test-only or test-adding PR must include a **fault-injection
check (CI-enforced by the `negative-control` gate): inject a
production fault the OLD tests miss; the new tests must go red while
every existing test stays green.** Declare the fault branches in the
PR body under a `Negative Control:` fenced block — the gate verifies
the branches are test-identical to the PR and that every touched test
FAILS on at least one of them. A touched test that fails in its own
LAST touching commit is its own negative control (the test-first RED
commit satisfies the gate without branches).

Test files = anything under a `tests/` directory:
- `<crate>/tests/<name>.rs` runs as `cargo test -p <crate> --test <name>`
- `tests/regression/**.js` runs via `cargo run -p vm --example regression -- <file>`

Rules: a declared branch must be test-identical to the PR head (any
test diff rejects); a missing branch or a non-building branch rejects
(a broken control proves nothing); deletions are exempt; no
declaration + uncaught touched tests rejects.
