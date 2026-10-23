# CI

CI runs on this machine through a **self-hosted GitHub Actions runner**.
Every job runs in a fresh Docker container; build output is shared through a
persistent Cargo target volume.

- `CI`: `.github/workflows/ci.yml`
- Extended checks on `master`: `.github/workflows/extended-master.yml`
- Extended checks for a PR (manual): `.github/workflows/extended-pr.yml`
- Performance (manual): `.github/workflows/perf.yml`
- Runner label: `kettevm-ci`
- CI image: `localhost:5000/kettevm-ci:1`
- Image registry: `kettevm-registry` on `127.0.0.1:5000`
- Shared Cargo target: Docker volume `kettevm-cargo-target`
- test262 corpus: Docker volume `kettevm-test262`
- Runner user: `ghrunner` (UID/GID 1501)

## What runs

`CI` on PRs and `master`:
- `format`: `cargo fmt --all --check` and `cargo +nightly fmt --check` in
  `interpreter_become`
- `build`: builds every variant used by the other jobs, including match and
  nightly become release binaries, test262 examples, match GC-stress test
  binaries, and the `interpreter_become` test binaries
- `test`: stable match tests and the `interpreter_become` tests

`Extended checks (master)` runs on every push to `master`:
- `test262-match`
- `test262-become` (nightly become)
- `gc-stress-match`
- `gc-stress-become` (nightly become `interpreter_become` stress suite)

The full `vm` workspace under `--features vm/fast` currently has five known
`vm/tests/elements_ic.rs` failures, so that combination is not gated. Become
coverage comes from `test262-become` and the `interpreter_become` tests.
Benchmark-style tests (`bench`, `deltablue`, `fannkuch`, `nbody`) are excluded
from the become GC-stress run; they still run non-stress in `test`. The match
GC-stress run uses the workspace tests, which do not include those benchmark
files.

The extended jobs run in this order and share the Cargo target cache. Each
master job reports one check run; no duplicate commit status is posted.

`Extended checks (PR)` is manual. It runs the same four checks against the PR
merge commit (or head when there is no merge commit) and posts four commit
statuses on the PR head:
`test262-match`, `test262-become`, `gc-stress-match`, `gc-stress-become`.

Clippy is intentionally commented out in `ci.yml` until the current findings
are cleaned up.

## Shared build

All build/test/extended jobs mount `kettevm-cargo-target` at `/target` and set
`CARGO_TARGET_DIR=/target`. The first job to need a variant builds it; later
jobs reuse it. The `build` job pre-warms every variant, so the test and
extended jobs normally only run binaries.

`CI` runs `format` -> `build` -> `test`. The extended workflows chain
`test262-match` -> `test262-become` -> `gc-stress-match` -> `gc-stress-become`.
With one runner, separate workflows are serialized as well.

The cache is persistent across commits. Cargo fingerprints the source and
feature set, so it is safe to share; a clean rebuild only happens when the
relevant inputs change. Delete and recreate the volume if it ever gets too
large or corrupted.

## First-time setup

Run these on the runner host:

```bash
# 1. Start the local image registry (once; restarts on boot).
docker run -d --restart=unless-stopped --name kettevm-registry \
  -p 127.0.0.1:5000:5000 \
  -v kettevm-registry-data:/var/lib/registry \
  registry:2

# 2. Build and publish the job image.
docker build -t kettevm-ci:1 .github/ci
docker tag kettevm-ci:1 localhost:5000/kettevm-ci:1
docker push localhost:5000/kettevm-ci:1

# 3. Create the shared Cargo target volume.
docker volume create kettevm-cargo-target
docker run --rm -v kettevm-cargo-target:/target alpine chown -R 1501:1501 /target

# 4. Create/fill the test262 volume (no sudo needed).
./ci/github/update-test262.sh

# 5. Install and register the runner. This re-execs with sudo and prompts
#    for a registration token.
./ci/github/install-runner.sh
```

Get the registration token from
`Settings -> Actions -> Runners -> New self-hosted runner`, or after
`gh auth login`:

