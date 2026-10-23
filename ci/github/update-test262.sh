#!/usr/bin/env bash
# Create or update the test262 corpus in the Docker volume used by CI.
# Run this on the runner host as a user that can talk to the Docker daemon.
#
#   ./ci/github/update-test262.sh                     # sync to baseline
#   TEST262_COMMIT=main ./ci/github/update-test262.sh # advance to latest
#   TEST262_COMMIT=<sha> ./ci/github/update-test262.sh # pin a corpus
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
baseline_file="$root/.github/test262-baseline"

volume="${TEST262_VOLUME:-kettevm-test262}"
image="${TEST262_GIT_IMAGE:-alpine/git:latest}"
baseline_commit="$(awk -F= '/^corpus_commit=/{print $2; exit}' "$baseline_file" 2>/dev/null || true)"
ref="${TEST262_COMMIT:-${baseline_commit:-main}}"

if ! command -v docker >/dev/null 2>&1; then
    echo "docker is required" >&2
    exit 1
fi

if ! docker volume inspect "$volume" >/dev/null 2>&1; then
    docker volume create "$volume" >/dev/null
fi

git_run() {
    docker run --rm -v "$volume:/test262" "$image" -C /test262 "$@"
}

if ! git_run rev-parse --git-dir >/dev/null 2>&1; then
    echo "cloning test262 into volume $volume"
    docker run --rm -v "$volume:/test262" "$image" \
        clone --depth 1 https://github.com/tc39/test262.git /test262
fi

echo "updating test262 volume $volume to $ref"
git_run fetch --depth 1 origin "$ref"
git_run reset --hard FETCH_HEAD
git_run clean -fdx

commit="$(git_run rev-parse HEAD)"
echo "test262 volume $volume is at $commit"
