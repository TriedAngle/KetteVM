# CI

CI runs on this machine through a **self-hosted GitHub Actions runner**.
Every job runs in a fresh Docker container; the host runner only keeps the
workspace and the CI image.

- Workflow: `.github/workflows/ci.yml`
- Test262 workflow (master + manual PR): `.github/workflows/test262.yml`
- Performance workflow (manual): `.github/workflows/perf.yml`
- Runner label: `kettevm-ci`
- CI image: `kettevm-ci:1` from `.github/ci/Dockerfile`
- Runner user: `ghrunner` (UID/GID 1501)

## What runs

`regression` (PRs and `master`, workflow `CI`):
- formatting for the workspace and `interpreter_become`
- `cargo test --workspace --locked`
- `cargo test --workspace --locked --features vm/stress-minor-gc`
- stable release build
- nightly fast-interpreter release build
- `interpreter_become` tests

This is the fast check that runs on every PR.

`test262` (workflow `Test262`):
- runs automatically on every push to `master`
- for a PR, runs when manually dispatched with the PR number
- runs the corpus mounted read-only from the `kettevm-test262` Docker volume
- gates on `min_pass` / `max_panic` in `.github/test262-baseline`
- uploads the full `test262.log` as an artifact
- posts a `test262` commit status on the PR head or on the master commit

Clippy is intentionally commented out in the workflow until the current
findings are cleaned up.

## Running test262

On `master` it runs automatically on every push, so every merge is covered.

For a PR, start it when the PR is ready:

- Actions -> **Test262** -> **Run workflow** -> enter the PR number, or
- `gh workflow run test262.yml -f pr=123`

The PR run uses the trusted workflow from the default branch, checks out the
PR's merge commit when GitHub has one (otherwise the head), and posts a
`test262` commit status on the PR head. It does not run the PR's own workflow
file, so it is also the safer way to test an outside PR. Dispatch it on
`master` (the default); do not select a PR branch.

`test262` is required for merging. A PR shows `test262` as expected until the
workflow is run and passes. A new commit gets a new head SHA, so rerun the
workflow after pushing changes. The previous run is not reused.

The manual Actions run itself is attached to `master` (the dispatch ref); the
PR result is the `test262` commit status. A failed manual run can therefore
show a red check on master's commit even though master itself is fine. Require
`test262`, not the workflow's `run` job.

test262 runs in a fresh container, so the first job includes compiling the
example; the actual parallel test run is a few seconds. If you want every
run to be that fast, a persistent cargo target cache can be added later.

## First-time setup

Run these on the runner host:

```bash
# 1. Build the job image (as a user that can access Docker).
docker build -t kettevm-ci:1 .github/ci

# 2. Create/fill the test262 volume (no sudo needed).
./ci/github/update-test262.sh

# 3. Install and register the runner. This re-execs with sudo and prompts
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

After changing `.github/ci/Dockerfile` (Rust version, system packages, UID,
...), rebuild `kettevm-ci:1` on the runner host; jobs pick up the new image
automatically. The image is intentionally built on the host rather than in CI
so the job container never needs the Docker socket.

## Trusted PRs vs. outside PRs

Repository collaborators (write access) run CI automatically. Fork PRs do
not run until a maintainer approves them:

- PR -> Checks -> **Approve and run workflows**.

Before approving, review the PR's workflow changes. A fork PR can edit
`.github/workflows/ci.yml`, and that edited workflow is what runs on this
machine. The job container limits the blast radius, but approval is the
trust boundary.

The **Test262** workflow's manual PR mode is safer for outside PRs: it uses
the workflow file from the default branch and only checks out the PR's code.
You still need to review changes to the test runner and test262 script,
since those are part of the code being tested.

Recommended repository settings:

- Settings -> Actions -> General:
  - Fork pull request workflows: require approval for **all outside/external
    contributors** (not just first-time contributors).
  - Workflow permissions: **Read repository contents and packages permissions**.
  - Do not allow GitHub Actions to create or approve pull requests.
- Settings -> Branches -> `master`:
  - Require a pull request before merging.
  - Require the `regression` status check.
  - Require the `test262` status check. Every PR must have a manually-run,
    passing `test262` status on its current head commit before it can merge.
  - Require branches to be up to date before merging.
  - Leave administrator bypass enabled if you still want emergency direct pushes.

GitHub treats `skipped` required checks as acceptable. That is why test262 is
a separate workflow that posts a commit status only when it actually runs,
instead of a label-filtered job that would show up as skipped.

`master` shows the normal green check / red X from both the `CI` and `Test262`
workflows. The README badge tracks `CI`.

## Updating test262

By default the update script syncs the volume to the commit in
`.github/test262-baseline`, so CI stays reproducible:

```bash
./ci/github/update-test262.sh
```

To advance to the latest test262 `main`:

```bash
TEST262_COMMIT=main ./ci/github/update-test262.sh
CI_TEST262=1 ./ci/github/ci.sh
```

Then update `.github/test262-baseline` (`corpus_commit`, `min_pass`,
`max_panic`) to the new intentional values. To see the raw counts without the
gate:

```bash
TEST262_MIN_PASS=0 TEST262_MAX_PANIC=999 CI_TEST262=1 ./ci/github/ci.sh
```

The script warns when the mounted corpus commit differs from the calibrated
`corpus_commit`, so update the baseline deliberately after pulling test262.

## Performance runs

The performance workflow is manual only:

- Actions -> Performance -> Run workflow, or
- `gh workflow run perf.yml`

It builds the stable match-loop binary and the nightly fast binary, runs
`benchmarks/bench.sh`, and uploads `perf-results.md`. Add thresholds there
once the numbers are stable enough to gate on. If you want QuickJS/Node
comparison columns, add those binaries to the CI image or install them in the
job first.

## Local checks

```bash
./ci/github/ci.sh              # fast checks, same commands as the regression job
CI_TEST262=1 ./ci/github/ci.sh # also run test262 in the CI image
```

The local script uses the host Rust toolchains; the CI job uses the pinned
toolchains in `kettevm-ci:1`.
