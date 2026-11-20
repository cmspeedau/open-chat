#!/usr/bin/env bash
# Local smoke test for the built AppImage: does the packaged daemon serve?
#
# Two paths, because both are user-facing:
#   1. no arguments at all -> AppRun must default to `serve`
#   2. explicit `serve --bind` -> the endpoints the web panel calls
#
# Teardown is by port->PID lookup, never `pkill -f`: a pattern kill also
# matches the shell running this script.
set -euo pipefail

ROOT="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
IMG="$(ls "$ROOT"/dist/open-chat-*-x86_64.AppImage | head -1)"
DEFAULT_PORT=8420
PORT="${PORT:-8429}"

WS="$(mktemp -d)"
LOG="$WS/serve.log"
mkdir -p "$WS/.openchat"
cat >"$WS/.openchat/config.toml" <<'TOML'
[llm]
model = "mock-model"
[skills]
paths = []
TOML

export APPIMAGE_EXTRACT_AND_RUN=1

wait_for_port() { # wait_for_port PORT -> echoes pid, or empty on timeout
	port="$1"
	for _ in $(seq 1 60); do
		pid="$(ss -tlnp 2>/dev/null | grep ":$port " | grep -oP 'pid=\K[0-9]+' | head -1 || true)"
		[ -n "$pid" ] && { echo "$pid"; return 0; }
		sleep 0.5
	done
	echo ""
}

stop_port() { # stop_port PORT PID
	[ -n "${2:-}" ] && kill "$2" 2>/dev/null || true
	sleep 0.5
	if [ -n "$(ss -tlnp 2>/dev/null | grep ":$1 " || true)" ]; then
		echo "warning: port $1 is still bound" >&2
	fi
}

if [ -n "$(ss -tlnp 2>/dev/null | grep ":$DEFAULT_PORT " || true)" ]; then
	echo "SKIP path 1: something already listens on $DEFAULT_PORT (not ours to kill)"
else
	echo "=== 1. no-arg invocation (AppRun default) ==="
	(cd "$WS" && setsid "$IMG" >"$LOG" 2>&1 &)
	pid="$(wait_for_port "$DEFAULT_PORT")"
	if [ -z "$pid" ]; then
		echo "FAIL: no-arg AppRun never served on $DEFAULT_PORT" >&2
		cat "$LOG" >&2
		exit 1
	fi
	echo "healthz: $(curl -sf -m 10 "http://127.0.0.1:$DEFAULT_PORT/healthz")"
	stop_port "$DEFAULT_PORT" "$pid"
fi

echo "=== 2. explicit serve --bind $PORT ==="
(cd "$WS" && setsid "$IMG" serve --bind "127.0.0.1:$PORT" >"$LOG" 2>&1 &)
pid="$(wait_for_port "$PORT")"
if [ -z "$pid" ]; then
	echo "FAIL: daemon from the image never listened on $PORT" >&2
	cat "$LOG" >&2
	exit 1
fi

echo "healthz: $(curl -sf -m 10 "http://127.0.0.1:$PORT/healthz")"
status="$(curl -sf -m 10 "http://127.0.0.1:$PORT/status")"
printf 'status : %s\n' "$(printf '%s' "$status" | python3 -c '
import json, sys
d = json.load(sys.stdin)
g = d["governor"]
print(d["version"], d["llm_provider"], d["llm_model"],
      str(g["cores"]) + "/" + str(g["usable_total"]) + " cores")
')"
usage_doc="$(curl -sf -m 10 "http://127.0.0.1:$PORT/usage")"
printf 'usage  : %s\n' "$(printf '%s' "$usage_doc" | python3 -c '
import json, sys
d = json.load(sys.stdin)
limits = d.get("limits") or {}
print(str(d["turns"]) + " turns, reported " + str(d["reported_input"]) + "/"
      + str(d["reported_output"]) + ", limits keys: " + (",".join(limits) or "none"))
')"
printf 'index  : %s\n' "$(curl -sf -m 10 "http://127.0.0.1:$PORT/" | head -c 44 | tr -d '\n')"
printf 'logo   : HTTP %s\n' "$(curl -s -m 10 -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PORT/logo-light.svg")"
# an unconfigured provider must answer with an honest error, never a hang
printf 'models : HTTP %s (502 = provider not configured, disclosed)\n' "$(curl -s -m 20 -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PORT/models")"

stop_port "$PORT" "$pid"
echo "PASS"
