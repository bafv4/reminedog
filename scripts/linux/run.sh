#!/usr/bin/env bash
# Linux の開発用のコンテナ（scripts/linux/Dockerfile）でコマンドを動かす。要ればイメージを先に
# ビルドする。リポジトリは /src にマウントし、Cargo のレジストリとビルドの出力
# （CARGO_TARGET_DIR=/target）は、Windows のビルドとは別に Docker のボリュームに置く。
#
# 使い方（リポジトリで、Git Bash か WSL から）：scripts/linux/run.sh COMMAND...
#   scripts/linux/run.sh cargo test -p reminedog-agent
#   scripts/linux/run.sh scripts/linux/smoke.sh --agent /target/release/libreminedog_linux.so
set -euo pipefail

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
image=reminedog-linux
docker build -q -t "$image" "$repo/scripts/linux" >/dev/null
# コンテナは root で動く。Linux のホスト（WSL も）では、リポジトリに残したファイル（dist/、
# installer/build/）を後で利用者に返す。Windows のドライブでは持ち主は関係ない。
owner=''
if [ "$(uname -s)" = Linux ] && [ "$(id -u)" != 0 ]; then
  owner=$(id -u):$(id -g)
fi
# Git Bash は /src を Windows のパスに変えてしまう。
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
