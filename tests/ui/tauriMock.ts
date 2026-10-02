import type { Page } from "@playwright/test";

export interface MockConversation {
  id: string;
  title: string;
  project_id: string | null;
  is_archived: boolean;
  created_at: string;
  updated_at: string;
}

export interface MockMessage {
  id: string;
  conversation_id: string;
  role: "system" | "user" | "assistant";
  content: string;
  created_at: string;
  reasoning?: string;
}

export interface MockTauriOptions {
  conversations?: MockConversation[];
  archivedConversations?: MockConversation[];
  projects?: unknown[];
  modelConfigs?: unknown[];
  modelSettings?: unknown;
  memories?: unknown[];
  messagesByConversation?: Record<string, MockMessage[]>;
}

/** IPC calls recorded by the in-page mock, in order. */
export interface MockIpcCall {
  cmd: string;
  args: Record<string, unknown>;
}

/** Bridge the smoke tests use to drive the mocked Tauri event channel. */
export interface MiraTestBridge {
  calls: MockIpcCall[];
  lastSend: {
    conversationId: string | null;
    requestId: string;
    content: string;
  } | null;
  emit(event: string, payload: unknown): void;
  resolveSend(value: unknown): void;
  rejectSend(reason: unknown): void;
}

declare global {
  interface Window {
    __MIRA_TEST__?: MiraTestBridge;
  }
}

/**
 * Installs a minimal, explicit mock of the Tauri IPC boundary before the app
 * bundle runs. Every command the smoke tests need resolves from `options`; the
 * `send_message` command is held open so the test can drive stream deltas
 * through `window.__MIRA_TEST__` and then settle the call.
 *
 * This replaces the native side entirely: it does not exercise SQLite, the
 * system keyring, or process restarts.
 */
export async function installTauriMock(
  page: Page,
  options: MockTauriOptions = {},
): Promise<void> {
  await page.addInitScript((mockOptions: MockTauriOptions) => {
    const callbacks = new Map<number, (data: unknown) => void>();
    const listeners = new Map<number, { event: string; callbackId: number }>();
    const calls: MockIpcCall[] = [];
    let nextId = 1;
    let pendingResolve: ((value: unknown) => void) | null = null;
    let pendingReject: ((reason: unknown) => void) | null = null;

    function transformCallback(callback: (data: unknown) => void): number {
      const id = nextId++;
      callbacks.set(id, callback);
      return id;
    }

    function emit(eventName: string, payload: unknown) {
      for (const [id, entry] of listeners) {
        if (entry.event !== eventName) {
          continue;
        }
        callbacks.get(entry.callbackId)?.({ event: eventName, id, payload });
      }
    }

    const bridge: MiraTestBridge = {
      calls,
      lastSend: null,
      emit,
      resolveSend: (value: unknown) => pendingResolve?.(value),
      rejectSend: (reason: unknown) => pendingReject?.(reason),
    };

    const tauriWindow = window as unknown as {
      __TAURI_INTERNALS__?: unknown;
      __TAURI_EVENT_PLUGIN_INTERNALS__?: unknown;
      __MIRA_TEST__?: MiraTestBridge;
    };

    const internals = {
      metadata: {
        currentWindow: { label: "main" },
        currentWebview: { label: "main", windowLabel: "main" },
      },
      callbacks,
      plugins: { path: { sep: "/", delimiter: ":" } },
      transformCallback,
      unregisterCallback: (id: number) => callbacks.delete(id),
      runCallback: (id: number, data: unknown) => callbacks.get(id)?.(data),
      convertFileSrc: (filePath: string, protocol = "asset") =>
        `${protocol}://localhost/${encodeURIComponent(filePath)}`,
      invoke: async (cmd: string, args: Record<string, unknown> = {}) => {
        calls.push({ cmd, args });
        switch (cmd) {
          case "plugin:event|listen": {
            const id = nextId++;
            listeners.set(id, {
              event: String(args.event),
              callbackId: Number(args.handler),
            });
            return id;
          }
          case "plugin:event|unlisten":
            listeners.delete(Number(args.eventId));
            return null;
          case "plugin:event|emit":
          case "plugin:event|emit_to":
            return null;
          case "plugin:app|version":
            return "0.0.0-test";
          case "plugin:updater|check":
            return null;
          case "list_conversations":
            return mockOptions.conversations ?? [];
          case "list_archived_conversations":
            return mockOptions.archivedConversations ?? [];
          case "list_projects":
            return mockOptions.projects ?? [];
          case "list_model_configs":
            return mockOptions.modelConfigs ?? [];
          case "get_model_settings":
            return (
              mockOptions.modelSettings ?? {
                chat_model_config_id: null,
                background_model_config_id: null,
                background_model_follows_chat: true,
              }
            );
          case "list_memories":
            return mockOptions.memories ?? [];
          case "get_conversation_messages":
            return (
              mockOptions.messagesByConversation?.[
                String(args.conversationId)
              ] ?? []
            );
          case "send_message": {
            bridge.lastSend = {
              conversationId: (args.conversationId as string | null) ?? null,
              requestId: String(args.requestId),
              content: String(args.content),
            };
            return new Promise((resolve, reject) => {
              pendingResolve = resolve;
              pendingReject = reject;
            });
          }
          default:
            return null;
        }
      },
    };

    tauriWindow.__TAURI_INTERNALS__ = internals;
    tauriWindow.__TAURI_EVENT_PLUGIN_INTERNALS__ = {
      unregisterListener: (_event: string, id: number) => {
        listeners.delete(id);
        callbacks.delete(id);
      },
    };
    tauriWindow.__MIRA_TEST__ = bridge;
  }, options);
}
