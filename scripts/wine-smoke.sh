#!/usr/bin/env bash
# Run the LWJGL smoke program (ci/smoke/Smoke.java) on a Windows JVM under Wine + Xvfb
# (Mesa llvmpipe through GLX), optionally with an agent DLL loaded via -agentpath:.
#
# Usage: scripts/wine-smoke.sh [--agent DLL] [--agent-options STR] [--lwjgl VERSION]
#                              [--frames N] [--legacy] [--readback] [--natives MODE]
#                              [--jvm-arg ARG]... [--screenshot PNG] [--sdl] [--keep]
#                              [--seconds N] [--capture] [--mc] [--xdotool CMDS]
#                              [--xdotool-delay S] [--grab PNG]
#   --agent DLL          load DLL with -agentpath: (reminedog.dll)
#   --agent-options STR  options string passed to Agent_OnLoad (-agentpath:DLL=STR)
#   --lwjgl VERSION      LWJGL version (default 3.3.3; 3.2.2 = Minecraft 1.14-1.18)
#   --natives MODE       where LWJGL loads glfw.dll etc. from, as different launchers do:
#                          extract       natives jars on the classpath, extracted to
#                                        <gamedir>/natives (vanilla launcher 1.19+; default)
#                          temp          natives jars on the classpath, extracted to LWJGL's
#                                        default %TEMP%\lwjgl_<user>\... folder
#                          library-path  DLLs pre-extracted to <gamedir>/natives and found via
#                                        -Djava.library.path (vanilla <= 1.18, Prism, MultiMC)
#   --frames N           frames to render (default 120)
#   --legacy             default GLFW window hints (Minecraft 1.13-1.16) instead of 3.2 core
#   --readback           print pixels read back after the last frame
#   --jvm-arg ARG        extra JVM argument, e.g. -Dorg.lwjgl.util.Debug=true (repeatable)
#   --screenshot PNG     save the last frame (back buffer after the swap) to PNG
#   --sdl                run ci/smoke/SmokeSdl.java (an SDL3 window, like Minecraft 26.x)
#                        instead of the GLFW one; needs --lwjgl 3.4.0 or later
#   --seconds N          run the harness for N seconds (~60 fps) instead of --frames
#   --capture            grab the cursor as Minecraft does in game
#   --mc                 render like Minecraft: into an own framebuffer at the size the
#                        window system reports, then copied into the window
#   --xdotool CMDS       once the window is up (after --xdotool-delay seconds, default 4),
#                        focus it and run "xdotool CMDS", e.g. "key ctrl+i sleep 0.5 type abc"
#   --grab PNG           after the xdotool commands, save what the X screen shows (keys held
#                        with "keydown" are still down); needs Pillow for python3
#   --keep               keep the run directory (game dir, output) instead of deleting it
# Environment:
#   REMINEDOG_WINE_CACHE  cache for the JRE, LWJGL, classes and WINEPREFIX
#                         (default <repo>/target/wine-cache)
#   SMOKE_TIMEOUT         seconds before the JVM is killed (default 300)
#   WINEDEBUG             Wine debug channels (default -all)
# Exit code: the smoke program's (0 = "SMOKE OK"), 124 on timeout, 2 on bad usage.
set -euo pipefail

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
self=$repo/scripts/wine-smoke.sh

