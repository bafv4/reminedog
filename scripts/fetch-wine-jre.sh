#!/usr/bin/env bash
# Wine で動く Windows x64 の Java のランタイムを取る：PyPI の jdk4py の wheel に入っている、
# jlink した Temurin の JRE（java.exe と java.base、jdk.unsupported、...。LWJGL が要るものはすべて）。
# wheel は版と SHA-256 で固定する。pip の使える python3 が要る。
# java.exe の Linux のパスを標準出力に出す。進み具合は標準エラーへ。
#
# 使い方：scripts/fetch-wine-jre.sh [CACHE_DIR]
#   CACHE_DIR の既定は $REMINEDOG_WINE_CACHE、なければ <repo>/target/wine-cache。
set -euo pipefail

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cache=${1:-${REMINEDOG_WINE_CACHE:-$repo/target/wine-cache}}

# Java 21 は Minecraft 1.20.5 以降が動くもの。
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

  # LWJGL には java.base と jdk.unsupported（sun.misc.Unsafe）が要る。
  modules=$(grep '^MODULES=' "$tmp/wheel/jdk4py/java-runtime/release") || {
    echo "fetch-wine-jre: the runtime's release file lists no modules" >&2
    exit 1
  }
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
