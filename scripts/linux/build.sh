#!/usr/bin/env bash
# Linux のエージェント（libreminedog_linux.so）をリリースでビルドし、必要ならインストーラーの
# jar と一緒に dist/linux に出す。Rust のある Linux（インストーラーには JDK 17 以降も）で動かすか、
# Windows から開発用のコンテナで動かす：
#   scripts/linux/run.sh scripts/linux/build.sh [OPTIONS]
#
# 使い方：build.sh [--check] [--installer] [--out DIR]
#   --check      CI と同じく、先に fmt（確認だけ）、clippy（警告はエラー）、テスト
#   --installer  インストーラーもビルドする（そのテストと reminedog-installer.jar）
#   --out DIR    ファイルの出力先（既定：リポジトリの dist/linux）
# REMINEDOG_VERSION を設定すれば、それがライブラリと jar の版になる（リリースのワークフローと
# 同じ）。ライブラリの名前はそのまま、jar は reminedog-installer-<version>.jar。
set -euo pipefail

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
check='' installer='' out=$repo/dist/linux
while [ "$#" -gt 0 ]; do
  case $1 in
    --check) check=1; shift ;;
    --installer) installer=1; shift ;;
    --out) out=$2; shift 2 ;;
    *) echo "build.sh: unknown argument $1" >&2; exit 2 ;;
  esac
done
# 移った先のリポジトリではなく、スクリプトを動かした場所からの相対。
case $out in /*) ;; *) out=$PWD/$out ;; esac

cd "$repo"
# Cargo の出力のフォルダ（設定がどこに置いていても。CARGO_TARGET_DIR、build.target-dir）。
target=$(cargo metadata --format-version 1 --no-deps | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')
[ -n "$target" ] || { echo "build.sh: cargo metadata gave no target directory" >&2; exit 1; }
if [ -n "$check" ]; then
  echo "== fmt" && cargo fmt --all --check
  echo "== clippy" && cargo clippy --locked --workspace --all-targets -- -D warnings
  echo "== test" && cargo test --locked --workspace
fi

echo "== build libreminedog_linux.so"
cargo build --locked --release -p reminedog-hook-linux
lib=$target/release/libreminedog_linux.so
# JVM は入口を名前で引く。
if command -v nm >/dev/null && ! nm -D --defined-only "$lib" | grep -qw Agent_OnLoad; then
  echo "build.sh: $lib does not export Agent_OnLoad" >&2
  exit 1
fi
files=("$lib")

if [ -n "$installer" ]; then
  echo "== build the installer"
  gradle_args=(build --no-daemon)
  jar=reminedog-installer.jar
  if [ -n "${REMINEDOG_VERSION:-}" ]; then
    gradle_args+=("-PreminedogVersion=$REMINEDOG_VERSION")
    jar=reminedog-installer-$REMINEDOG_VERSION.jar
  fi
  (cd installer && sh ./gradlew "${gradle_args[@]}")
  files+=("$repo/installer/build/libs/$jar")
fi

mkdir -p "$out"
cp -f "${files[@]}" "$out/"
echo "== done"
for file in "${files[@]}"; do
  ls -l "$out/$(basename "$file")"
done
