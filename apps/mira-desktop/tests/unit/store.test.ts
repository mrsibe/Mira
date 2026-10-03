import { beforeEach, describe, expect, it, vi } from "vitest";
import type {
  ChatMessage,
  Conversation,
  ModelConfig,
  ModelSettings,
} from "../../src/core/types";

type EventHandler = (event: { payload: unknown }) => void;

const eventBridge = vi.hoisted(() => {
  const listeners = new Map<string, EventHandler[]>();
  const listen = vi.fn(async (event: string, handler: EventHandler) => {
    const handlers = listeners.get(event) ?? [];
    handlers.push(handler);
    listeners.set(event, handlers);
    return () => {
      listeners.set(
        event,
        (listeners.get(event) ?? []).filter((item) => item !== handler),
      );
    };
  });
  const emit = (event: string, payload: unknown) => {
    for (const handler of [...(listeners.get(event) ?? [])]) {
      handler({ payload });
    }
  };
  const reset = () => listeners.clear();
  return { listen, emit, reset };
});

const client = vi.hoisted(() => ({
  cancelMessage: vi.fn(),
  listConversations: vi.fn(),
  listArchivedConversations: vi.fn(),
  listProjects: vi.fn(),
  listModelConfigs: vi.fn(),
  getModelSettings: vi.fn(),
  listMemories: vi.fn(),
  getConversationMessages: vi.fn(),
  createConversation: vi.fn(),
  archiveConversation: vi.fn(),
  restoreConversation: vi.fn(),
  deleteConversation: vi.fn(),
  moveConversationToProject: vi.fn(),
  renameConversation: vi.fn(),
  renameProject: vi.fn(),
  createProject: vi.fn(),
  deleteProject: vi.fn(),
  sendMessage: vi.fn(),
  saveModelSettings: vi.fn(),
  saveModelConfig: vi.fn(),
  deleteModelConfig: vi.fn(),
  createSavedMemory: vi.fn(),
  updateMemory: vi.fn(),
  deleteMemory: vi.fn(),
  runMemoryCleanup: vi.fn(),
}));

vi.mock("@tauri-apps/api/event", () => ({ listen: eventBridge.listen }));
vi.mock("../../src/core/tauriClient", () => ({ tauriClient: client }));

import { useAppStore } from "../../src/store/useAppStore";

const pristineState = useAppStore.getState();

const CONVERSATION: Conversation = {
  id: "c1",
  title: "First",
  project_id: null,
  is_archived: false,
  created_at: "2024-01-01T00:00:00.000Z",
  updated_at: "2024-01-01T00:00:00.000Z",
};

function conversation(overrides: Partial<Conversation> = {}): Conversation {
  return { ...CONVERSATION, ...overrides };
}

function chatMessage(overrides: Partial<ChatMessage> = {}): ChatMessage {
  return {
    id: "m1",
    conversation_id: "c1",
    role: "user",
    content: "hi",
    created_at: "2024-01-01T00:00:00.000Z",
    ...overrides,
  };
}

function modelConfig(overrides: Partial<ModelConfig> = {}): ModelConfig {
  return {
    id: "model-1",
    provider: "openai",
    name: "Mock provider",
    base_url: "https://example.test/v1",
    model: "mock-model",
    is_default: true,
    ...overrides,
  };
}

function modelSettings(overrides: Partial<ModelSettings> = {}): ModelSettings {
  return {
    chat_model_config_id: null,
    background_model_config_id: null,
    background_model_follows_chat: true,
    ...overrides,
  };
}

