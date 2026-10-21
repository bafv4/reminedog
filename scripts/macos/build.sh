#!/usr/bin/env bash
# Mac で：macOS のエージェント（libreminedog_macos.dylib。試作）を、Apple silicon と Intel の両方の
# 1 つのライブラリ（lipo）としてリリースでビルドし、必要ならインストーラーの jar と一緒に
# dist/macos に出す。両方のターゲットのある Rust（rustup target add aarch64-apple-darwin
# x86_64-apple-darwin）と Xcode のコマンドラインツールが要る。インストーラーには JDK 17 以降が要る。
#
# 使い方：build.sh [--check] [--installer] [--native] [--out DIR]
#   --check      CI と同じく、先に fmt（確認だけ）、clippy（警告はエラー）、テスト
#   --installer  インストーラーもビルドする（そのテストと reminedog-installer.jar）
#   --native     この Mac のアーキテクチャだけ（lipo なし、2 つ目のターゲットは要らない）
#   --out DIR    ファイルの出力先（既定：リポジトリの dist/macos）
# REMINEDOG_VERSION を設定すれば、それがライブラリと jar の版になる（リリースのワークフローと
# 同じ）。ライブラリの名前はそのまま、jar は reminedog-installer-<version>.jar。
set -euo pipefail

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
check='' installer='' native='' out=$repo/dist/macos
while [ "$#" -gt 0 ]; do
  case $1 in
    --check) check=1; shift ;;
    --installer) installer=1; shift ;;
    --native) native=1; shift ;;
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
name=libreminedog_macos.dylib
if [ -n "$check" ]; then
  echo "== fmt" && cargo fmt --all --check
  echo "== clippy" && cargo clippy --locked --workspace --all-targets -- -D warnings
  echo "== test" && cargo test --locked --workspace
fi

lib=$target/release/$name
if [ -n "$native" ]; then
  echo "== build $name ($(uname -m))"
  cargo build --locked --release -p reminedog-hook-macos
else
  installed=$(rustup target list --installed)
  for triple in aarch64-apple-darwin x86_64-apple-darwin; do
    if ! grep -qx "$triple" <<<"$installed"; then
      echo "build.sh: the Rust target $triple is missing: rustup target add $triple (or use --native)" >&2
      exit 1
    fi
  done
  slices=()
  for triple in aarch64-apple-darwin x86_64-apple-darwin; do
    echo "== build $name ($triple)"
    cargo build --locked --release -p reminedog-hook-macos --target "$triple"
    slices+=("$target/$triple/release/$name")
  done
  lib=$target/universal-apple-darwin/release/$name
  mkdir -p "$(dirname "$lib")"
  lipo -create -output "$lib" "${slices[@]}"
  lipo -info "$lib"
fi
# JVM は入口を名前で引く。
if ! nm -gU "$lib" | grep -qw _Agent_OnLoad; then
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
