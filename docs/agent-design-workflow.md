# Agent design workflow

Impeccable is optional **development infrastructure**, not a Mira runtime dependency. It does not ship in the desktop app. The project contract PR introduces root `PRODUCT.md` and `DESIGN.md`; read those before any UI work. Until that PR lands, use the README, `docs/design.md`, and actual UI as the incumbent evidence.

## Install locally

Requirements: Node.js 22+, GitHub CLI (`gh`) with release-download access, and `unzip`. Linux/macOS terminals are supported; on Windows use Git Bash with these commands available. Native PowerShell installation is not provided by this script.

From the checkout:

```sh
node scripts/setup-design-tools.mjs pi codex claude
# Or install only the harness you use:
node scripts/setup-design-tools.mjs pi
```

The script downloads `pbakaus/impeccable`'s `skill-v4.5.0` universal release, verifies the SHA-256 committed in `tools/impeccable.lock.json`, and copies only the requested skill folders:

| Harness     | Local skill                 |
| ----------- | --------------------------- |
| Pi          | `.pi/skills/impeccable`     |
| Codex       | `.agents/skills/impeccable` |
| Claude Code | `.claude/skills/impeccable` |

When both Pi and Codex are requested, only `.agents/skills/impeccable` is installed: Pi discovers that shared path too, avoiding duplicate skill-name warnings. These generated folders are ignored, not vendored. No global harness settings, hook manifests, standalone advisor agents, app dependencies, or credentials are changed. Existing skills are never replaced. For updates, review upstream changes, update the release/hash lock in a PR, and manually move the old installation aside before reinstalling. Do not use an unpinned `update` command for the project baseline.

Reload the harness after installing. Verify the skill is discovered there; slash syntax varies by harness (Pi's explicit form is `/skill:impeccable`; Codex also exposes `$impeccable`). Instructions and commands are in the installed `SKILL.md`.

The upstream launcher can download its engine on first use into `~/.impeccable/bin/`, checking the upstream checksum sidecar. The pinned skill requests engine `0.1.11`. It can also use an existing engine or `IMPECCABLE_BIN`; that override/cache is a developer trust boundary, not validated by Mira's installer. Review engine downloads before unattended use. Setup alone does not execute the engine.

## Bounded UI workflow

1. Read `PRODUCT.md`, `DESIGN.md`, relevant components, translations, and the requested behavior. Mira's existing quiet Geist identity outranks generic detector taste warnings.
2. For a new interaction, use Impeccable `shape`; for existing UI, start with `audit` or `critique`. Name a narrow target such as the composer or provider settings.
3. Implement only the approved scope. Do not replace product contracts with generated preferences or add tools, orchestration UI, decorative dashboards, or new factual claims.
4. Inspect the rendered app: light/dark, English/Chinese, narrow window, keyboard focus, empty/loading/error/streaming/cancel states. Browser mocks do not prove native persistence or OS behavior.
5. Use `polish` for a bounded finish pass. Run relevant engineering checks; attach findings and screenshots to the PR, not runtime code.

The installed skill is sufficient for ordinary review/refinement. Optional upstream standalone agents, automatic hooks, image generation, and live-mode browser wiring are **not installed or configured**. Their permissions and downloads require a separate review. Do not run `init`/`document` merely to overwrite already-approved root contracts.

## Deterministic detector

After a Pi-only install, invoke from the checkout:

```sh
sh .pi/skills/impeccable/scripts/impeccable engine-probe
sh .pi/skills/impeccable/scripts/impeccable detect --json src
```

For the shared Pi/Codex install or a Claude-only install, substitute that harness's skill path. On Windows without `sh`, use the skill's `scripts/impeccable.cmd` launcher.

Exit `0` means completed without primary findings, `2` means completed with findings, and `1` means scan failure. Treat findings as review evidence, **not a blocking CI gate** yet. A clean static scan cannot certify accessibility, usability, responsive behavior, or rendered contrast.

Baseline on `f16ca85` with engine `0.1.11`: three warnings—Markdown blockquote `border-l-4` in `MarkdownMessage.tsx`, and Geist Sans/Mono in `index.css`. Those are incumbent design choices requiring contextual review, not authorization to restyle the app. No suppressions or unrelated UI fixes are introduced by this infrastructure PR.

## Safety and artifacts

Skill text is executable workflow guidance: review the pinned source before trusting it. The checksum pins the reviewed archive; it is not an independent publisher signature. Installer download/extraction errors fail the command. If copying fails partway, preserve the partial folder and inspect it before reinstalling (the next run refuses to overwrite it).

Local caches/screenshots/live state are ignored. Shared `.impeccable/config.json`, `design.json`, surface briefs, and critique Markdown are not ignored; commit them only when deliberately reviewed, with private conversation/provider data removed. Never weaken browser security, expose API keys, connect to live providers in smoke tests, or allow a design tool to change production resources.

Upstream: [repository](https://github.com/pbakaus/impeccable), [pinned release](https://github.com/pbakaus/impeccable/releases/tag/skill-v4.5.0), [Apache-2.0 license](https://github.com/pbakaus/impeccable/blob/main/LICENSE).
