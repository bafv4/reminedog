#!/usr/bin/env bash
# Inside the Linux container (scripts/linux/run.sh): run the LWJGL smoke harness (ci/smoke) on
# Xvfb with Mesa llvmpipe, optionally with an agent library loaded via -agentpath:.
#
# Usage: smoke.sh [--agent SO] [--agent-options STR] [--lwjgl VERSION] [--sdl] [-- ARGS...]
#   --agent SO           load SO with -agentpath:
#   --agent-options STR  options passed to Agent_OnLoad (-agentpath:SO=STR)
#   --lwjgl VERSION      LWJGL version (default 3.3.3; --sdl needs 3.4.0 or later)
#   --sdl                run SmokeSdl (an SDL3 window, like Minecraft 26.x) instead of Smoke
#   ARGS                 the harness's own arguments (default: --seconds=5 --mc)
# The run directory (game dir, output, logs) is /tmp/smoke-run, printed at the end.
set -euo pipefail

agent='' agent_options='' lwjgl=3.3.3 harness=Smoke
args=()
while [ "$#" -gt 0 ]; do
  case $1 in
    --agent) agent=$2; shift 2 ;;
    --agent-options) agent_options=$2; shift 2 ;;
    --lwjgl) lwjgl=$2; shift 2 ;;
    --sdl) harness=SmokeSdl; shift ;;
    --) shift; args=("$@"); break ;;
    *) echo "smoke.sh: unknown argument $1" >&2; exit 2 ;;
  esac
done
[ "${#args[@]}" -gt 0 ] || args=(--seconds=5 --mc)

cache=${SMOKE_CACHE:-/target/smoke-cache}
modules=''
[ "$harness" = SmokeSdl ] && modules=lwjgl-sdl
jars=$(LWJGL_NATIVES=natives-linux LWJGL_EXTRA_MODULES="$modules" \
  bash /src/ci/smoke/fetch-lwjgl.sh "$lwjgl" "$cache/lwjgl/$lwjgl")
cp=$(echo "$jars" | paste -sd: -)

classes=$cache/classes-$lwjgl-$harness
rm -rf "$classes" && mkdir -p "$classes"
javac --release 8 -nowarn -cp "$cp" -d "$classes" "/src/ci/smoke/$harness.java" /src/ci/smoke/SmokeImage.java

run=/tmp/smoke-run
rm -rf "$run" && mkdir -p "$run/natives" && cd "$run"
java_args=("-Dorg.lwjgl.system.SharedLibraryExtractPath=$run/natives")
if [ -n "$agent" ]; then
  java_args+=("-agentpath:$agent${agent_options:+=$agent_options}")
fi
status=0
LIBGL_ALWAYS_SOFTWARE=1 timeout "${SMOKE_TIMEOUT:-120}" \
  xvfb-run -a -s '-screen 0 1280x800x24' \
  java "${java_args[@]}" -cp "$cp:$classes" "$harness" "${args[@]}" 2>&1 | tee smoke.txt || status=$?
echo "smoke.sh: exit $status, run directory $run"
exit "$status"
