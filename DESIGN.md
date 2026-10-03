# Mira Design Contract

This is the canonical design document for Mira. It upgrades the Geist token
reference into a contract covering tokens, layout, interaction states,
accessibility, errors, and cancellation. Implementation notes describe current
code; design requirements guide future changes and are not claims of full
compliance. See `src/index.css`, `src/components/*`, and `src/pages/*` for the
source of truth. Source paths beginning with `src/` or `public/` in this
contract are relative to `apps/mira-desktop/`.

`docs/design.md` is kept only as a compatibility pointer to this file.

## Character And Principles

Quiet, native-feeling, minimal, content-first. Mira should feel like a focused
chat client, closer to ChatGPT / Linear / Raycast than a SaaS dashboard. Preserve
the incumbent Geist identity, calm neutral surfaces and readable conversation.
The conversation—not app chrome—is the primary visual element.

- Prefer inline actions and clear state feedback; reserve confirmation dialogs
  for destructive operations and materially disruptive decisions.
- Keep hover actions secondary, but make them accessible by keyboard and touch.
- Describe user tasks, not runtime concepts: conversations, projects, memory,
  providers. Do not expose Pi sessions, extensions or orchestration internals.
- Reuse `Button`, inputs, dialogs, sidebar items, messages and the composer before
  inventing another component language. Distinguish primary, secondary and
  destructive actions consistently.
- Preserve English/Chinese layouts, light/dark themes, narrow desktop windows,
  text scaling and long content; do not rely on hover, color or icons alone.
- Avoid cards inside cards, decorative gradients, giant hero typography, excess
  pills, decorative icons and unnecessary borders. Code/blockquote formatting
  may use meaningful structural borders.

## Foundations (Geist Tokens)

Mira adopts the Vercel Geist grays and surfaces. The values below match
`src/index.css`.

### Color Scale Intent

Each gray step encodes a purpose, not just a lightness:

| Step | Usage                     |
| ---- | ------------------------- |
| 100  | Surface / card background |
| 200  | Secondary surface (hover) |
| 300  | Active background         |
| 400  | Default border            |
| 500  | Hover border              |
| 600  | Active border             |
| 700  | Solid fill, high contrast |
| 800  | Solid fill hover          |
| 900  | Secondary text / icons    |
| 1000 | Primary text / icons      |

### Light Theme (CSS Variables)

```css
:root {
  --gray-100: #f2f2f2;
  --gray-200: #ebebeb;
  --gray-300: #e6e6e6;
  --gray-400: #eaeaea;
  --gray-500: #c9c9c9;
  --gray-600: #a8a8a8;
  --gray-700: #8f8f8f;
  --gray-800: #7d7d7d;
  --gray-900: #4d4d4d;
  --gray-1000: #171717;
  --bg-100: #ffffff;
  --bg-200: #fafafa;
  --red-800: #ea001d;
  --red-900: #d8001b;
  --blue-700: #006bff;
}
```

### Dark Theme (CSS Variables)

Applied through `[data-theme="dark"]` by `src/utils/theme.ts`.

```css
[data-theme="dark"] {
  --gray-100: #1a1a1a;
  --gray-200: #1f1f1f;
  --gray-300: #292929;
  --gray-400: #2e2e2e;
  --gray-500: #454545;
  --gray-600: #878787;
  --gray-700: #8f8f8f;
  --gray-800: #7d7d7d;
  --gray-900: #a0a0a0;
  --gray-1000: #ededed;
  --bg-100: #000000;
  --bg-200: #000000;
  --red-800: #e2162a;
  --red-900: #d8001b;
  --blue-700: #006efe;
  --blue-900: #47a8ff;
}
```

### Semantic Aliases

Components consume semantic variables, not raw gray steps:

