#!/usr/bin/env bash
# On macOS (a GitHub Actions runner or a Mac): run the LWJGL smoke harness (ci/smoke) in a
# window of the logged-in session, optionally with an agent library loaded via -agentpath:.
# GLFW and SDL3 want the main thread on macOS, so Java starts with -XstartOnFirstThread, as
# Minecraft's launchers do.
#
# Usage: smoke.sh [--agent DYLIB] [--agent-options STR] [--lwjgl VERSION] [--sdl]
#                 [--grab SECONDS] [-- ARGS...]
#   --agent DYLIB        load DYLIB with -agentpath:
#   --agent-options STR  options passed to Agent_OnLoad (-agentpath:DYLIB=STR)
#   --lwjgl VERSION      LWJGL version (default 3.3.3; Apple silicon needs 3.3.0 or later, --sdl
#                        3.4.0 or later). GLFW needs a GPU: it asks for an accelerated pixel
#                        format, which a virtual machine with only the software renderer lacks
#   --sdl                run SmokeSdl (an SDL3 window, like Minecraft 26.x) instead of Smoke
#   --grab SECONDS       that long after start, save the screen as screen.png in the run
#                        directory (screencapture; the overlay is only checked this way)
#   ARGS                 the harness's own arguments (default: --seconds=5 --mc)
# The run directory (game dir, output, logs) is /tmp/smoke-run, printed at the end.
# Environment: SMOKE_SRC (the repository, default: this script's), SMOKE_CACHE (jars and
# classes, default /tmp/smoke-cache), SMOKE_RUN (the run directory), SMOKE_TIMEOUT (seconds,
# 120).
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

# macOS has no timeout(1): a watchdog ends Java if it hangs. Its output goes elsewhere, so its
# sleep does not hold this script's stdout (a pipe to tee) open after it is ended.
java "${java_args[@]}" -cp "$cp:$classes" "$harness" "${args[@]}" >smoke.txt 2>&1 &
java=$!
(sleep "${SMOKE_TIMEOUT:-120}" && kill "$java" && echo "smoke.sh: timed out" >&2) >/dev/null 2>&1 &
watchdog=$!
if [ -n "$grab" ]; then
  sleep "$grab"
  screencapture -x "$run/screen.png" || echo "smoke.sh: screencapture failed ($?)" >&2
fi
status=0
wait "$java" || status=$?
{ pkill -P "$watchdog" sleep; kill "$watchdog" && wait "$watchdog"; } 2>/dev/null || true
cat smoke.txt
echo "smoke.sh: exit $status, run directory $run"
exit "$status"
