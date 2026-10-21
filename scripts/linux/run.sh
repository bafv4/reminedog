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
# The container runs as root. On a Linux host (WSL included), files it leaves in the
# repository (dist/, installer/build/) are handed back to the user afterwards; on Windows'
# drives the owner does not matter.
owner=''
if [ "$(uname -s)" = Linux ] && [ "$(id -u)" != 0 ]; then
  owner=$(id -u):$(id -g)
fi
# Git Bash would turn /src into a Windows path.
MSYS_NO_PATHCONV=1 docker run --rm \
  -v "$repo:/src" \
  -v reminedog-cargo-registry:/usr/local/cargo/registry \
  -v reminedog-target:/target \
  -e CARGO_TARGET_DIR=/target \
  -e "HOST_OWNER=$owner" \
  "$image" bash -c '
    status=0
    "$@" || status=$?
    if [ -n "$HOST_OWNER" ]; then
      find /src -xdev -user 0 -exec chown "$HOST_OWNER" {} + 2>/dev/null || true
    fi
    exit "$status"
  ' run.sh "$@"
