#!/usr/bin/env node
// Project-local, opt-in setup. No hooks or Mira runtime dependencies.
import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import {
  cpSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const lock = JSON.parse(
  readFileSync(join(root, "tools/impeccable.lock.json"), "utf8"),
);
const providers = process.argv.slice(2);
const destinations = { pi: ".pi", codex: ".agents", claude: ".claude" };
if (
  !providers.length ||
  providers.some((provider) => !Object.hasOwn(destinations, provider))
) {
  console.error(
    "Usage: node scripts/setup-design-tools.mjs pi [codex] [claude]",
  );
  process.exit(1);
}
// Pi also discovers .agents/skills; avoid duplicate names when Codex is selected.
const selected = [...new Set(providers)].filter(
  (provider) => !(provider === "pi" && providers.includes("codex")),
);
if (providers.includes("pi") && providers.includes("codex")) {
  console.log("Pi will discover the shared Codex skill in .agents/skills.");
}
if (
  (selected.includes("pi") &&
    existsSync(join(root, ".agents/skills/impeccable"))) ||
  (selected.includes("codex") &&
    existsSync(join(root, ".pi/skills/impeccable")))
) {
  throw new Error(
    "A Pi-discoverable Impeccable skill already exists. Review/move it before installing another copy.",
  );
}
const targets = selected.map((provider) => ({
  provider,
  path: join(destinations[provider], "skills/impeccable"),
}));
// Never replace an existing skill, even if it was installed by this script.
for (const target of targets) {
  if (existsSync(join(root, target.path))) {
    throw new Error(
      `${target.path} already exists. Review/move the existing installation before updating.`,
    );
  }
}
const staging = mkdtempSync(join(tmpdir(), "mira-design-tools-"));
try {
  execFileSync(
    "gh",
    [
      "release",
      "download",
      lock.release,
      "--repo",
      lock.repository,
      "--pattern",
      lock.asset,
      "--dir",
      staging,
    ],
    { stdio: "inherit" },
  );
  const archive = join(staging, lock.asset);
  const digest = createHash("sha256")
    .update(readFileSync(archive))
    .digest("hex");
  if (digest !== lock.sha256)
    throw new Error(
      "Impeccable archive checksum mismatch; refusing installation.",
    );
  // Only extract the pinned, checksum-verified skill folders. No provider hooks.
  execFileSync(
    "unzip",
    [
      "-q",
      archive,
      ...targets.map((target) => `${target.path}/*`),
      "-d",
      join(staging, "unpacked"),
    ],
    { stdio: "inherit" },
  );
  for (const target of targets) {
    const source = join(staging, "unpacked", target.path);
    if (!existsSync(join(source, "SKILL.md")))
      throw new Error(`Missing skill for ${target.provider}`);
  }
  for (const target of targets) {
    mkdirSync(dirname(join(root, target.path)), { recursive: true });
    cpSync(join(staging, "unpacked", target.path), join(root, target.path), {
      recursive: true,
      errorOnExist: true,
      force: false,
    });
    console.log(
      `Installed ${lock.release} for ${target.provider} at ${target.path}`,
    );
  }
  console.log(
    "Reload your coding agent. Read PRODUCT.md and DESIGN.md before invoking Impeccable.",
  );
} finally {
  rmSync(staging, { recursive: true, force: true });
}