```bash
gh api -X POST repos/TriedAngle/KetteVM/actions/runners/registration-token --jq .token
```

Check the runner:

```bash
sudo /home/ghrunner/actions-runner/svc.sh status
journalctl -u 'actions.runner.*' -f
```

After changing `.github/ci/Dockerfile`, rebuild and publish the image:

```bash
docker build -t kettevm-ci:1 .github/ci
docker tag kettevm-ci:1 localhost:5000/kettevm-ci:1
docker push localhost:5000/kettevm-ci:1
```

Jobs pull `localhost:5000/kettevm-ci:1` from the local registry. The image is
built on the host rather than in CI so the job container never needs the
Docker socket.

## Branch protection

`master` is protected with these required checks:

- `format`
- `build`
- `test`
- `test262-match`
- `test262-become`
- `gc-stress-match`
- `gc-stress-become`

The rule also requires a PR, requires branches to be up to date, and disallows
force pushes/deletions. Administrators can bypass, so maintainers can still
push directly in an emergency.

A PR cannot merge until `Extended checks (PR)` has been run for its current
head commit and all four statuses passed. A new commit gets a new head SHA, so
rerun the workflow after pushing changes.

## Running extended checks for a PR

- Actions -> **Extended checks (PR)** -> **Run workflow** -> enter the PR
  number, or
- `gh workflow run extended-pr.yml -f pr=123`

It uses the trusted workflow from the default branch, checks out the PR's
merge commit when available, runs all four checks sequentially, and posts the
four statuses on the PR head. It does not run the PR's own workflow file, so
it is also the safer way to test an outside PR. Dispatch it on `master` (the
default); do not select a PR branch.

The manual jobs are named `pr-*` on the dispatch ref. They do not create
`test262-*` or `gc-stress-*` check runs on master; the required PR checks are
the commit statuses. Master pushes use the `extended-master` job checks
directly, without a second commit status, so each extended check appears once.

The manual Actions run itself is attached to `master` (the dispatch ref); the
PR result is the commit status. A failed manual run can therefore show a red
`pr-*` check on master's commit even though master itself is fine.

## test262 baseline

Both engines use the same gate in `.github/test262-baseline`:

```text
min_pass=11745
max_panic=45
```

Current actual counts on test262 main `d53ebc8...`:

- match: `pass=11745`, `panic=45`
- become: `pass=11828`, `panic=45`

The shared minimum is the lower of the two pass counts. To see raw counts
without the gate:

```bash
TEST262_ENGINES=match  TEST262_MIN_PASS=0 TEST262_MAX_PANIC=999 CI_TEST262=1 ./ci/github/ci.sh
TEST262_ENGINES=become TEST262_MIN_PASS=0 TEST262_MAX_PANIC=999 CI_TEST262=1 ./ci/github/ci.sh
```

By default the update script syncs the corpus to the commit in
`.github/test262-baseline`, so CI stays reproducible:

```bash
./ci/github/update-test262.sh
```

To advance to the latest test262 `main`:

```bash
TEST262_COMMIT=main ./ci/github/update-test262.sh
TEST262_ENGINES="match become" CI_TEST262=1 ./ci/github/ci.sh
```

Then update `.github/test262-baseline` (`corpus_commit`, `min_pass`,
`max_panic`) and the README test262 count badges to the new intentional
values.

## Badges

The README CI badge tracks the `CI` workflow for `master` pushes. The two
test262 badges show the actual calibrated pass counts and are updated when
the baseline is recalibrated.

## Performance runs

The performance workflow is manual only:

- Actions -> Performance -> Run workflow, or
- `gh workflow run perf.yml`

It builds the stable match-loop binary and the nightly fast binary, runs
`benchmarks/bench.sh`, and uploads `perf-results.md`.

## Local checks

```bash
./ci/github/ci.sh              # format, build, and tests
CI_TEST262=1 ./ci/github/ci.sh # also run test262 for both engines
```

The local script uses the host Rust toolchains; the CI jobs use the pinned
toolchains in `kettevm-ci:1`.
