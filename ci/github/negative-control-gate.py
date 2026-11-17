#!/usr/bin/env python3
"""The negative-control gate's runner (invoked by the workflow).

KetteVM's local copy of the servyi/lints reusable gate (same rules,
plus the tests/regression whole-file mapping below).

A FILE (not a heredoc): running a script puts its own directory first
on sys.path, so `import region_diff` resolves regardless of cwd or
GITHUB_WORKSPACE quirks. Env: BRANCHES COSMETIC BASE_SHA HEAD_SHA
SPANS_BIN (optional).
"""
import json
import os
import re
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
    if os.environ.get("SPANS_BIN"):
        out = sh(os.environ["SPANS_BIN"], tmp, check=False)
    else:
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
# ── Branch declarations: local (`/name`) or external (a full
# ── branch URL: https://github.com/O/R/tree/branch) ──
import re as _re
_EXT = _re.compile(r"^https://github\.com/([^/]+)/([^/]+)/tree/(.+)$")

def entry_of(b):
    m = _EXT.match(b)
    if m:
        return ("ext", "https://github.com/%s/%s.git" % (m.group(1), m.group(2)), m.group(3))
    return ("local", None, b[1:] if b.startswith("/") else b)

missing, fetched, violations = [], {}, []
external = {}   # ref -> (repo_url, sha)
for b in branches:
    kind, repo_url, ref = entry_of(b)
    if kind == "local":
        if sh("git", "fetch", "--depth=1", "origin", ref, check=False).returncode != 0:
            missing.append(ref)
            continue
    else:
        if sh("git", "fetch", "--depth=1", repo_url, ref, check=False).returncode != 0:
            missing.append(b)
            continue
    sha = sh("git", "rev-parse", "FETCH_HEAD").stdout.strip()
    fetched[ref] = sha
    if kind == "ext":
        external[ref] = (repo_url, sha)

# ── External verification — BEFORE any external code is built or ──
# ── run: the diff head..external must be limited to non-test Rust ──
# ── lines in files that already exist in the PR head. ──
def head_spans(f):
    txt = sh("git", "show", f"{head}:{f}", check=False).stdout
    return spans_for(f, txt) or [], txt

ext_bad = []
for ref, (repo_url, sha) in sorted(external.items()):
    ns = sh("git", "diff", "--name-status", head, sha).stdout
    for line in ns.splitlines():
        parts = line.split("\t")
        st, path = (parts[0], parts[-1]) if parts else ("", "")
        why = None
        if st != "M":
            why = "not a plain modification (added/deleted/renamed file)"
        elif not path.endswith(".rs"):
            why = "not a Rust source"
        elif sh("git", "cat-file", "-e", f"{head}:{path}", check=False).returncode != 0:
            why = "file does not exist in the PR head"
        else:
            sp, _ = head_spans(path)
            hunks = []
            d = sh("git", "diff", "-U0", head, sha, "--", path).stdout
            for h in _re.finditer(r"^@@[^+]*\+(\d+)(?:,(\d+))? @@", d, _re.M):
                s = int(h.group(1)); e = s + int(h.group(2) or 1) - 1
                if e >= s:
                    hunks.append((s, e))
            hit = [t["path"] for t in sp
                   for (s, e) in hunks
                   if not (e < t["start"] or s > t["end"])]
            if hit:
                why = "diff touches test region(s): " + ", ".join(sorted(set(hit)))
        if why:
            ext_bad.append((ref, path or line, why))
for ref, path, why in ext_bad:
    print(f"REJECT: external {ref}: {path}: {why}")
if ext_bad:
    sys.exit(1)
if external:
    print("external branches verified: diffs limited to non-test Rust lines"
          " in pre-existing files")
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
    # KetteVM regression scripts: whole-file checks run through the
    # vm example; exit-code semantics, no per-test attribution.
    if file.startswith("tests/regression/") and file.endswith(".js"):
        return ("regression", file)
    parts = file.split("/tests/")
    if len(parts) == 2 and parts[0] and parts[1].endswith(".rs"):
        return ("cargo", (manifest_dir(file), parts[1][:-3]))
    # Inline #[cfg(test)] tests inside a crate's own sources run via
    # the lib target; the span path is already module-qualified
    # (e.g. "tests::inline_add_works") and fits --exact directly.
    parts = file.split("/src/")
    if len(parts) == 2 and parts[0] and parts[1] in ("lib.rs", "main.rs"):
        return ("unit", (manifest_dir(file),))
    return None

def manifest_dir(file, sha="HEAD"):
    """Directory of the Cargo.toml that OWNS the file (nearest one up
    the tree, resolved in git so branch checkouts are honored).  Cargo
    is driven by --manifest-path from there: package-name guessing
    breaks when the directory is not a workspace member (a standalone
    tools/ crate) or the dir name differs from the package name."""
    parts = file.split("/")
    for i in range(len(parts) - 1, 0, -1):
        cand = "/".join(parts[:i] + ["Cargo.toml"])
        if sh("git", "cat-file", "-e", f"{sha}:{cand}", check=False).returncode == 0:
            return "/".join(parts[:i])
    return None

def build_cmd(file):
    """The COMPILE-ONLY command for the targets a file's tests run in.

    Per-target on purpose: a whole-workspace --all-targets build is
    neither needed nor universally possible (e.g. rustc-driver-linked
    lint crates do not compile as lib-test targets) - the gate only
    owes proof that THESE tests can build and run.
    """
    r = runner_of(file)
    if r is None:
        return None
    if r[0] == "regression":
        return ["cargo", "build", "--locked", "-p", "vm", "--example", "regression"]
    m = ["cargo", "test", "--locked", "--manifest-path", r[1][0] + "/Cargo.toml"]
    if r[0] == "unit":
        return m + ["--lib", "--no-run"]
    return m + ["--test", r[1][1], "--no-run"]