# Everything that talks to Wine runs inside one xvfb-run session (this script re-executed
# with --wine-session): a wineserver started without $DISPLAY (even by winepath) leaves a
# driver-less desktop behind, and GLFW then fails to create its helper window.
wine_session() {
  # shellcheck source=/dev/null
  source "$1"
  if [ ! -f "$WINEPREFIX/system.reg" ]; then
    echo "wine-smoke: initializing WINEPREFIX $WINEPREFIX" >&2
    # 64-bit only: wineboot complains about the missing syswow64 half, which is harmless.
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
  # HotSpot asks Wine for the process group affinity (a stub) whenever it counts CPUs and
  # prints a warning each time, even in the middle of the program's lines; a fixed count
  # skips the query.
  args+=("-XX:ActiveProcessorCount=$(nproc)")
  case $natives in
    extract) args+=("-Dorg.lwjgl.system.SharedLibraryExtractPath=$(winepath -w "$gamedir/natives")") ;;
    library-path) args+=("-Djava.library.path=$(winepath -w "$gamedir/natives")") ;;
    *) ;;
  esac
  args+=("${jvm_args[@]}" -cp "$cp_win" "$main_class" "${smoke_args[@]}")

  # Keep the host's JVM settings (proxy trust store etc.) away from the Windows JVM.
  unset JAVA_TOOL_OPTIONS JDK_JAVA_OPTIONS _JAVA_OPTIONS
  local status=0
  cd "$gamedir"
  echo "wine-smoke: wine $java_win ${args[*]}" >"$output"
  if [ -n "$xdotool_cmds" ]; then
    # Simulated input once the harness window is up (xdotool talks to this Xvfb).
    (
      sleep "$xdotool_delay"
      xdotool search --sync --name "reminedog smoke" windowfocus --sync >/dev/null 2>&1 || true
      local cmds
      read -ra cmds <<<"$xdotool_cmds"
      echo "wine-smoke: xdotool ${cmds[*]}" >>"$output"
      xdotool "${cmds[@]}" >>"$output" 2>&1 || echo "wine-smoke: xdotool failed" >>"$output"
      if [ -n "$grab" ]; then
        # What the X server shows right now (keys may still be held down).
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
        --grab) grab=$(cd "$(dirname "$2")" && pwd)/$(basename "$2") ;;
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

# One run per cache at a time: concurrent runs would share a wineserver whose desktop lives
# on the other run's Xvfb display.
exec 9>"$cache/.lock"
if ! flock -n 9; then
  echo "wine-smoke: waiting for another run using $cache" >&2
  flock 9
fi

java_exe=$("$repo/scripts/fetch-wine-jre.sh" "$cache")
jar_list=$(LWJGL_EXTRA_MODULES=$extra_modules "$repo/ci/smoke/fetch-lwjgl.sh" "$lwjgl" "$cache/lwjgl/$lwjgl")
mapfile -t jars <<<"$jar_list"

# Compile with the host JDK; the class files run on any Java 8+ JVM.
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
  # Unpack the 64-bit DLLs flat, as launchers do (3.2.x jars also carry *32.dll).
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

# Hand the settings to the --wine-session half through a sourced file (arrays included).
[ -n "$screenshot" ] && smoke_flags+=(--screenshot)
smoke_args=("$frames" "${smoke_flags[@]}")
timeout=${SMOKE_TIMEOUT:-300}
export WINEPREFIX=$cache/prefix WINEARCH=win64 WINEDEBUG=${WINEDEBUG:--all}
# No Mono/Gecko install prompts, no menu entries on the host.
export WINEDLLOVERRIDES="mscoree,mshtml=;winemenubuilder.exe=d${WINEDLLOVERRIDES:+;$WINEDLLOVERRIDES}"
declare -p java_exe classpath agent agent_options natives jvm_args main_class smoke_args gamedir output timeout \
  xdotool_cmds xdotool_delay grab \
  >"$run/session.sh"

start=$(date +%s)
status=0
# (fd 9, the lock, is closed so that no lingering Wine process keeps holding it.)
xvfb-run -a -s "-screen 0 1280x1024x24 +extension GLX" "$BASH" "$self" --wine-session "$run/session.sh" \
  9>&- || status=$?
echo "=== smoke output (LWJGL $lwjgl, exit $status, $(($(date +%s) - start))s) ==="
# The Windows JVM writes CRLF line ends.
tr -d '\r' <"$output" 2>/dev/null || echo "(no output)"
log=$gamedir/reminedog/reminedog.log
if [ -f "$log" ]; then
  echo "=== $log ==="
  cat "$log"
fi
# A JVM crash (e.g. in the agent) leaves hs_err_pid*.log in the working directory.
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
