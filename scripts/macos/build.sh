#!/usr/bin/env bash
# On a Mac: builds the macOS agent (libreminedog_macos.dylib, a prototype) in release, as one
# library for both Apple silicon and Intel (lipo), optionally with the installer's jar, into
# dist/macos. Needs Rust with both targets (rustup target add aarch64-apple-darwin
# x86_64-apple-darwin) and the Xcode command line tools; the installer needs a JDK 17 or later.
#
# Usage: build.sh [--check] [--installer] [--native] [--out DIR]
#   --check      first fmt (check only), clippy (warnings are errors) and the tests, as the CI
#   --installer  also build the installer (its tests and reminedog-installer.jar)
#   --native     only for this Mac's architecture (no lipo, no second target needed)
#   --out DIR    where the files go (default: dist/macos in the repository)
# REMINEDOG_VERSION, when set, is the version in the library and the jar (as the release
# workflow sets it); the library keeps its plain name, the jar is reminedog-installer-<version>.jar.
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
# Relative to where the script was run, not the repository it changes to.
case $out in /*) ;; *) out=$PWD/$out ;; esac

cd "$repo"
# Cargo's output folder, wherever its settings put it (CARGO_TARGET_DIR, build.target-dir).
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
# The JVM looks the entry point up by name.
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
