#!/usr/bin/env bash
# Wine＋Xvfb（GLX を通した Mesa の llvmpipe）の Windows の JVM で LWJGL のスモークのプログラム
# （ci/smoke/Smoke.java）を動かす。必要ならエージェントの DLL を -agentpath: で読み込む。
#
# 使い方：scripts/wine-smoke.sh [--agent DLL] [--agent-options STR] [--lwjgl VERSION]
#                              [--frames N] [--legacy] [--readback] [--natives MODE]
#                              [--jvm-arg ARG]... [--screenshot PNG] [--sdl] [--keep]
#                              [--seconds N] [--capture] [--mc] [--xdotool CMDS]
#                              [--xdotool-delay S] [--grab PNG]
#   --agent DLL          DLL を -agentpath: で読み込む（reminedog.dll）
#   --agent-options STR  Agent_OnLoad に渡すオプションの文字列（-agentpath:DLL=STR）
#   --lwjgl VERSION      LWJGL の版（既定は 3.3.3。3.2.2 = Minecraft 1.14〜1.18）
#   --natives MODE       ランチャーによって違う、LWJGL が glfw.dll などを読み込む場所：
#                          extract       クラスパスのネイティブの jar を <gamedir>/natives に
#                                        展開する（公式ランチャーの 1.19 以降。既定）
#                          temp          クラスパスのネイティブの jar を、LWJGL の既定の
#                                        %TEMP%\lwjgl_<user>\... のフォルダに展開する
#                          library-path  DLL を前もって <gamedir>/natives に展開し、
#                                        -Djava.library.path で見つける（公式の 1.18 以前、Prism、MultiMC）
#   --frames N           描くフレームの数（既定は 120）
#   --legacy             3.2 core の代わりに GLFW の既定のウィンドウのヒント（Minecraft 1.13〜1.16）
#   --readback           最後のフレームの後に読み戻したピクセルを出す
#   --jvm-arg ARG        追加の JVM 引数。例：-Dorg.lwjgl.util.Debug=true（繰り返せる）
#   --screenshot PNG     最後のフレーム（スワップの後のバックバッファ）を PNG に保存する
#   --sdl                GLFW のものの代わりに ci/smoke/SmokeSdl.java（Minecraft 26.x と同じく
#                        SDL3 のウィンドウ）を動かす。--lwjgl 3.4.0 以降が要る
#   --seconds N          --frames の代わりに、テスト用プログラムを N 秒（約 60 fps）動かす
#   --capture            ゲーム中の Minecraft と同じくカーソルを捕まえる
#   --mc                 Minecraft と同じく描く：ウィンドウシステムが報告する大きさで自前の
#                        フレームバッファに描き、ウィンドウに写す
#   --xdotool CMDS       ウィンドウが出たら（--xdotool-delay 秒後。既定は 4）、そこにフォーカスを
#                        移して "xdotool CMDS" を動かす。例："key ctrl+i sleep 0.5 type abc"
#   --grab PNG           xdotool のコマンドの後、X の画面が見せているものを保存する（"keydown" で
#                        押したキーはまだ押したまま）。python3 の Pillow が要る
#   --keep               実行のフォルダ（ゲームのフォルダ、出力）を消さずに残す
# 環境変数：
#   REMINEDOG_WINE_CACHE  JRE、LWJGL、クラス、WINEPREFIX のキャッシュ
#                         （既定は <repo>/target/wine-cache）
#   SMOKE_TIMEOUT         JVM を止めるまでの秒数（既定は 300）
#   WINEDEBUG             Wine のデバッグのチャンネル（既定は -all）
# 終了コード：スモークのプログラムのもの（0 = "SMOKE OK"）、時間切れなら 124、使い方の誤りなら 2。
set -euo pipefail

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
self=$repo/scripts/wine-smoke.sh

