#!/usr/bin/env bash
# Download the LWJGL 3 jars the smoke harness needs (core, GLFW, OpenGL and their
# natives-windows jars) from Maven Central into DEST_DIR, verifying each against the SHA-256
# in ci/smoke/lwjgl.sha256 (a jar without one there is refused). Already-present jars are
# checked again and kept, so re-running is cheap.
# Prints the path of every jar on stdout (progress goes to stderr).
# Needs only bash, curl and sha256sum, so it also runs in Git Bash on Windows runners.
#
# Usage: ci/smoke/fetch-lwjgl.sh VERSION DEST_DIR
set -euo pipefail

if [ "$#" -ne 2 ]; then
  echo "usage: $0 VERSION DEST_DIR" >&2
  exit 2
fi
version=$1
dest=$2
base=${LWJGL_MAVEN_BASE:-https://repo1.maven.org/maven2/org/lwjgl}

# Download URL to FILE. Maven Central answers bursts with HTTP 429 and may keep doing so
# for minutes on the same connection (so curl's own --retry does not help); on 429/5xx, a
# network error or a transfer cut short (curl fails even though the status was 200), try
# again in a fresh curl process after a growing pause.
fetch() {
  local url=$1 file=$2 attempt code rc
  for attempt in 1 2 3 4 5; do
    rc=0
    code=$(curl -sSL --retry 3 -o "$file" -w '%{http_code}' "$url") || rc=$?
    if [ "$rc" -eq 0 ] && [ "$code" = 200 ]; then
      return 0
    fi
    case $code in
      200 | 429 | 5?? | 000) ;;
      *) break ;;
    esac
    [ "$attempt" -lt 5 ] || break
    echo "fetch-lwjgl: HTTP $code (curl exit $rc), retrying in $((attempt * 5))s" >&2
    sleep $((attempt * 5))
  done
  rm -f "$file"
  echo "fetch-lwjgl: download failed (HTTP $code, curl exit $rc): $url" >&2
  return 1
}

hashes=$(dirname "${BASH_SOURCE[0]}")/lwjgl.sha256

mkdir -p "$dest"
for module in lwjgl lwjgl-glfw lwjgl-opengl ${LWJGL_EXTRA_MODULES:-}; do
  for classifier in "" "-natives-windows"; do
    name="$module-$version$classifier.jar"
    out="$dest/$name"
    want=$(tr -d '\r' <"$hashes" | awk -v name="$name" '$2 == name { print $1 }')
    if [ -z "$want" ]; then
      echo "fetch-lwjgl: no SHA-256 for $name in $hashes" >&2
      exit 1
    fi
    if [ ! -s "$out" ]; then
      url="$base/$module/$version/$name"
      echo "fetch-lwjgl: $url" >&2
      fetch "$url" "$out.part"
      mv -f "$out.part" "$out"
    fi
    got=$(sha256sum "$out" | cut -d ' ' -f 1)
    if [ "$want" != "$got" ]; then
      rm -f "$out"
      echo "fetch-lwjgl: SHA-256 mismatch for $name (expected '$want', got '$got')" >&2
      exit 1
    fi
    echo "$out"
  done
done
