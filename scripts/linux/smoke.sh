#!/usr/bin/env bash
# Inside the Linux container (scripts/linux/run.sh): run the LWJGL smoke harness (ci/smoke) on
# Xvfb with Mesa llvmpipe, optionally with an agent library loaded via -agentpath:.
#
# Usage: smoke.sh [--agent SO] [--agent-options STR] [--lwjgl VERSION] [--sdl]
#                 [--xdotool CMDS] [--xdotool-delay S] [-- ARGS...]
#   --agent SO           load SO with -agentpath:
#   --agent-options STR  options passed to Agent_OnLoad (-agentpath:SO=STR)
#   --lwjgl VERSION      LWJGL version (default 3.3.3; --sdl needs 3.4.0 or later)
#   --sdl                run SmokeSdl (an SDL3 window, like Minecraft 26.x) instead of Smoke
#   --xdotool CMDS       once the window is up (after --xdotool-delay seconds, default 3),
#                        focus it and run "xdotool CMDS", e.g. "keydown z sleep 1 keyup z"
#   ARGS                 the harness's own arguments (default: --seconds=5 --mc)
# The run directory (game dir, output, the agent's log) is /target/smoke-run, on the
# container's volume so it outlives the container: read it with, for example,
#   scripts/linux/run.sh cat /target/smoke-run/reminedog/reminedog.log
# Environment: SMOKE_SRC (the repository, default /src), SMOKE_CACHE (jars and classes,
# default /target/smoke-cache), SMOKE_RUN (the run directory), SMOKE_TIMEOUT (seconds, 120).
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

# An X server of our own, so xdotool can reach the window.
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
  # GLFW's window by its name, to focus it (there is no window manager). SDL3 takes the focus
  # itself, and names its windows in _NET_WM_NAME only, which xdotool does not read.
  window=$(xdotool search --onlyvisible --name 'reminedog smoke' 2>/dev/null | tail -n 1) || true
  if [ -n "$window" ]; then
    timeout 5 xdotool windowfocus "$window" || true
  fi
  sleep 0.3
  # shellcheck disable=SC2086 # the commands are word-split on purpose
  timeout 60 xdotool $xdotool || echo "smoke.sh: xdotool failed ($?)" >&2
fi
status=0
wait "$java" || status=$?
cat smoke.txt
echo "smoke.sh: exit $status, run directory $run"
exit "$status"