# Wine と話すものはすべて 1 つの xvfb-run のセッション（--wine-session でこのスクリプトを動かし
# 直したもの）の中で動かす：$DISPLAY なしで起動した wineserver（winepath によるものでも）は
# ドライバのないデスクトップを残し、GLFW が補助のウィンドウを作れなくなる。
wine_session() {
  # shellcheck source=/dev/null
  source "$1"
  if [ ! -f "$WINEPREFIX/system.reg" ]; then
    echo "wine-smoke: initializing WINEPREFIX $WINEPREFIX" >&2
    # 64 ビットだけ：wineboot は syswow64 の半分がないと文句を言うが、害はない。
    wineboot --init >/dev/null 2>&1 || true
    wineserver -w
    [ -f "$WINEPREFIX/system.reg" ] || { echo "wine-smoke: wineboot failed" >&2; return 1; }
  fi

  local java_win cp_win path agent_arg
  java_win=$(winepath -w "$java_exe")
  cp_win=
  while IFS= read -r path; do
    cp_win+="${cp_win:+;}$path"
  done < <(winepath -w "${classpath[@]}")
  local args=()
  if [ -n "$agent" ]; then
    agent_arg="-agentpath:$(winepath -w "$agent")"
    [ -n "$agent_options" ] && agent_arg+="=$agent_options"
    args+=("$agent_arg")
  fi
  # HotSpot は CPU を数えるたびに Wine にプロセスグループのアフィニティ（スタブ）を聞き、毎回、
  # プログラムの行の途中にでも警告を出す。数を決めておけば問い合わせを省く。
  args+=("-XX:ActiveProcessorCount=$(nproc)")
  case $natives in
    extract) args+=("-Dorg.lwjgl.system.SharedLibraryExtractPath=$(winepath -w "$gamedir/natives")") ;;
    library-path) args+=("-Djava.library.path=$(winepath -w "$gamedir/natives")") ;;
    *) ;;
  esac
  args+=("${jvm_args[@]}" -cp "$cp_win" "$main_class" "${smoke_args[@]}")

  # ホストの JVM の設定（プロキシのトラストストアなど）を Windows の JVM に渡さない。
  unset JAVA_TOOL_OPTIONS JDK_JAVA_OPTIONS _JAVA_OPTIONS
  local status=0
  cd "$gamedir"
  echo "wine-smoke: wine $java_win ${args[*]}" >"$output"
  if [ -n "$xdotool_cmds" ]; then
    # テスト用プログラムのウィンドウが出たら入力を真似る（xdotool はこの Xvfb と話す）。
    (
      sleep "$xdotool_delay"
      xdotool search --sync --name "reminedog smoke" windowfocus --sync >/dev/null 2>&1 || true
      local cmds
      read -ra cmds <<<"$xdotool_cmds"
      echo "wine-smoke: xdotool ${cmds[*]}" >>"$output"
      xdotool "${cmds[@]}" >>"$output" 2>&1 || echo "wine-smoke: xdotool failed" >>"$output"
      if [ -n "$grab" ]; then
        # X サーバーが今見せているもの（キーはまだ押したままかもしれない）。
        python3 -c 'import sys; from PIL import ImageGrab; ImageGrab.grab(xdisplay=None).save(sys.argv[1])' \
          "$grab" >>"$output" 2>&1 || echo "wine-smoke: grab failed (needs python3-pil)" >>"$output"
      fi
    ) &
  fi
  timeout -k 10 "$timeout" wine "$java_win" "${args[@]}" >>"$output" 2>&1 || status=$?
  if [ "$status" -eq 124 ]; then
    echo "wine-smoke: timed out after ${timeout}s" >>"$output"
    wineserver -k || true
  fi
  wineserver -w
  return "$status"
}

if [ "${1:-}" = --wine-session ]; then
  wine_session "$2"
  exit
fi

usage() {
  sed -n '2,/^set -euo/{/^set -euo/d;s/^# \{0,1\}//;p}' "$self" >&2
  exit 2
}