def builds(file):
    """Does this file's test target COMPILE?  False + stderr tail on
    failure (a broken build proves nothing, and the reason matters)."""
    cmd = build_cmd(file)
    if cmd is None:
        return True  # nothing to compile for this mapping
    r = sh(*cmd, check=False)
    if r.returncode != 0:
        tail = "\n".join((r.stderr or r.stdout or "").strip().splitlines()[-12:])
        print("    build failed ({}):\n{}".format(" ".join(cmd[2:]), tail))
    return r.returncode == 0

def run_test(file, path, check=False):
    """Exit-code run (whole-file checks, or filtered single tests)."""
    r = runner_of(file)
    if r is None:
        return None
    if r[0] == "regression":
        return sh("cargo", "run", "--locked", "-p", "vm", "--example",
                  "regression", "--", r[1], check=check)
    m = ["cargo", "test", "--locked", "--manifest-path", r[1][0] + "/Cargo.toml"]
    if r[0] == "unit":
        return sh(*m, "--lib", "--", path, "--exact", check=check)
    return sh(*m, "--test", r[1][1], "--", path, "--exact", check=check)

def run_target(file, check=False):
    """Run the file's WHOLE test target unfiltered and return the set
    of FAILED test names (libtest prints 'test NAME ... FAILED').

    One run per target replaces per-test --exact invocations, and the
    failed set is what makes the old-tests-stay-green rule possible:
    the fault must be caught ONLY by the PR's own tests."""
    r = runner_of(file)
    if r is None or r[0] == "regression":
        return None
    m = ["cargo", "test", "--locked", "--manifest-path", r[1][0] + "/Cargo.toml"]
    if r[0] == "unit":
        r = sh(*m, "--lib", check=check)
    else:
        r = sh(*m, "--test", r[1][1], check=check)
    failed, passed = set(), set()
    ansi = re.compile(r"\x1b\[[0-9;]*m")
    for line in (r.stdout or "").splitlines():
        line = ansi.sub("", line).strip()
        if line.startswith("test ") and line.endswith("FAILED"):
            failed.add(line[len("test "):-len(" ... FAILED")].strip())
        elif line.startswith("test ") and line.endswith("ok"):
            passed.add(line[len("test "):-len(" ... ok")].strip())
    # a non-zero rc (a test ABORTS: SIGABRT/segv/non-unwinding panic
    # kills the process mid-run, printing NO result line for itself
    # or anything after) means every test without an explicit ok did
    # not pass — callers treat those as failed. Without this, an
    # aborting fault reads as green and the control proves nothing.
    return failed, passed, r.returncode

files = sorted({f for f, _ in need})
runnables = [(f, runner_of(f)) for f in files]
runnables = [(f, r) for f, r in runnables if r is not None]
if not runnables:
    print("changed tests map to no runner — gate passes (review covers them)")
    sys.exit(0)

# ── Self-control: the LAST commit touching the FILE, the  ──
# ── specific test must FAIL there — and NOTHING ELSE may. ──
self_caught = set()
for f in sorted({f for (f, _p) in need}):
    out = sh("git", "rev-list", base + ".." + head, "--", f).stdout
    shas = out.split()
    if not shas:
        continue
    sha = shas[0]
    sh("git", "checkout", "-q", "--detach", sha)
    if not builds(f):
        for (ff, p) in sorted(need):
            if ff == f:
                print(f"  self-control {f} :: {p}: last-touch commit does not build — no proof")
        continue
    failed, passed, rc = run_target(f, check=False)
    mine = {p for (ff, p) in need if ff == f}
    def did_fail(name):
        return name in failed or (rc != 0 and name not in passed)
    for name in sorted(n for n in failed if n not in mine):
        print(f"REJECT: self-control {f}: existing test {name} already fails in the RED commit — "
              "the fault was covered before this PR; inject one the old tests miss")
        sys.exit(1)
    for p in sorted(p for p in mine if did_fail(p)):
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
already = []  # (branch, file, old test name) — faults pre-covered
for ref, sha in sorted(fetched.items()):
    sh("git", "checkout", "-q", "--detach", sha)
    ok = all(builds(f) for (f, _) in sorted(need_b))
    print(f"=== {ref}: {'building' if ok else 'BROKEN'}")
    if not ok:
        broken.append(ref)
        continue
    for f in sorted({f for (f, _p) in need_b}):
        failed, passed, rc = run_target(f, check=False)
        mine = {p for (ff, p) in need_b if ff == f}
        def did_fail(name):
            return name in failed or (rc != 0 and name not in passed)
        for name in sorted(n for n in failed if n not in mine):
            already.append((ref, f, name))
        for (ff, p) in sorted(need_b):
            if ff == f:
                caught = did_fail(p)
                how = ("FAIL (caught)" if p in failed
                       else "ABORT (caught)" if caught else "pass")
                print(f"  {f} :: {p}: {how}")
                if caught:
                    results[(f, p)] = True

for ref, f, name in already:
    print(f"REJECT: branch {ref} — the fault is already caught by existing test {name} ({f}); "
          "inject a fault the OLD tests miss")
missed = [k for k, caught in results.items() if not caught]
rejected = bool(already)
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
