#!/usr/bin/env bash
# Starts the Rust API in this terminal, waits until it is healthy,
# then opens the Vite UI in a NEW terminal window. Ctrl+C stops the backend.
set -euo pipefail
cd "$(dirname "$0")"

command -v cargo >/dev/null 2>&1 || { echo "cargo was not found. Install Rust, then retry." >&2; exit 1; }
command -v npm >/dev/null 2>&1 || { echo "npm was not found. Install Node.js, then retry." >&2; exit 1; }
if [ ! -d "web/node_modules" ]; then
  echo "Installing frontend packages..."
  npm install --prefix web
fi

echo ""
echo "API  http://127.0.0.1:8787"
echo "UI   http://127.0.0.1:5173"
echo "Starting backend first; the UI opens in a new window once the API is healthy."
echo ""

cargo run --bin rust-job-agent &
BACKEND_PID=$!

cleanup() {
  kill -INT "$BACKEND_PID" 2>/dev/null || true
  wait "$BACKEND_PID" 2>/dev/null || true
}
trap cleanup INT TERM EXIT

# Wait for health (up to 180s); abort if the backend dies first.
for _ in $(seq 1 180); do
  if ! kill -0 "$BACKEND_PID" 2>/dev/null; then
    echo "Backend exited before becoming healthy." >&2
    exit 1
  fi
  if curl -fsS --max-time 3 http://127.0.0.1:8787/api/health >/dev/null 2>&1; then
    HEALTHY=1
    break
  fi
  sleep 1
done
if [ "${HEALTHY:-0}" != "1" ]; then
  echo "Backend never became healthy at http://127.0.0.1:8787/api/health." >&2
  exit 1
fi

FE_CMD="cd '$PWD/web' && npm run dev -- --host 127.0.0.1 --port 5173 --strictPort; exec \$SHELL"
if [ "$(uname)" = "Darwin" ]; then
  osascript -e "tell application \"Terminal\" to do script \"$FE_CMD\"" -e 'tell application "Terminal" to activate'
  echo "UI opening in a new Terminal window."
elif command -v gnome-terminal >/dev/null 2>&1; then
  gnome-terminal -- sh -c "$FE_CMD"
  echo "UI opening in a new gnome-terminal window."
elif command -v konsole >/dev/null 2>&1; then
  konsole -e sh -c "$FE_CMD"
  echo "UI opening in a new konsole window."
elif command -v xfce4-terminal >/dev/null 2>&1; then
  xfce4-terminal -e "sh -c \"$FE_CMD\""
  echo "UI opening in a new xfce4-terminal window."
elif command -v x-terminal-emulator >/dev/null 2>&1; then
  x-terminal-emulator -e "sh -c \"$FE_CMD\""
  echo "UI opening in a new terminal window."
elif command -v xterm >/dev/null 2>&1; then
  xterm -e "sh -c \"$FE_CMD\"" &
  echo "UI opening in a new xterm window."
else
  echo "No GUI terminal emulator found; running UI in the background of this terminal."
  (cd web && npm run dev -- --host 127.0.0.1 --port 5173 --strictPort &)
fi

echo "UI   http://127.0.0.1:5173 (new window; close it separately)"
echo "The UI proxies /api to this backend. Ctrl+C stops the backend."
wait "$BACKEND_PID"
