// Runs the rope unit tests: bundles src/rope.test.ts with esbuild into the
// OS temp dir, then executes it with node --test. Keeps the repo clean.
import esbuild from "esbuild";
import { execFileSync } from "node:child_process";
import { tmpdir } from "node:os";
import { join } from "node:path";

const out = join(tmpdir(), `rope-test-${process.pid}.cjs`);
await esbuild.build({
  entryPoints: ["src/rope.test.ts"],
  bundle: true,
  platform: "node",
  format: "cjs",
  outfile: out,
  logLevel: "error",
});
try {
  execFileSync(process.execPath, ["--test", out], { stdio: "inherit" });
} finally {
  const { rmSync } = await import("node:fs");
  try {
    rmSync(out, { force: true });
  } catch {
    /* best effort */
  }
}