agent='' agent_options='' lwjgl=3.3.3 frames=120 natives=extract keep=0 screenshot=''
main_class=Smoke extra_modules=''
xdotool_cmds='' xdotool_delay=4 grab=
smoke_flags=() jvm_args=()
while [ "$#" -gt 0 ]; do
  case $1 in
    --agent | --agent-options | --lwjgl | --frames | --natives | --jvm-arg | --screenshot | \
      --seconds | --xdotool | --xdotool-delay | --grab)
      [ "$#" -ge 2 ] || usage
      case $1 in
        --agent) agent=$2 ;;
        --agent-options) agent_options=$2 ;;
        --lwjgl) lwjgl=$2 ;;
        --frames) frames=$2 ;;
        --natives) natives=$2 ;;
        --jvm-arg) jvm_args+=("$2") ;;
        --screenshot) screenshot=$2 ;;
        --seconds) smoke_flags+=("--seconds=$2") ;;
        --xdotool) xdotool_cmds=$2 ;;
        --xdotool-delay) xdotool_delay=$2 ;;
        --grab)
          grab_dir=$(dirname "$2")
          if [ ! -d "$grab_dir" ]; then
            echo "wine-smoke: --grab: no folder $grab_dir" >&2
            exit 2
          fi
          grab=$(cd "$grab_dir" && pwd)/$(basename "$2")
          ;;
      esac
      shift 2
      ;;
    --legacy | --readback | --capture | --mc) smoke_flags+=("$1"); shift ;;
    --keep) keep=1; shift ;;
    --sdl) main_class=SmokeSdl extra_modules=lwjgl-sdl; shift ;;
    -h | --help) usage ;;
    *) echo "wine-smoke: unknown argument $1" >&2; usage ;;
  esac
done
case $frames in '' | *[!0-9]*) echo "wine-smoke: --frames needs a number" >&2; usage ;; esac
case $natives in extract | temp | library-path) ;; *) echo "wine-smoke: bad --natives" >&2; usage ;; esac
if [ -n "$agent" ]; then
  [ -f "$agent" ] || { echo "wine-smoke: agent not found: $agent" >&2; exit 2; }
  agent=$(cd "$(dirname "$agent")" && pwd)/$(basename "$agent")
fi
for tool in wine wineboot winepath wineserver xvfb-run javac python3 curl timeout flock; do
  command -v "$tool" >/dev/null || { echo "wine-smoke: $tool not found" >&2; exit 2; }
done
if [ -n "$xdotool_cmds" ]; then
  command -v xdotool >/dev/null || { echo "wine-smoke: xdotool not found" >&2; exit 2; }
fi

cache=${REMINEDOG_WINE_CACHE:-$repo/target/wine-cache}
mkdir -p "$cache"
cache=$(cd "$cache" && pwd)

# キャッシュごとに一度に 1 回だけ動かす：同時に動かすと、もう一方の Xvfb のディスプレイに
# デスクトップがある wineserver を共有してしまう。
exec 9>"$cache/.lock"
if ! flock -n 9; then
  echo "wine-smoke: waiting for another run using $cache" >&2
  flock 9
fi

java_exe=$("$repo/scripts/fetch-wine-jre.sh" "$cache")
jar_list=$(LWJGL_EXTRA_MODULES=$extra_modules "$repo/ci/smoke/fetch-lwjgl.sh" "$lwjgl" "$cache/lwjgl/$lwjgl")
mapfile -t jars <<<"$jar_list"

# ホストの JDK でコンパイルする。クラスファイルは Java 8 以降のどの JVM でも動く。
classes=$cache/smoke-classes/$lwjgl-$main_class
smoke_src=$repo/ci/smoke/$main_class.java
if [ ! -f "$classes/$main_class.class" ] || [ "$smoke_src" -nt "$classes/$main_class.class" ] ||
  [ "$repo/ci/smoke/SmokeImage.java" -nt "$classes/$main_class.class" ]; then
  echo "wine-smoke: compiling $main_class.java against LWJGL $lwjgl" >&2
  rm -rf "$classes"
  mkdir -p "$classes"
  javac --release 8 -Xlint:-options -d "$classes" -cp "$(IFS=:; echo "${jars[*]}")" "$smoke_src" \
    "$repo/ci/smoke/SmokeImage.java" 2>&1 |
    grep -v '^Picked up JAVA_TOOL_OPTIONS' >&2 || true
  [ -f "$classes/$main_class.class" ] || { echo "wine-smoke: javac failed" >&2; exit 1; }
