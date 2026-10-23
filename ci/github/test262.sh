#!/usr/bin/env bash
# Run the test262 corpus and gate on a pass-count baseline.
#
# The runner intentionally has known failures, so its own exit code is not
# the gate. The baselines live in .github/test262-baseline; update them
# deliberately when coverage changes.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$root"

test262="${TEST262_DIR:-/test262}"
baseline_file="$root/.github/test262-baseline"
log="$root/test262.log"
engine="${TEST262_ENGINE:-match}"
# The baseline in .github/test262-baseline was calibrated with 64 workers.
export KETTEVM_JOBS="${KETTEVM_JOBS:-64}"

case "$engine" in
    match | become) ;;
    *)
        echo "TEST262_ENGINE must be 'match' or 'become', got '$engine'" >&2
        exit 2
        ;;
esac

if [[ ! -f "$test262/harness/sta.js" || ! -f "$test262/harness/assert.js" || ! -d "$test262/test" ]]; then
    echo "test262 corpus not found at $test262" >&2
    echo "Create/update the corpus with ci/github/update-test262.sh." >&2
    exit 2
fi

min_pass="${TEST262_MIN_PASS:-}"
if [[ -z "$min_pass" ]]; then
    min_pass="$(awk -F= '$1 == "min_pass" { print $2; exit }' "$baseline_file")"
fi
max_panic="${TEST262_MAX_PANIC:-}"
if [[ -z "$max_panic" ]]; then
    max_panic="$(awk -F= '$1 == "max_panic" { print $2; exit }' "$baseline_file")"
fi
: "${min_pass:?missing min_pass in $baseline_file}"
: "${max_panic:?missing max_panic in $baseline_file}"

baseline_commit="$(awk -F= '/^corpus_commit=/{print $2; exit}' "$baseline_file")"
if corpus_commit="$(git -c safe.directory="$test262" -C "$test262" rev-parse HEAD 2>/dev/null)"; then
    echo "test262 corpus: $corpus_commit"
    if [[ -n "$baseline_commit" && "$corpus_commit" != "$baseline_commit" ]]; then
        echo "::warning::test262 corpus is $corpus_commit but the baseline was calibrated on $baseline_commit"
    fi
else
    echo "test262 corpus: $test262 (not a git checkout)"
fi

if [[ "$engine" == "become" ]]; then
    cargo_cmd=(cargo +nightly run --release --locked -p vm --example test262 --features fast --)
else
    cargo_cmd=(cargo run --release --locked -p vm --example test262 --)
fi

set +e
"${cargo_cmd[@]}" \
    "$test262/harness/sta.js" \
    "$test262/harness/assert.js" \
    "$test262/test" >"$log" 2>&1
runner_status=$?
set -e

# Print the runner summary, then a bounded failure list. The complete log is
# uploaded as an artifact by the workflow.
sed -n '/^total:/,/^skipped panic:/p' "$log" || true
failure_lines="$(grep -cE '^  (FAIL|PANIC) ' "$log" || true)"
if (( failure_lines > 100 )); then
    echo "test262[$engine]: showing first 100 of $failure_lines failures; full log is the test262[$engine]-log artifact"
fi
grep -E '^  (FAIL|PANIC) ' "$log" | head -n 100 || true

pass="$(awk '/^pass:/{print $2; exit}' "$log")"
panic="$(awk '/^panic:/{print $2; exit}' "$log")"
total="$(awk '/^total:/{print $2; exit}' "$log")"
fail="$(awk '/^fail:/{print $2; exit}' "$log")"

if [[ -z "$pass" || -z "$panic" || -z "$total" || -z "$fail" ]]; then
    echo "::error::test262[$engine] runner did not produce a summary (exit $runner_status)"
    tail -n 100 "$log" || true
    exit "${runner_status:-1}"
fi

echo
echo "test262[$engine]: total=$total pass=$pass fail=$fail panic=$panic"
echo "test262[$engine]: baseline min_pass=$min_pass max_panic=$max_panic"

if (( total == 0 )); then
    echo "::error::test262[$engine] ran zero tests"
    exit 1
fi
if (( pass < min_pass )); then
    echo "::error::test262[$engine] pass count $pass is below baseline $min_pass"
    exit 1
fi
if (( panic > max_panic )); then
    echo "::error::test262[$engine] panic count $panic exceeds baseline $max_panic"
    exit 1
fi

if (( runner_status != 0 )); then
    echo "test262[$engine] runner exited $runner_status (known failures/panics); pass-count gate passed"
fi
