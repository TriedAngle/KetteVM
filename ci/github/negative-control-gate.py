#!/usr/bin/env python3
"""The negative-control gate's runner (invoked by the workflow).

A FILE (not a heredoc): running a script puts its own directory first
on sys.path, so `import region_diff` resolves regardless of cwd or
GITHUB_WORKSPACE quirks. Env: BRANCHES COSMETIC BASE_SHA HEAD_SHA.
"""
import json
import os
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import region_diff

branches = json.loads(os.environ["BRANCHES"])
cosmetic = set(json.loads(os.environ.get("COSMETIC", "[]")))
base, head = os.environ["BASE_SHA"], os.environ["HEAD_SHA"]

def sh(*cmd, check=True):
    return subprocess.run(cmd, capture_output=True, text=True, check=check)

def spans_for(file, text):
    # Write the version to a temp file and ask test-spans.
    import tempfile
    with tempfile.NamedTemporaryFile("w", suffix=".rs", delete=False) as f:
        f.write(text); tmp = f.name
    out = sh("cargo", "run", "-q", "-p", "test-spans", "--", tmp, check=False)
    os.unlink(tmp)
    if out.returncode != 0:
        return None  # unparseable: treat as no spans (whole file rules)
    try:
        return json.loads(out.stdout.strip().splitlines()[-1])
    except Exception:
        return None

region_diff.spans_of = spans_for

# ── Touched test FILES (deletions exempt) ──
diff = sh("git", "diff", "--name-only", "--diff-filter=ACMR", base, head).stdout
touched = sorted(l for l in diff.splitlines() if l.endswith(".rs"))
if not touched:
    print("no touched rust files — gate passes vacuously")
    sys.exit(0)
print("touched files:")
for t in touched:
    print("  ", t)

# ── Per-file: which TESTS changed (base vs head)? ──
# This is the coverage unit now: a test (by span path).
head_texts, base_texts = {}, {}
changed_tests = {}   # (file, test_path) -> kind
span_kind = {}       # (file, test_path) -> "fn" | "mod"
for f in touched:
    b = sh("git", "show", f"{base}:{f}", check=False).stdout
    h = sh("git", "show", f"{head}:{f}", check=False).stdout
    if b == h:
        continue
    base_texts[f], head_texts[f] = b, h
    sp_a = spans_for(f, b) or []
    sp_h = spans_for(f, h) or []
    for s in sp_h:
        span_kind[(f, s["path"])] = s.get("kind", "fn")
    for s in sp_a:
        span_kind.setdefault((f, s["path"]), s.get("kind", "fn"))
    for path, kind in region_diff.changed_test_regions(f, sp_a, b, sp_h, h):
        changed_tests[(f, path)] = kind
if not changed_tests:
    print("no changed test regions — gate passes (library-only PR)")
    sys.exit(0)
print("changed tests:")
for (f, p), k in sorted(changed_tests.items()):
    print(f"   {f} :: {p} ({k})")

# Cosmetic opt-out: listed tests drop out of coverage AND
# rule 1 (human reviewers verify the claim).
dropped = [(f, p) for (f, p) in changed_tests if p in cosmetic or f"{f}::{p}" in cosmetic]
for d in dropped:
    print(f"   cosmetic opt-out: {d[0]} :: {d[1]} (human-verified)")
need = {k: v for k, v in changed_tests.items() if k not in dropped}
# Coverage is about RUNNABLE tests: a cfg(test) MODULE span ("mod")
# is diffed by rule 1 but maps to no test (cargo test -- mod
# --exact matches nothing); a removed test no longer runs anywhere.
need = {k: v for k, v in need.items()
        if span_kind.get(k, "fn") != "mod" and v != "removed"}
if not need:
    print("gate ok: every changed test is cosmetic-opt-out")
    sys.exit(0)

# ── Rule 1 (span-level): on each declared branch, every
# ── NOT-opted-out test region must be cosmetic-equal.  ──
# Every violation is collected and reported — one round of fixing.
missing, fetched, violations = [], {}, []
for b in branches:
    ref = b[1:] if b.startswith("/") else b
    if sh("git", "fetch", "--depth=1", "origin", ref, check=False).returncode != 0:
        missing.append(ref)
        continue
    sha = sh("git", "rev-parse", f"origin/{ref}").stdout.strip()
    fetched[ref] = sha
    for f in head_texts:
        br = sh("git", "show", f"{sha}:{f}", check=False).stdout
        sp_b = spans_for(f, br) or []
        sp_h = spans_for(f, head_texts[f]) or []
        for path, kind in region_diff.changed_test_regions(f, sp_b, br, sp_h, head_texts[f]):
            if kind == "changed" and (f, path) not in dropped:
                violations.append((ref, f, path))
