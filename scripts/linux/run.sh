#!/usr/bin/env bash
# Runs a command in the Linux development container (scripts/linux/Dockerfile), building the
# image first if needed. The repository is mounted at /src; Cargo's registry and the build
# output (CARGO_TARGET_DIR=/target) live in Docker volumes, apart from the Windows build.
#
# Usage (from the repository, in Git Bash or WSL): scripts/linux/run.sh COMMAND...
#   scripts/linux/run.sh cargo test -p reminedog-agent
#   scripts/linux/run.sh scripts/linux/smoke.sh --agent /target/release/libreminedog_linux.so
set -euo pipefail

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
image=reminedog-linux
docker build -q -t "$image" "$repo/scripts/linux" >/dev/null
# Git Bash would turn /src into a Windows path.
MSYS_NO_PATHCONV=1 docker run --rm \
  -v "$repo:/src" \
  -v reminedog-cargo-registry:/usr/local/cargo/registry \
  -v reminedog-target:/target \
  -e CARGO_TARGET_DIR=/target \
  "$image" "$@"
