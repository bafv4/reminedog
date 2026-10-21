#!/usr/bin/env bash
# macOS（GitHub Actions のランナーか Mac）で：ログインしているセッションのウィンドウで LWJGL の
# スモークのテスト用プログラム（ci/smoke）を動かす。必要ならエージェントのライブラリを
# -agentpath: で読み込む。macOS の GLFW と SDL3 はメインスレッドを求めるので、Minecraft の
# ランチャーと同じく Java を -XstartOnFirstThread で起動する。
#
# 使い方：smoke.sh [--agent DYLIB] [--agent-options STR] [--lwjgl VERSION] [--sdl]
#                 [--grab SECONDS] [-- ARGS...]
#   --agent DYLIB        DYLIB を -agentpath: で読み込む
#   --agent-options STR  Agent_OnLoad に渡すオプション（-agentpath:DYLIB=STR）
#   --lwjgl VERSION      LWJGL の版（3.3.3 か 3.4.3。既定は 3.3.3。--sdl には 3.4.3 が要る）。GLFW
#                        には GPU が要る：Accelerated のピクセルフォーマットを求めるが、ソフトウェア
#                        レンダラーしかない仮想マシンにはない
#   --sdl                Smoke の代わりに SmokeSdl（Minecraft 26.x と同じく SDL3 のウィンドウ）を動かす
#   --grab SECONDS       起動からその秒数の後、画面を実行のフォルダに screen.png として保存する
#                        （screencapture。オーバーレイはこれでしか確かめない）
#   ARGS                 テスト用プログラム自身の引数（既定：--seconds=5 --mc）
# 実行のフォルダ（ゲームのフォルダ、出力、ログ）は /tmp/smoke-run。最後に出す。
# 環境変数：SMOKE_SRC（リポジトリ。既定はこのスクリプトのもの）、SMOKE_CACHE（jar とクラス。
# 既定は /tmp/smoke-cache）、SMOKE_RUN（実行のフォルダ）、SMOKE_TIMEOUT（秒。120）。
set -euo pipefail

agent='' agent_options='' lwjgl=3.3.3 harness=Smoke grab=''
args=()
while [ "$#" -gt 0 ]; do
  case $1 in
    --agent) agent=$2; shift 2 ;;
    --agent-options) agent_options=$2; shift 2 ;;
    --lwjgl) lwjgl=$2; shift 2 ;;
    --sdl) harness=SmokeSdl; shift ;;
    --grab) grab=$2; shift 2 ;;
    --) shift; args=("$@"); break ;;
    *) echo "smoke.sh: unknown argument $1" >&2; exit 2 ;;
  esac
done
[ "${#args[@]}" -gt 0 ] || args=(--seconds=5 --mc)

src=${SMOKE_SRC:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}
cache=${SMOKE_CACHE:-/tmp/smoke-cache}
case $(uname -m) in
  arm64) natives=natives-macos-arm64 ;;
  *) natives=natives-macos ;;
esac
modules=''
[ "$harness" = SmokeSdl ] && modules=lwjgl-sdl
jars=$(LWJGL_NATIVES=$natives LWJGL_EXTRA_MODULES="$modules" \
  bash "$src/ci/smoke/fetch-lwjgl.sh" "$lwjgl" "$cache/lwjgl/$lwjgl")
cp=$(echo "$jars" | paste -sd: -)

classes=$cache/classes-$lwjgl-$harness
rm -rf "$classes" && mkdir -p "$classes"
javac --release 8 -nowarn -cp "$cp" -d "$classes" "$src/ci/smoke/$harness.java" "$src/ci/smoke/SmokeImage.java"

run=${SMOKE_RUN:-/tmp/smoke-run}
rm -rf "$run" && mkdir -p "$run/natives" && cd "$run"
java_args=(-XstartOnFirstThread "-Dorg.lwjgl.system.SharedLibraryExtractPath=$run/natives")
if [ -n "$agent" ]; then
  java_args+=("-agentpath:$agent${agent_options:+=$agent_options}")
fi

# macOS には timeout(1) がない：見張りが、止まった Java を終わらせ（TERM、Java が終わらなければ
# KILL）、そう書いたファイルを残す。見張りの出力は別のところへ送るので、見張りを終わらせた後に
# その sleep がこのスクリプトの標準出力（tee へのパイプ）を開いたままにすることはない。
java "${java_args[@]}" -cp "$cp:$classes" "$harness" "${args[@]}" >smoke.txt 2>&1 &
java=$!
(
  sleep "${SMOKE_TIMEOUT:-120}" &&
    touch "$run/timed-out" &&
    kill "$java" && sleep 10 && kill -9 "$java"
) >/dev/null 2>&1 &
watchdog=$!
if [ -n "$grab" ]; then
  sleep "$grab"
  screencapture -x "$run/screen.png" || echo "smoke.sh: screencapture failed ($?)" >&2
fi
status=0
wait "$java" || status=$?
# 先に見張り（それ以上進まないように）、それからその sleep。
{ kill "$watchdog"; pkill -P "$watchdog" sleep; wait "$watchdog"; } 2>/dev/null || true
cat smoke.txt
if [ -e "$run/timed-out" ]; then
  echo "smoke.sh: timed out after ${SMOKE_TIMEOUT:-120}s" >&2
fi
echo "smoke.sh: exit $status, run directory $run"
exit "$status"