for ref in missing:
    print(f"REJECT: branch {ref} not found")
for ref, f, path in violations:
    print(f"REJECT: {ref} changes test {f} :: {path} beyond whitespace")
if missing or violations:
    sys.exit(1)
if branches:
    print("rule 1 ok: every branch is test-region-identical (whitespace-neutral)")
else:
    print("no declared branches — self-control is the only path")

# ── The RUNNER: how a test file/target executes. ──
def runner_of(file):
    if file.startswith("tests/regression/") and file.endswith(".js"):
        return ("regression", file)
    parts = file.split("/tests/")
    if len(parts) == 2 and parts[0] and parts[1].endswith(".rs"):
        return ("cargo", (parts[0].split("/")[-1], parts[1][:-3]))
    # Inline #[cfg(test)] tests inside a crate's own sources run via
    # the lib target; the span path is already module-qualified
    # (e.g. "tests::inline_add_works") and fits --exact directly.
    parts = file.split("/src/")
    if len(parts) == 2 and parts[0] and parts[1] in ("lib.rs", "main.rs"):
        return ("unit", (parts[0].split("/")[-1],))
    return None

def run_file(file, check=False):
    r = runner_of(file)
    if r is None:
        return None
    if r[0] == "regression":
        return sh("cargo", "run", "--locked", "-p", "vm", "--example", "regression", "--", r[1], check=check)
    pkg, stem = r[1]
    return sh("cargo", "test", "--locked", "-p", pkg, "--test", stem, check=check)

def run_test(file, path, check=False):
    r = runner_of(file)
    if r is None:
        return None
    if r[0] == "regression":
        return run_file(file, check=check)
    if r[0] == "unit":
        (pkg,) = r[1]
        return sh("cargo", "test", "--locked", "-p", pkg, "--lib", "--",
                  path, "--exact", check=check)
    pkg, stem = r[1]
    # The filter and --exact must be SEPARATE argv elements — a single
    # "path --exact" string matches no test and passes vacuously (the
    # C7/C10/C15 matrix rounds failed exactly there).
    return sh("cargo", "test", "--locked", "-p", pkg, "--test", stem, "--",
              path, "--exact", check=check)

def build(check=False):
    return sh("cargo", "build", "--locked", "--workspace", "--all-targets", check=check)

files = sorted({f for f, _ in need})
runnables = [(f, runner_of(f)) for f in files]
runnables = [(f, r) for f, r in runnables if r is not None]
if not runnables:
    print("changed tests map to no runner — gate passes (review covers them)")
    sys.exit(0)

# ── Self-control: the LAST commit touching the FILE, the  ──
# ── specific test must FAIL there.                        ──
self_caught = set()
for (f, p), kind in sorted(need.items()):
    out = sh("git", "rev-list", base + ".." + head, "--", f).stdout
    shas = out.split()
    if not shas:
        continue
    sha = shas[0]
    sh("git", "checkout", "-q", "--detach", sha)
    if build(check=False).returncode != 0:
        print(f"  self-control {f} :: {p}: last-touch commit does not build — no proof")
        continue
    if run_test(f, p, check=False).returncode != 0:
        print(f"  self-control {f} :: {p}: FAILS in its own last-touch commit")
        self_caught.add((f, p))
sh("git", "checkout", "-q", head)

need_b = {k: v for k, v in need.items() if k not in self_caught}
if not need_b:
    print("gate ok: every changed test is its own negative control")
    sys.exit(0)
if not branches:
    print("REJECT: changed tests with neither self-control nor declared branches:")
    for (f, p) in sorted(need_b):
        print(f"   {f} :: {p}")
    sys.exit(1)

# ── Branches: each remaining test must fail somewhere. ──
# Broken branches don't abort the sweep: every branch is built and
# every broken one reported, so one CI round lists everything.
results = {k: False for k in need_b}
broken = []
for ref, sha in sorted(fetched.items()):
    sh("git", "checkout", "-q", "--detach", sha)
    print(f"=== {ref}: building")
    if build(check=False).returncode != 0:
        broken.append(ref)
        continue
    for (f, p) in sorted(need_b):
        failed = run_test(f, p, check=False).returncode != 0
        print(f"  {f} :: {p}: {'FAIL (caught)' if failed else 'pass'}")
        if failed:
            results[(f, p)] = True

missed = [k for k, caught in results.items() if not caught]
rejected = False
if missed:
    print("REJECT: tests that pass on EVERY negative-control branch:")
    for (f, p) in sorted(missed):
        print(f"   {f} :: {p}")
    rejected = True
for ref in broken:
    print(f"REJECT: branch {ref} does not build — a broken negative control proves nothing")
    rejected = True
if rejected:
    sys.exit(1)
print("gate ok: every changed test fails on at least one branch")