| Alias             | Maps to       | Purpose                       |
| ----------------- | ------------- | ----------------------------- |
| `--bg`            | `--bg-200`    | App background                |
| `--panel`         | `--bg-100`    | Cards, composer, title bar    |
| `--panel-soft`    | `--bg-200`    | Softer panel surface          |
| `--sidebar`       | `--bg-100`    | Conversation list surface     |
| `--text`          | `--gray-1000` | Primary text                  |
| `--muted`         | `--gray-900`  | Secondary text                |
| `--subtle`        | `--gray-800`  | Tertiary text                 |
| `--border`        | `--gray-400`  | Default border                |
| `--border-strong` | `--gray-500`  | Emphasized border / scrollbar |
| `--hover`         | `--gray-300`  | Hover background              |
| `--active`        | `--gray-400`  | Active background             |
| `--message-user`  | `--gray-200`  | User message bubble           |
| `--primary`       | `--gray-1000` | Primary action fill           |
| `--primary-text`  | `--bg-100`    | Text on primary action        |
| `--danger`        | `--red-800`   | Destructive / stop action     |
| `--code-block`    | `#0f1115`     | Code block background (fixed) |

### Radii

| Token | Value  | Usage                      |
| ----- | ------ | -------------------------- |
| sm    | 6px    | Controls, buttons, inputs  |
| md    | 12px   | Panels, modals, dialogs    |
| lg    | 16px   | Full-screen surfaces       |
| full  | 9999px | Pills, avatars, scrollbars |

Applied classes: controls use `rounded-md` (6px); panel, modal, and dialog
surfaces use `rounded-xl` (12px).

### Shadows

Light: raised `0 2px 2px rgba(0,0,0,0.04)`; popover
`0 1px 1px rgba(0,0,0,0.02), 0 4px 8px -4px rgba(0,0,0,0.04), 0 16px 24px -8px rgba(0,0,0,0.06)`;
modal replaces the popover's second and third layers, giving
`0 1px 1px rgba(0,0,0,0.02), 0 8px 16px -4px rgba(0,0,0,0.04), 0 24px 32px -8px rgba(0,0,0,0.06)`. Dark: raised `0 1px 2px rgba(0,0,0,0.16)`; popover and modal match
light.

### Typography

- UI text: Geist Sans (14px / 20px, weight 400)
- Labels / small: Geist Sans (13px / 16px, weight 400)
- Buttons: Geist Sans (14px / 20px, weight 500)
- Mono: Geist Mono (14px / 20px, weight 400)
- Code blocks: Geist Mono, 14px

Geist Sans and Geist Mono are loaded from `public/fonts` via `@font-face` in
`src/index.css`, with CJK fallbacks (`Noto Sans SC`, `Microsoft YaHei UI`).

### Spacing Scale

Base unit: 4px. Rhythm: 8px inside a group, 16px between groups, 32–40px
between sections.

## Layout Contract

### App Shell

Defined in `src/components/AppShell.tsx`.

- The shell is a full-height grid.
- Expanded: `grid-cols-[var(--sidebar-width)_4px_minmax(0,1fr)]` — sidebar,
  a 4px resize gutter, and the main content column.
- Collapsed: `grid-cols-[minmax(0,1fr)]` — the sidebar and gutter are removed.
- Sidebar width starts at 280px and is clamped to **220px minimum / 360px
  maximum**. The gutter sets `cursor: col-resize` and highlights to
  `--border-strong` on hover.
- Content column always uses `min-h-0 min-w-0 overflow-hidden` so children own
  their own scrolling.

### Window Chrome

Defined in `src/components/WindowTitleBar.tsx`. The window is undecorated
(`decorations: false`), so Mira renders its own title bar with minimize,
maximize/restore, and close controls, plus separate sidebar expand/collapse
controls.

### Conversation Surface

- The message column and composer are centered with `max-w-3xl`.
- The composer is pinned to the bottom (`shrink-0`) with a `min-h-14` control
  that grows with its textarea up to `max-h-44`.
- Long content scrolls inside the message column; the page itself does not
  scroll (`overflow: hidden` on `html, body, #root`).

### Component Metrics

Matches `src/components/ui/*`:

- Button default: `h-10` (40px), `rounded-md` (6px), `px-4`, `text-sm`.
- Button small: `h-8` (32px), `rounded-md`, `px-3`, `text-xs`.
- Button icon: `h-9 w-9` (36px).
- Input: `h-10` (40px), `rounded-md` (6px), `px-3`, `text-sm`.
- Textarea: `min-h-24` (96px), `rounded-md` (6px), `px-3 py-3`, `text-sm`.
- Composer container: `rounded-xl` (12px), `border`, `shadow-raised`.
- Alert dialog surface: `rounded-xl` (12px), `p-5`, `shadow-modal`.

Focus styling is a 2px ring using `--border-strong` or `--border`, not a glow.

## Interaction And State Contract

- **Empty conversation** shows a centered prompt
  (`chat.emptyHeading`) before the first message.
- **Sending** disables re-submit while a request is in flight
  (`isSending`), and the send control is disabled when the composer is empty.
- **Streaming** appends deltas to a temporary `streaming-<requestId>` message.
  While reasoning output arrives, the UI shows `chat.thinking` and, once
  reasoning text exists, a `chat.thought` section.
- **Errors** surface as a local fallback message in the conversation, never as
  a modal that blocks the composer.
- **Theme** follows `light`, `dark`, or `system`; `system` tracks
  `prefers-color-scheme` live.

## Accessibility Contract

Requirements for changed UI:

- Icon-only actions need accessible names and visible keyboard focus. Every
  operation must be usable without hover; resizing must have a keyboard path.
- Dialogs need an accessible title/description, initial focus, focus containment,
  Escape handling and restoration to the trigger after dismissal.
- Verify body/secondary/error text contrast in both themes; token use alone
  does not certify contrast. Respect reduced-motion preferences.
- Preserve Enter to submit and Shift+Enter for newline; do not submit during
  Chinese IME composition. Test long translations and content overflow.

Current baseline: many controls have `aria-label`, the gutter uses
`role="separator"`, and alert dialogs use `aria-modal`/`role="alertdialog"` with
Escape dismissal. However, `alert-dialog.tsx` does **not** implement a focus trap
or focus restoration, and the gutter does not implement keyboard resizing.
These are follow-up UI gaps, not functionality delivered by this documentation
PR. Full contrast, IME and reduced-motion compliance is not verified here.

## Error Contract

- Backend-not-ready: on bootstrap failure the store fetches a localized
  fallback (`errors.backendNotReady`) instead of an empty screen.
- Send failure: the store clears the streaming placeholder and appends
  `errors.sendFailed` with the raw error text.
- Errors are shown in-place in the conversation; the user can keep chatting
  without reloading.
- Provider/runtime failures are mapped to safe Chinese categories by the
  desktop inference adapter; provider prose, URLs and credentials are not
  surfaced. Other application errors still use backend strings. Further
  localization and a bounded diagnostic sink remain future work; raw provider
  bodies are not a safe logging API.

## Cancellation Contract

- While a reply streams, the send button becomes a stop button (`Square` icon,
  danger styling, `title="Stop generating"`).
- Pressing stop calls `requestCancel()`, which flags `cancelRequested` and
  invokes the backend `cancel_message` command.
- The Rust backend cancels the foreground runtime token directly, including
  stalled header/body reads and retry waits, then returns the `__CANCELLED__`
  sentinel. Attempt identity prevents an old request from registering over a
  successor's cancellation slot. Buffered provider output is not flushed after
  cancellation.
- The store treats `cancelRequested` or `__CANCELLED__` as a quiet stop: it
  clears `isSending` and stops appending deltas. Text already rendered stays on
  screen as the local `streaming-<requestId>` message; it is not a persisted
  assistant message.
- The backend writes no assistant message for a cancelled turn (it returns
  `assistant_message: null`), so the partial reply is not durable and is not
  reloaded from storage later.
- Cancellation is a quiet stop rather than an assistant error. The frontend
  still keeps `isSending` true until the request settles; immediate re-send is
  not implemented. Transport cancellation no longer depends on a new byte
  arriving. Background memory sessions are independent from foreground stop.

## Compatibility Pointer

`docs/design.md` previously held these token tables. It now points here. Keep
that pointer intact so older links still resolve.
