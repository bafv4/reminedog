#!/usr/bin/env bash
# Linux のコンテナ（scripts/linux/run.sh）の中で：Mesa の llvmpipe の Xvfb で LWJGL のスモークの
# テスト用プログラム（ci/smoke）を動かす。必要ならエージェントのライブラリを -agentpath: で読み込む。
#
# 使い方：smoke.sh [--agent SO] [--agent-options STR] [--lwjgl VERSION] [--sdl]
#                 [--xdotool CMDS] [--xdotool-delay S] [-- ARGS...]
#   --agent SO           SO を -agentpath: で読み込む
#   --agent-options STR  Agent_OnLoad に渡すオプション（-agentpath:SO=STR）
#   --lwjgl VERSION      LWJGL の版（既定は 3.3.3。--sdl には 3.4.0 以降が要る）
#   --sdl                Smoke の代わりに SmokeSdl（Minecraft 26.x と同じく SDL3 のウィンドウ）を動かす
#   --xdotool CMDS       ウィンドウが出たら（--xdotool-delay 秒後。既定は 3）、そこにフォーカスを
#                        移して "xdotool CMDS" を動かす。例："keydown z sleep 1 keyup z"
#   ARGS                 テスト用プログラム自身の引数（既定：--seconds=5 --mc）
# 実行のフォルダ（ゲームのフォルダ、出力、エージェントのログ）は /target/smoke-run。コンテナの
# ボリュームにあるので、コンテナが消えても残る。例えばこう読む：
#   scripts/linux/run.sh cat /target/smoke-run/reminedog/reminedog.log
# 環境変数：SMOKE_SRC（リポジトリ。既定は /src）、SMOKE_CACHE（jar とクラス。既定は
# /target/smoke-cache）、SMOKE_RUN（実行のフォルダ）、SMOKE_TIMEOUT（秒。120）。
set -euo pipefail

agent='' agent_options='' lwjgl=3.3.3 harness=Smoke xdotool='' delay=3
args=()
while [ "$#" -gt 0 ]; do
  case $1 in
    --agent) agent=$2; shift 2 ;;
    --agent-options) agent_options=$2; shift 2 ;;
    --lwjgl) lwjgl=$2; shift 2 ;;
    --sdl) harness=SmokeSdl; shift ;;
    --xdotool) xdotool=$2; shift 2 ;;
    --xdotool-delay) delay=$2; shift 2 ;;
    --) shift; args=("$@"); break ;;
    *) echo "smoke.sh: unknown argument $1" >&2; exit 2 ;;
  esac
done
[ "${#args[@]}" -gt 0 ] || args=(--seconds=5 --mc)

src=${SMOKE_SRC:-/src}
cache=${SMOKE_CACHE:-/target/smoke-cache}
modules=''
[ "$harness" = SmokeSdl ] && modules=lwjgl-sdl
jars=$(LWJGL_NATIVES=natives-linux LWJGL_EXTRA_MODULES="$modules" \
  bash "$src/ci/smoke/fetch-lwjgl.sh" "$lwjgl" "$cache/lwjgl/$lwjgl")
cp=$(echo "$jars" | paste -sd: -)

classes=$cache/classes-$lwjgl-$harness
rm -rf "$classes" && mkdir -p "$classes"
javac --release 8 -nowarn -cp "$cp" -d "$classes" "$src/ci/smoke/$harness.java" "$src/ci/smoke/SmokeImage.java"

run=${SMOKE_RUN:-/target/smoke-run}
rm -rf "$run" && mkdir -p "$run/natives" && cd "$run"
java_args=("-Dorg.lwjgl.system.SharedLibraryExtractPath=$run/natives")
if [ -n "$agent" ]; then
  java_args+=("-agentpath:$agent${agent_options:+=$agent_options}")
fi

# xdotool がウィンドウに届くように、自前の X サーバー。
export DISPLAY=:99
Xvfb "$DISPLAY" -screen 0 1280x800x24 -nolisten tcp >/dev/null 2>&1 &
xvfb=$!
trap 'kill "$xvfb" 2>/dev/null || true' EXIT
for _ in $(seq 50); do xdpyinfo >/dev/null 2>&1 && break; sleep 0.1; done

LIBGL_ALWAYS_SOFTWARE=1 timeout "${SMOKE_TIMEOUT:-120}" \
  java "${java_args[@]}" -cp "$cp:$classes" "$harness" "${args[@]}" >smoke.txt 2>&1 &
java=$!
if [ -n "$xdotool" ]; then
  sleep "$delay"
  # フォーカスを移すために、GLFW のウィンドウを名前で探す（ウィンドウマネージャがない）。SDL3 は
  # 自分でフォーカスを取り、ウィンドウの名前を _NET_WM_NAME にしか付けないので、xdotool からは
  # 見えない。
  window=$(xdotool search --onlyvisible --name 'reminedog smoke' 2>/dev/null | tail -n 1) || true
  if [ -n "$window" ]; then
    timeout 5 xdotool windowfocus "$window" || true
  fi
  sleep 0.3
  # shellcheck disable=SC2086 # コマンドはわざと単語に分ける
  timeout 60 xdotool $xdotool || echo "smoke.sh: xdotool failed ($?)" >&2
fi
status=0
wait "$java" || status=$?
cat smoke.txt
echo "smoke.sh: exit $status, run directory $run"
exit "$status"