describe("useAppStore", () => {
  beforeEach(() => {
    eventBridge.reset();
    for (const fn of Object.values(client)) {
      fn.mockReset();
    }
    client.listConversations.mockResolvedValue([]);
    client.listArchivedConversations.mockResolvedValue([]);
    client.listProjects.mockResolvedValue([]);
    client.listModelConfigs.mockResolvedValue([]);
    client.getModelSettings.mockResolvedValue(modelSettings());
    client.listMemories.mockResolvedValue([]);
    client.getConversationMessages.mockResolvedValue([]);
    client.cancelMessage.mockResolvedValue(undefined);
    client.saveModelSettings.mockImplementation(async (settings) => settings);
    useAppStore.setState(pristineState, true);
  });

  describe("bootstrap", () => {
    it("hydrates state from the backend", async () => {
      client.listConversations.mockResolvedValue([conversation()]);
      client.getConversationMessages.mockResolvedValue([chatMessage()]);
      client.listModelConfigs.mockResolvedValue([modelConfig()]);
      client.getModelSettings.mockResolvedValue(
        modelSettings({ chat_model_config_id: "model-1" }),
      );
      client.listMemories.mockResolvedValue([{ id: 1, fact: "likes tea" }]);

      await useAppStore.getState().bootstrap();

      const state = useAppStore.getState();
      expect(state.conversations).toEqual([conversation()]);
      expect(state.activeConversationId).toBe("c1");
      expect(state.messages).toEqual([chatMessage()]);
      expect(state.activeModelConfigId).toBe("model-1");
      expect(state.memories).toEqual([{ id: 1, fact: "likes tea" }]);
      expect(state.error).toBeNull();
      expect(client.getConversationMessages).toHaveBeenCalledWith("c1");
    });

    it("records a fallback message when the backend is unavailable", async () => {
      client.listConversations.mockRejectedValue(new Error("ipc down"));

      await useAppStore.getState().bootstrap();

      const state = useAppStore.getState();
      expect(state.error).toContain("ipc down");
      expect(state.messages).toHaveLength(1);
      expect(state.messages[0].conversation_id).toBe("local");
      expect(state.messages[0].role).toBe("assistant");
      expect(state.messages[0].content).toContain("Backend is not ready");
    });
  });

  describe("conversations", () => {
    it("replaces the previous draft with a new draft", async () => {
      await useAppStore.getState().createConversation("p1");

      let state = useAppStore.getState();
      expect(state.conversations).toHaveLength(1);
      expect(state.activeConversationId).toMatch(/^draft-/);
      expect(state.activeProjectId).toBe("p1");

      await useAppStore.getState().createConversation("p1");
      state = useAppStore.getState();
      expect(state.conversations).toHaveLength(1);
      expect(state.activeConversationId).toMatch(/^draft-/);
    });

    it("drops an untouched draft when navigating to settings", async () => {
      await useAppStore.getState().createConversation();

      useAppStore.getState().setPage("settings");

      const state = useAppStore.getState();
      expect(state.currentPage).toBe("settings");
      expect(state.conversations).toHaveLength(0);
      expect(state.activeConversationId).toBeNull();
      expect(state.messages).toHaveLength(0);
    });

    it("archives a draft locally without touching the backend", async () => {
      await useAppStore.getState().createConversation();
      const draftId = useAppStore.getState().activeConversationId;
      expect(draftId).not.toBeNull();

      await useAppStore.getState().archiveConversation(draftId as string);

      const state = useAppStore.getState();
      expect(state.conversations).toHaveLength(0);
      expect(state.archivedConversations).toHaveLength(0);
      expect(state.activeConversationId).toBeNull();
      expect(client.archiveConversation).not.toHaveBeenCalled();
    });

    it("deletes a persisted conversation through the backend", async () => {
      useAppStore.setState({
        conversations: [conversation()],
        activeConversationId: "c1",
        messages: [chatMessage()],
      });
      client.deleteConversation.mockResolvedValue(undefined);

      await useAppStore.getState().deleteConversation("c1");

      const state = useAppStore.getState();
      expect(client.deleteConversation).toHaveBeenCalledWith("c1");
      expect(state.conversations).toHaveLength(0);
      expect(state.activeConversationId).toBeNull();
      expect(state.messages).toHaveLength(0);
    });

    it("loads messages when selecting a persisted conversation", async () => {
      useAppStore.setState({ conversations: [conversation()] });
      client.getConversationMessages.mockResolvedValue([chatMessage()]);

      await useAppStore.getState().selectConversation("c1");

      const state = useAppStore.getState();
      expect(client.getConversationMessages).toHaveBeenCalledWith("c1");
      expect(state.activeConversationId).toBe("c1");
      expect(state.messages).toEqual([chatMessage()]);
    });
  });

  describe("models", () => {
    it("persists the active chat model selection", async () => {
      useAppStore.setState({
        modelSettings: modelSettings({ chat_model_config_id: "model-a" }),
      });

      useAppStore.getState().setActiveModel("model-b");

      expect(useAppStore.getState().activeModelConfigId).toBe("model-b");
      expect(client.saveModelSettings).toHaveBeenCalledWith(
        modelSettings({ chat_model_config_id: "model-b" }),
      );
    });

    it("falls back to another model when the active one is deleted", async () => {
      useAppStore.setState({
        modelConfigs: [
          modelConfig({ id: "model-primary", is_default: false }),
          modelConfig({ id: "model-fallback", is_default: true }),
        ],
        activeModelConfigId: "model-primary",
        modelSettings: modelSettings({
          chat_model_config_id: "model-primary",
          background_model_config_id: "model-primary",
        }),
      });
      client.deleteModelConfig.mockResolvedValue(undefined);

      await useAppStore.getState().deleteModelConfig("model-primary");

      const state = useAppStore.getState();
      expect(client.deleteModelConfig).toHaveBeenCalledWith("model-primary");
      expect(state.modelConfigs.map((config) => config.id)).toEqual([
        "model-fallback",
      ]);
      expect(state.activeModelConfigId).toBe("model-fallback");
      expect(state.modelSettings?.chat_model_config_id).toBe("model-fallback");
      expect(state.modelSettings?.background_model_config_id).toBe(
        "model-fallback",
      );
    });
  });

  describe("sendMessage", () => {
    it("persists a draft conversation before the first send", async () => {
      const created = conversation({ id: "created-1", title: "Hi Mira" });
      client.createConversation.mockResolvedValue(created);
      client.sendMessage.mockResolvedValue({
        conversation: created,
        user_message: chatMessage({
          id: "user-1",
          conversation_id: "created-1",
        }),
        assistant_message: chatMessage({
          id: "assistant-1",
          conversation_id: "created-1",
          role: "assistant",
          content: "ok",
        }),
      });

      await useAppStore.getState().createConversation();
      await useAppStore.getState().sendMessage("Hi Mira");

      expect(client.createConversation).toHaveBeenCalledWith("Hi Mira", null);
      expect(client.sendMessage).toHaveBeenCalledWith(
        "created-1",
        "Hi Mira",
        null,
        null,
        expect.any(String),
      );
      const state = useAppStore.getState();
      expect(state.activeConversationId).toBe("created-1");
      expect(state.messages.map((message) => message.id)).toEqual([
        "user-1",
        "assistant-1",
      ]);
      expect(state.isSending).toBe(false);
    });

    it("folds streamed deltas into the assistant message", async () => {
      useAppStore.setState({
        conversations: [conversation()],
        activeConversationId: "c1",
        activeModelConfigId: "model-1",
      });
      client.sendMessage.mockImplementation(
        async (
          conversationId: string,
          _content: string,
          _modelConfigId: string | null,
          _projectId: string | null,
          requestId: string,
        ) => {
          eventBridge.emit("message_stream_delta", {
            request_id: requestId,
            conversation_id: conversationId,
            content: "Hello ",
          });
          eventBridge.emit("message_stream_delta", {
            request_id: requestId,
            conversation_id: conversationId,
            content: "from mock",
          });
          eventBridge.emit("message_stream_delta", {
            request_id: requestId,
            conversation_id: conversationId,
            content: "",
            reasoning: "considering",
          });
          return {
            conversation: conversation(),
            user_message: chatMessage({ id: "user-1", content: "Hi Mira" }),
            assistant_message: chatMessage({
              id: "assistant-1",
              role: "assistant",
              content: "Hello from mock",
            }),
          };
        },
      );

      await useAppStore.getState().sendMessage("Hi Mira");

      const state = useAppStore.getState();
      const assistant = state.messages.filter(
        (message) => message.role === "assistant",
      );
      expect(assistant).toHaveLength(1);
      expect(assistant[0].id).toBe("assistant-1");
      expect(assistant[0].content).toBe("Hello from mock");
      expect(assistant[0].reasoning).toBe("considering");
      expect(state.messages[0].content).toBe("Hi Mira");
      expect(state.isSending).toBe(false);
      expect(state.error).toBeNull();
    });

    it("ignores deltas that arrive after a cancel request", async () => {
      useAppStore.setState({
        conversations: [conversation()],
        activeConversationId: "c1",
        activeModelConfigId: "model-1",
      });
      let release: () => void = () => {};
      client.sendMessage.mockImplementation(
        (
          conversationId: string,
          _content: string,
          _modelConfigId: string | null,
          _projectId: string | null,
          requestId: string,
        ) =>
          new Promise((resolve) => {
            release = () => {
              eventBridge.emit("message_stream_delta", {
                request_id: requestId,
                conversation_id: conversationId,
                content: "late chunk",
              });
              resolve({
                conversation: conversation(),
                user_message: chatMessage({ id: "user-1" }),
                assistant_message: null,
              });
            };
          }),
      );

      const pending = useAppStore.getState().sendMessage("Hi Mira");
      await vi.waitFor(() => expect(client.sendMessage).toHaveBeenCalled());

      useAppStore.getState().requestCancel();
      expect(useAppStore.getState().cancelRequested).toBe(true);
      expect(client.cancelMessage).toHaveBeenCalledTimes(1);

      release();
      await pending;

      const state = useAppStore.getState();
      expect(state.isSending).toBe(false);
      expect(state.cancelRequested).toBe(false);
      expect(state.error).toBeNull();
      const assistant = state.messages.find(
        (message) => message.role === "assistant",
      );
      expect(assistant?.content).toBe("");
    });

    it("surfaces send failures and removes the streaming placeholder", async () => {
      useAppStore.setState({
        conversations: [conversation()],
        activeConversationId: "c1",
        activeModelConfigId: "model-1",
      });
      client.sendMessage.mockRejectedValue(new Error("provider offline"));

      await useAppStore.getState().sendMessage("Hi Mira");

      const state = useAppStore.getState();
      expect(state.isSending).toBe(false);
      expect(state.error).toContain("provider offline");
      expect(
        state.messages.some((message) => message.id.startsWith("streaming-")),
      ).toBe(false);
      expect(state.messages.at(-1)?.content).toContain("provider offline");
    });
  });
});
