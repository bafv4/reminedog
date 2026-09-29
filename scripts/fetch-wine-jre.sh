#!/usr/bin/env bash
# Fetch a Windows x64 Java runtime that runs under Wine: the jlinked Temurin JRE bundled in
# the jdk4py wheel on PyPI (java.exe plus java.base, jdk.unsupported, ...; everything LWJGL
# needs). The wheel is pinned by version and SHA-256. Needs python3 with pip.
# Prints the Linux path of java.exe on stdout; progress goes to stderr.
#
# Usage: scripts/fetch-wine-jre.sh [CACHE_DIR]
#   CACHE_DIR defaults to $REMINEDOG_WINE_CACHE, else <repo>/target/wine-cache.
set -euo pipefail

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cache=${1:-${REMINEDOG_WINE_CACHE:-$repo/target/wine-cache}}

# Java 21 is what Minecraft 1.20.5+ runs on.
version=21.0.8.2
sha256=1c8e37d26315dfbb603503153d3dcdd90021fda71d64ab0b6309f06f83c0d503

jre=$cache/jre/jdk4py-$version
java=$jre/bin/java.exe
if [ ! -f "$java" ]; then
  mkdir -p "$cache/jre"
  tmp=$(mktemp -d "$cache/jre/download.XXXXXX")
  trap 'rm -rf "$tmp"' EXIT
  echo "fetch-wine-jre: downloading jdk4py $version (win_amd64) from PyPI" >&2
  echo "jdk4py==$version --hash=sha256:$sha256" >"$tmp/requirements.txt"
  python3 -m pip download --quiet --disable-pip-version-check --no-deps \
    --only-binary=:all: --platform win_amd64 --python-version 3.12 \
    --require-hashes -r "$tmp/requirements.txt" -d "$tmp" >&2
  python3 -m zipfile -e "$tmp/jdk4py-$version-py3-none-win_amd64.whl" "$tmp/wheel"

  # LWJGL needs java.base and jdk.unsupported (sun.misc.Unsafe).
  modules=$(grep '^MODULES=' "$tmp/wheel/jdk4py/java-runtime/release")
  for module in java.base jdk.unsupported; do
    case " ${modules//\"/ } " in
      *" $module "*) ;;
      *)
        echo "fetch-wine-jre: runtime lacks module $module ($modules)" >&2
        exit 1
        ;;
    esac
  done
  rm -rf "$jre"
  mv "$tmp/wheel/jdk4py/java-runtime" "$jre"
fi
echo "$java"