fi

run=$(mktemp -d "$cache/run.XXXXXX")
if [ "$keep" -eq 0 ]; then
  trap 'rm -rf "$run"' EXIT
fi
gamedir=$run/game
output=$run/output.txt
mkdir -p "$gamedir"

classpath=("$classes")
natives_jars=()
for jar in "${jars[@]}"; do
  case $jar in
    *-natives-windows.jar) natives_jars+=("$jar") ;;
    *) classpath+=("$jar") ;;
  esac
done
if [ "$natives" = library-path ]; then
  # ランチャーと同じく、64 ビットの DLL を平らに展開する（3.2.x の jar は *32.dll も持つ）。
  python3 - "$gamedir/natives" "${natives_jars[@]}" <<'EOF'
import os, sys, zipfile
dest = sys.argv[1]
os.makedirs(dest, exist_ok=True)
for jar in sys.argv[2:]:
    with zipfile.ZipFile(jar) as z:
        for name in z.namelist():
            base = os.path.basename(name)
            if base.endswith(".dll") and not base.endswith("32.dll"):
                with open(os.path.join(dest, base), "wb") as f:
                    f.write(z.read(name))
EOF
else
  classpath+=("${natives_jars[@]}")
fi

# 設定を、source するファイル（配列も含む）で --wine-session の側に渡す。
[ -n "$screenshot" ] && smoke_flags+=(--screenshot)
smoke_args=("$frames" "${smoke_flags[@]}")
timeout=${SMOKE_TIMEOUT:-300}
export WINEPREFIX=$cache/prefix WINEARCH=win64 WINEDEBUG=${WINEDEBUG:--all}
# Mono・Gecko のインストールの確認を出さず、ホストにメニューの項目を作らない。
export WINEDLLOVERRIDES="mscoree,mshtml=;winemenubuilder.exe=d${WINEDLLOVERRIDES:+;$WINEDLLOVERRIDES}"
declare -p java_exe classpath agent agent_options natives jvm_args main_class smoke_args gamedir output timeout \
  xdotool_cmds xdotool_delay grab \
  >"$run/session.sh"

start=$(date +%s)
status=0
# （ロックの fd 9 は閉じる。残った Wine のプロセスがそれを持ち続けないように。）
xvfb-run -a -s "-screen 0 1280x1024x24 +extension GLX" "$BASH" "$self" --wine-session "$run/session.sh" \
  9>&- || status=$?
echo "=== smoke output (LWJGL $lwjgl, exit $status, $(($(date +%s) - start))s) ==="
# Windows の JVM は行末に CRLF を書く。
tr -d '\r' <"$output" 2>/dev/null || echo "(no output)"
log=$gamedir/reminedog/reminedog.log
if [ -f "$log" ]; then
  echo "=== $log ==="
  cat "$log"
fi
# JVM のクラッシュ（エージェントの中など）は、作業フォルダに hs_err_pid*.log を残す。
for crash in "$gamedir"/hs_err_pid*.log; do
  [ -f "$crash" ] || continue
  echo "=== $crash (first 60 lines) ==="
  head -n 60 "$crash" | tr -d '\r'
done
if [ -n "$screenshot" ] && [ -f "$gamedir/smoke-screenshot.png" ]; then
  cp "$gamedir/smoke-screenshot.png" "$screenshot"
  echo "wine-smoke: screenshot saved to $screenshot" >&2
fi
[ "$keep" -eq 1 ] && echo "wine-smoke: kept $run" >&2
exit "$status"
