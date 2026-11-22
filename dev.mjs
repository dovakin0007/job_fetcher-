// Cross-platform dev launcher: backend first, frontend in a new terminal.
// Usage: node dev.mjs            (Windows, macOS, Linux — needs cargo + node/npm)
//
// 1. Starts `cargo run --bin rust-job-agent` in THIS terminal.
// 2. Waits until http://127.0.0.1:8787/api/health answers 200.
// 3. Opens a NEW terminal window running the Vite UI (web/).
// Ctrl+C here stops the backend; close the UI window separately.
import { spawn, spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = dirname(fileURLToPath(import.meta.url));
const WEB = join(ROOT, "web");
const API = "http://127.0.0.1:8787";
const HEALTH = `${API}/api/health`;
const UI = "http://127.0.0.1:5173";
const FE_ARGS = ["run", "dev", "--", "--host", "127.0.0.1", "--port", "5173", "--strictPort"];

function have(cmd) {
  const probe = process.platform === "win32" ? "where" : "command -v";
  const r = spawnSync(probe, [cmd], { stdio: "ignore", shell: process.platform === "win32" });
  // `command -v` needs a shell; `where` does not. Retry accordingly.
  if (r.status !== 0 && process.platform !== "win32") {
    return spawnSync("sh", ["-c", `command -v ${cmd}`], { stdio: "ignore" }).status === 0;
  }
  return r.status === 0;
}

function npmCmd() {
  if (process.platform === "win32") return "npm.cmd";
  return "npm";
}

async function waitForHealth(timeoutMs = 180_000) {
  const start = Date.now();
  while (Date.now() - start < timeoutMs) {
    try {
      const res = await fetch(HEALTH);
      if (res.ok) return true;
    } catch {
      // backend not up yet
    }
    await new Promise((r) => setTimeout(r, 1000));
  }
  return false;
}

function openFrontendTerminal() {
  const npm = npmCmd();
  if (process.platform === "win32") {
    // New console window, stays open (cmd /k) running Vite.
    const inner = `cd /d "${WEB}" && ${npm} ${FE_ARGS.join(" ")}`;
    spawn("cmd.exe", ["/c", "start", "rust-job-agent-ui", "cmd", "/k", inner], {
      stdio: "ignore",
      detached: true,
    }).unref();
    return "new Command Prompt window";
  }
  if (process.platform === "darwin") {
    const script = `cd ${JSON.stringify(WEB)} && ${npm} ${FE_ARGS.join(" ")}`;
    spawn("osascript", [
      "-e",
      `tell application "Terminal" to do script ${JSON.stringify(script)}`,
      "-e",
      'tell application "Terminal" to activate',
    ], { stdio: "ignore", detached: true }).unref();
    return "new Terminal window";
  }
  // Linux: first emulator found wins.
  const feCmd = `cd ${JSON.stringify(WEB)} && ${npm} ${FE_ARGS.join(" ")}; exec $SHELL`;
  const terminals = [
    ["gnome-terminal", ["--", "sh", "-c", feCmd]],
    ["konsole", ["-e", "sh", "-c", feCmd]],
    ["xfce4-terminal", ["-e", `sh -c ${JSON.stringify(feCmd)}`]],
    ["x-terminal-emulator", ["-e", `sh -c ${JSON.stringify(feCmd)}`]],
    ["xterm", ["-e", `sh -c ${JSON.stringify(feCmd)}`]],
  ];
  for (const [bin, args] of terminals) {
    if (have(bin)) {
      spawn(bin, args, { stdio: "ignore", detached: true }).unref();
      return `new ${bin} window`;
    }
  }
  // No GUI terminal: fall back to running Vite in the background here.
  const ui = spawn(npm, FE_ARGS, { cwd: WEB, stdio: "inherit", shell: false });
  process.on("exit", () => { try { ui.kill(); } catch { /* noop */ } });
  return "background of this terminal (no GUI terminal emulator found)";
}

async function main() {
  if (!have("cargo")) {
    console.error("cargo was not found. Install Rust, then retry.");
    process.exit(1);
  }
  if (!have(process.platform === "win32" ? "npm.cmd" : "npm")) {
    console.error("npm was not found. Install Node.js, then retry.");
    process.exit(1);
  }
  if (!existsSync(join(WEB, "node_modules"))) {
    console.log("Installing frontend packages...");
    const r = spawnSync(npmCmd(), ["install"], { cwd: WEB, stdio: "inherit", shell: process.platform === "win32" });
    if (r.status !== 0) process.exit(r.status ?? 1);
  }

  console.log(`\nAPI  ${API}\nUI   ${UI}\n`);
  const backend = spawn("cargo", ["run", "--bin", "rust-job-agent"], {
    cwd: ROOT,
    stdio: "inherit",
    shell: process.platform === "win32",
  });

  let stopping = false;
  const stop = () => {
    if (stopping) return;
    stopping = true;
    try { backend.kill("SIGINT"); } catch { /* noop */ }
  };
  process.on("SIGINT", stop);
  process.on("SIGTERM", stop);

  const up = await waitForHealth();
  if (stopping) return;
  if (!up) {
    console.error(`Backend never became healthy at ${HEALTH}. Leaving backend running; fix the error above.`);
    return;
  }
  const where = openFrontendTerminal();
  console.log(`Backend is up. Frontend opening in ${where}: ${UI}`);
  console.log("The UI proxies /api to this backend. Ctrl+C here stops the backend.");

  await new Promise((resolve) => backend.on("exit", resolve));
}

main().catch((err) => {
  console.error(err instanceof Error ? err.message : err);
  process.exit(1);
});
