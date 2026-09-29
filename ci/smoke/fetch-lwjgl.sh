#!/usr/bin/env bash
# Download the LWJGL 3 jars the smoke harness needs (core, GLFW, OpenGL and their
# natives-windows jars) from Maven Central into DEST_DIR, verifying each against its
# published SHA-1. Already-present jars are kept, so re-running is cheap.
# Prints the path of every jar on stdout (progress goes to stderr).
# Needs only bash, curl and sha1sum, so it also runs in Git Bash on Windows runners.
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
# for minutes on the same connection (so curl's own --retry does not help); on 429/5xx or
# a network error, try again in a fresh curl process after a growing pause.
fetch() {
  local url=$1 file=$2 attempt code
  for attempt in 1 2 3 4 5; do
    code=$(curl -sSL --retry 3 -o "$file" -w '%{http_code}' "$url") || true
    case $code in
      200) return 0 ;;
      429 | 5?? | 000) ;;
      *) break ;;
    esac
    echo "fetch-lwjgl: HTTP $code, retrying in $((attempt * 5))s" >&2
    sleep $((attempt * 5))
  done
  rm -f "$file"
  echo "fetch-lwjgl: download failed (HTTP $code): $url" >&2
  return 1
}

mkdir -p "$dest"
for module in lwjgl lwjgl-glfw lwjgl-opengl; do
  for classifier in "" "-natives-windows"; do
    name="$module-$version$classifier.jar"
    out="$dest/$name"
    if [ ! -s "$out" ]; then
      url="$base/$module/$version/$name"
      echo "fetch-lwjgl: $url" >&2
      fetch "$url" "$out.part"
      fetch "$url.sha1" "$out.sha1"
      # Maven Central's .sha1 holds the bare hash (sometimes followed by a file name).
      want=$(tr -d '\r' <"$out.sha1" | cut -d ' ' -f 1)
      got=$(sha1sum "$out.part" | cut -d ' ' -f 1)
      rm -f "$out.sha1"
      if [ "$want" != "$got" ]; then
        rm -f "$out.part"
        echo "fetch-lwjgl: SHA-1 mismatch for $name (expected '$want', got '$got')" >&2
        exit 1
      fi
      mv -f "$out.part" "$out"
    fi
    echo "$out"
  done
done
