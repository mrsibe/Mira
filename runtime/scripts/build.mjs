import { execFileSync } from "node:child_process";
import { mkdirSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const host = execFileSync("rustc", ["-vV"], { encoding: "utf8" })
  .split("\n")
  .find((line) => line.startsWith("host: "))
  ?.slice(6);
const target = process.env.TAURI_ENV_TARGET_TRIPLE || host;
const targets = {
  "x86_64-unknown-linux-gnu": "bun-linux-x64-baseline",
  "aarch64-unknown-linux-gnu": "bun-linux-arm64",
  "x86_64-pc-windows-msvc": "bun-windows-x64-baseline",
  "aarch64-apple-darwin": "bun-darwin-arm64",
  "x86_64-apple-darwin": "bun-darwin-x64",
};
if (!target || !targets[target])
  throw new Error(`Unsupported runtime target: ${target}`);
const extension = target.includes("windows") ? ".exe" : "";
const output = resolve(
  root,
  `src-tauri/binaries/mira-runtime-${target}${extension}`,
);
mkdirSync(dirname(output), { recursive: true });
execFileSync(
  process.platform === "win32" ? "bun.exe" : "bun",
  [
    "build",
    "--compile",
    "--minify",
    `--target=${targets[target]}`,
    resolve(root, "runtime/src/main.ts"),
    "--outfile",
    output,
  ],
  { stdio: "inherit", cwd: root },
);
console.log(`Built Pi runtime for ${target}`);
