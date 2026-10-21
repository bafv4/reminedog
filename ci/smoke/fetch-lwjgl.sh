#!/usr/bin/env bash
# スモークのテスト用プログラムが要る LWJGL 3 の jar（core、GLFW、OpenGL とそれらのネイティブの
# jar：natives-windows、または LWJGL_NATIVES=natives-linux か natives-macos-arm64）を Maven Central
# から DEST_DIR にダウンロードし、それぞれを ci/smoke/lwjgl.sha256 の SHA-256 と比べる（そこに
# ない jar は拒む）。もうある jar は確かめ直して残すので、もう一度動かしても軽い。
# すべての jar のパスを標準出力に出す（進み具合は標準エラーへ）。
# bash、curl、sha256sum（macOS では shasum）だけが要るので、Windows のランナーの Git Bash でも
# 動く。
#
# 使い方：ci/smoke/fetch-lwjgl.sh VERSION DEST_DIR
set -euo pipefail

if [ "$#" -ne 2 ]; then
  echo "usage: $0 VERSION DEST_DIR" >&2
  exit 2
fi
version=$1
dest=$2
base=${LWJGL_MAVEN_BASE:-https://repo1.maven.org/maven2/org/lwjgl}

# URL を FILE にダウンロードする。Maven Central は集中した要求に HTTP 429 で答え、同じ接続では
# 何分も答え続けることがある（なので curl 自身の --retry は役に立たない）。429・5xx、ネットワークの
# エラー、途中で切れた転送（状態が 200 でも curl が失敗する）のときは、待ちを伸ばしながら新しい
# curl のプロセスでやり直す。
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
  for classifier in "" "-${LWJGL_NATIVES:-natives-windows}"; do
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
    if command -v sha256sum >/dev/null; then
      got=$(sha256sum "$out" | cut -d ' ' -f 1)
    else
      got=$(shasum -a 256 "$out" | cut -d ' ' -f 1)
    fi
    if [ "$want" != "$got" ]; then
      rm -f "$out"
      echo "fetch-lwjgl: SHA-256 mismatch for $name (expected '$want', got '$got')" >&2
      exit 1
    fi
    echo "$out"
  done
done
