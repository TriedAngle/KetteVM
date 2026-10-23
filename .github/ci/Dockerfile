# CI image for KetteVM.
#
# Build and publish to the local registry on the runner host:
#   docker build -t kettevm-ci:1 .github/ci
#   docker tag kettevm-ci:1 localhost:5000/kettevm-ci:1
#   docker push localhost:5000/kettevm-ci:1
#
# The UID/GID must match the dedicated `ghrunner` host user created by
# ci/github/install-runner.sh. GitHub runs job containers as the image's
# USER, so the mounted workspace is writable and cleanable by the runner.
FROM rust:1.98-bookworm

ARG RUNNER_UID=1501
ARG RUNNER_GID=1501

RUN apt-get update \
    && apt-get install -y --no-install-recommends git python3 \
    && rm -rf /var/lib/apt/lists/*

# Pinned because interpreter_become uses nightly-only features. Symlink the
# dated toolchain as `nightly` so `cargo +nightly` (and the repo's
# rust-toolchain.toml) stay on the pin instead of auto-installing latest.
RUN rustup component add rustfmt clippy \
    && rustup toolchain install nightly-2026-09-27 --profile minimal \
    && rustup component add rustfmt clippy --toolchain nightly-2026-09-27 \
    && ln -sfn \
        /usr/local/rustup/toolchains/nightly-2026-09-27-x86_64-unknown-linux-gnu \
        /usr/local/rustup/toolchains/nightly-x86_64-unknown-linux-gnu

RUN groupadd -g "${RUNNER_GID}" runner \
    && useradd -m -u "${RUNNER_UID}" -g "${RUNNER_GID}" -s /bin/bash runner \
    && mkdir -p /cargo /workspace \
    && chown -R runner:runner /cargo /workspace

ENV CARGO_HOME=/cargo \
    CARGO_INCREMENTAL=0 \
    CARGO_TERM_COLOR=always \
    RUSTUP_AUTO_INSTALL=0 \
    RUST_BACKTRACE=1

USER runner
WORKDIR /workspace
