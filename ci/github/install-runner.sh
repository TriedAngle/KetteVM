#!/usr/bin/env bash
# Install/register the KetteVM self-hosted GitHub Actions runner.
#
# Run from the repo root (or anywhere); it will re-exec itself with sudo:
#   ./ci/github/install-runner.sh
#
# The registration token is read from RUNNER_TOKEN if set, otherwise prompted
# for. Create one at:
#   https://github.com/TriedAngle/KetteVM/settings/actions/runners/new
# or, after `gh auth login`:
#   gh api -X POST repos/TriedAngle/KetteVM/actions/runners/registration-token --jq .token
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$root"

if [[ ${EUID} -ne 0 ]]; then
    exec sudo -E bash "$0" "$@"
fi

RUNNER_VERSION="${RUNNER_VERSION:-2.338.1}"
RUNNER_USER="${RUNNER_USER:-ghrunner}"
RUNNER_UID="${RUNNER_UID:-1501}"
RUNNER_GID="${RUNNER_GID:-1501}"
RUNNER_HOME="${RUNNER_HOME:-/home/$RUNNER_USER}"
RUNNER_DIR="${RUNNER_DIR:-$RUNNER_HOME/actions-runner}"
REPO_URL="${REPO_URL:-https://github.com/TriedAngle/KetteVM}"
RUNNER_NAME="${RUNNER_NAME:-$(hostname -s)-kettevm-ci}"
RUNNER_LABELS="${RUNNER_LABELS:-kettevm-ci}"

for cmd in curl tar docker sudo; do
    if ! command -v "$cmd" >/dev/null 2>&1; then
        echo "missing required command: $cmd" >&2
        exit 1
    fi
done

if ! getent group "$RUNNER_USER" >/dev/null 2>&1; then
    groupadd -g "$RUNNER_GID" "$RUNNER_USER"
fi
if ! id "$RUNNER_USER" >/dev/null 2>&1; then
    useradd -m -u "$RUNNER_UID" -g "$RUNNER_USER" -s /bin/bash "$RUNNER_USER"
fi

actual_uid="$(id -u "$RUNNER_USER")"
actual_gid="$(id -g "$RUNNER_USER")"
if [[ "$actual_uid" != "$RUNNER_UID" || "$actual_gid" != "$RUNNER_GID" ]]; then
    echo "user $RUNNER_USER is UID:GID $actual_uid:$actual_gid, expected $RUNNER_UID:$RUNNER_GID" >&2
    echo "Change RUNNER_UID/RUNNER_GID and the Dockerfile ARGs together." >&2
    exit 1
fi

if ! getent group docker >/dev/null 2>&1; then
    echo "the docker group does not exist; install Docker first" >&2
    exit 1
fi
usermod -aG docker "$RUNNER_USER"
if ! sudo -u "$RUNNER_USER" -H docker info >/dev/null 2>&1; then
    echo "warning: $RUNNER_USER cannot talk to Docker yet; a new login/service start may be needed" >&2
fi

install -d -o "$RUNNER_USER" -g "$RUNNER_GID" -m 0755 "$RUNNER_HOME"
install -d -o "$RUNNER_USER" -g "$RUNNER_GID" -m 0755 "$RUNNER_HOME/_work"

if [[ ! -x "$RUNNER_DIR/config.sh" ]]; then
    echo "downloading actions runner v$RUNNER_VERSION"
    archive="$(mktemp -t actions-runner.XXXXXX.tar.gz)"
    trap 'rm -f "$archive"' EXIT
    curl -fL --retry 3 \
        -o "$archive" \
        "https://github.com/actions/runner/releases/download/v${RUNNER_VERSION}/actions-runner-linux-x64-${RUNNER_VERSION}.tar.gz"
    mkdir -p "$RUNNER_DIR"
    tar xzf "$archive" -C "$RUNNER_DIR"
    rm -f "$archive"
    trap - EXIT
    chown -R "$RUNNER_USER:$RUNNER_GID" "$RUNNER_DIR"
fi

if [[ -x "$RUNNER_DIR/svc.sh" ]]; then
    "$RUNNER_DIR/svc.sh" stop >/dev/null 2>&1 || true
fi

token="${RUNNER_TOKEN:-}"
if [[ -z "$token" ]]; then
    read -rsp "GitHub runner registration token: " token < /dev/tty
    echo > /dev/tty
fi

cd "$RUNNER_DIR"
sudo -u "$RUNNER_USER" -H \
    ./config.sh \
    --unattended \
    --replace \
    --url "$REPO_URL" \
    --token "$token" \
    --name "$RUNNER_NAME" \
    --labels "$RUNNER_LABELS" \
    --work "$RUNNER_HOME/_work"
unset token

./svc.sh install "$RUNNER_USER"
./svc.sh start
status_output="$(./svc.sh status 2>&1 || true)"
echo "$status_output"
if ! grep -q 'active (running)' <<<"$status_output"; then
    echo "warning: runner service does not look active" >&2
    journalctl -u 'actions.runner.*' --no-pager -n 50 || true
fi

cat <<EOF

Runner registered as "$RUNNER_NAME" with label "$RUNNER_LABELS".

Next:
  docker build -t kettevm-ci:1 .github/ci
  docker tag kettevm-ci:1 localhost:5000/kettevm-ci:1
  docker push localhost:5000/kettevm-ci:1
  ./ci/github/update-test262.sh

The runner needs the local registry, the published image, and the
kettevm-test262 volume before jobs can run.
EOF
