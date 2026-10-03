import { create } from "zustand";
import { listen } from "@tauri-apps/api/event";
import type {
  ChatMessage,
  Conversation,
  Locale,
  Memory,
  MemoryPatch,
  MessageStreamDelta,
  ModelConfig,
  ModelSettings,
  Page,
  Project,
  ThemeMode,
} from "../core/types";
import { tauriClient } from "../core/tauriClient";
import { readStoredLocale, setCurrentLocale, storeLocale, t } from "../i18n";
import { fallbackMessage } from "./fallbackMessage";
import {
  applyThemeMode,
  readStoredThemeMode,
  storeThemeMode,
} from "../utils/theme";

let memoriesChangedUnlisten: Awaited<ReturnType<typeof listen>> | null = null;

async function sendToLlm(
  set: (
    partial: Partial<AppState> | ((state: AppState) => Partial<AppState>),
  ) => void,
  get: () => AppState,
  conversationId: string,
  content: string,
  conversationProjectId: string | null,
  userMessageId: string,
  userDisplayContent?: string,
) {
  const requestId = crypto.randomUUID();
  const optimisticAssistantMessage: ChatMessage = {
    id: `streaming-${requestId}`,
    conversation_id: conversationId,
    role: "assistant",
    content: "",
    created_at: new Date().toISOString(),
  };

  set((current) => ({
    isSending: true,
    messages:
      current.activeConversationId === conversationId
        ? [...current.messages, optimisticAssistantMessage]
        : current.messages,
  }));

  const unlisten = await listen<MessageStreamDelta>(
    "message_stream_delta",
    (event) => {
      const delta = event.payload;
      set((current) => {
        if (
          delta.request_id !== requestId ||
          current.activeConversationId !== delta.conversation_id
        ) {
          return current;
        }
        if (current.cancelRequested) {
          return current;
        }
        if (delta.reasoning) {
          return {
            messages: current.messages.map((message) =>
              message.id === optimisticAssistantMessage.id
                ? {
                    ...message,
                    reasoning: (message.reasoning ?? "") + delta.reasoning,
                  }
                : message,
            ),
          };
        }
        return {
          messages: current.messages.map((message) =>
            message.id === optimisticAssistantMessage.id
              ? { ...message, content: message.content + delta.content }
              : message,
          ),
        };
      });
    },
  );

  try {
    const result = await tauriClient.sendMessage(
      conversationId,
      content,
      get().activeModelConfigId,
      conversationProjectId,
      requestId,
    );
    if (get().cancelRequested) {
      set({ isSending: false, cancelRequested: false });
      return;
    }
    set((current) => ({
      activeConversationId: result.conversation.id,
      activeProjectId:
        current.activeConversationId === result.conversation.id
          ? result.conversation.project_id
          : current.activeProjectId,
      conversations: [
        result.conversation,
        ...current.conversations.filter(
          (item) => item.id !== result.conversation.id,
        ),
      ],
      messages:
        current.activeConversationId === result.conversation.id
          ? [
              ...current.messages.map((message) =>
                message.id === userMessageId
                  ? {
                      ...result.user_message,
                      content:
                        userDisplayContent ?? result.user_message.content,
                    }
                  : message.id === optimisticAssistantMessage.id
                    ? {
                        ...result.assistant_message!,
                        reasoning: current.messages.find(
                          (m) => m.id === optimisticAssistantMessage.id,
                        )?.reasoning,
                      }
                    : message,
              ),
            ]
          : current.messages,
      isSending: false,
    }));
    await get().loadMemories();
  } finally {
    unlisten();
  }
}

export interface AppState {
  currentPage: Page;
  conversations: Conversation[];
  archivedConversations: Conversation[];
  projects: Project[];
  activeProjectId: string | null;
  activeConversationId: string | null;
  messages: ChatMessage[];
  modelConfigs: ModelConfig[];
  modelSettings: ModelSettings | null;
  activeModelConfigId: string | null;
  memories: Memory[];
  isSending: boolean;
  cancelRequested: boolean;
  error: string | null;
  themeMode: ThemeMode;
  locale: Locale;
  isSidebarCollapsed: boolean;
  setPage: (page: Page) => void;
  setThemeMode: (themeMode: ThemeMode) => void;
  setLocale: (locale: Locale) => void;
  requestCancel: () => void;
  toggleSidebar: () => void;
  setActiveProject: (projectId: string | null) => void;
  bootstrap: () => Promise<void>;
  createConversation: (projectId?: string | null) => Promise<void>;
  createProject: (name: string) => Promise<void>;
  deleteProject: (projectId: string) => Promise<void>;
  renameProject: (projectId: string, name: string) => Promise<void>;
  renameConversation: (conversationId: string, title: string) => Promise<void>;
  selectConversation: (conversationId: string) => Promise<void>;
  archiveConversation: (conversationId: string) => Promise<void>;
  restoreConversation: (conversationId: string) => Promise<void>;
  deleteConversation: (conversationId: string) => Promise<void>;
  moveConversationToProject: (
    conversationId: string,
    projectId: string | null,
  ) => Promise<void>;
  sendMessage: (content: string) => Promise<void>;
  setActiveModel: (modelConfigId: string) => void;
  saveModelSettings: (settings: ModelSettings) => Promise<void>;
  loadMemories: (query?: string) => Promise<void>;
  createSavedMemory: (fact: string) => Promise<void>;
  updateMemory: (id: number, patch: MemoryPatch) => Promise<void>;
  deleteMemory: (id: number) => Promise<void>;
  runMemoryCleanup: () => Promise<void>;
  saveModelConfig: (config: ModelConfig) => Promise<void>;
  deleteModelConfig: (id: string) => Promise<void>;
}

export const useAppStore = create<AppState>((set, get) => ({
  currentPage: "chat",
  conversations: [],
  archivedConversations: [],
  projects: [],
  activeProjectId: null,
  activeConversationId: null,
  messages: [],
  modelConfigs: [],
  modelSettings: null,
  activeModelConfigId: null,
  memories: [],
  isSending: false,
  cancelRequested: false,
  error: null,
  themeMode: readStoredThemeMode(),
  locale: readStoredLocale(),
  isSidebarCollapsed: false,
  setPage: (page) =>
    set((state) => {
      if (page === state.currentPage) {
        return { currentPage: page };
      }
      if (
        page === "settings" &&
        isDraftConversationId(state.activeConversationId)
      ) {
        return {
          currentPage: page,
          conversations: state.conversations.filter(
            (item) => item.id !== state.activeConversationId,
          ),
          activeConversationId: null,
          messages: [],
        };
      }
      return { currentPage: page };
    }),
  setThemeMode: (themeMode) => {
    storeThemeMode(themeMode);
    applyThemeMode(themeMode);
    set({ themeMode });
  },
  setLocale: (locale) => {
    storeLocale(locale);
    setCurrentLocale(locale);
    set({ locale });
  },
  requestCancel: () => {
    set({ cancelRequested: true });
    tauriClient.cancelMessage();
  },
  toggleSidebar: () =>
    set((state) => ({ isSidebarCollapsed: !state.isSidebarCollapsed })),
  setActiveProject: (projectId) => set({ activeProjectId: projectId }),
  bootstrap: async () => {
    try {
      const [
        conversations,
        archivedConversations,
        projects,
        modelConfigs,
        modelSettings,
        memories,
      ] = await Promise.all([
        tauriClient.listConversations(),
        tauriClient.listArchivedConversations(),
        tauriClient.listProjects(),
        tauriClient.listModelConfigs(),
        tauriClient.getModelSettings(),
        tauriClient.listMemories(),
      ]);
      const activeConversationId = conversations[0]?.id ?? null;
      const messages = activeConversationId
        ? await tauriClient.getConversationMessages(activeConversationId)
        : [];
      const activeModelConfigId =
        modelSettings?.chat_model_config_id ??
        modelConfigs.find((config) => config.is_default)?.id ??
        modelConfigs[0]?.id ??
        null;
      if (!memoriesChangedUnlisten) {
        memoriesChangedUnlisten = await listen("memories_changed", () => {
          void get().loadMemories();
        });
      }
      set({
        conversations,
        archivedConversations,
        projects,
        modelConfigs,
        modelSettings: {
          ...modelSettings,
          chat_model_config_id: activeModelConfigId,
        },
        memories,
        activeConversationId,
        activeModelConfigId,
        messages,
        error: null,
      });
    } catch (error) {
      set({
        error: String(error),
        messages: [fallbackMessage(t("errors.backendNotReady"))],
      });
    }
  },
  createConversation: async (projectId) => {
    const targetProjectId =
      projectId !== undefined ? projectId : get().activeProjectId;
    const draft = createDraftConversation(targetProjectId);
    set((state) => ({
      conversations: [
        draft,
        ...state.conversations.filter(
          (item) => !isDraftConversationId(item.id),
        ),
      ],
      activeConversationId: draft.id,
      activeProjectId: draft.project_id,
      messages: [],
    }));
  },
  createProject: async (name) => {
    const project = await tauriClient.createProject(name);
    set((state) => ({
      projects: [project, ...state.projects],
      activeProjectId: project.id,
      currentPage: "chat",
    }));
  },
  deleteProject: async (projectId) => {
    await tauriClient.deleteProject(projectId);
    set((state) => {
      const removedIds = new Set(
        state.conversations
          .filter((c) => c.project_id === projectId)
          .map((c) => c.id),
      );
      const archivedRemovedIds = new Set(
        state.archivedConversations
          .filter((c) => c.project_id === projectId)
          .map((c) => c.id),
      );
      const activeConversationRemoved = removedIds.has(
        state.activeConversationId ?? "",
      );
      return {
        projects: state.projects.filter((p) => p.id !== projectId),
        conversations: state.conversations.filter((c) => !removedIds.has(c.id)),
        archivedConversations: state.archivedConversations.filter(
          (c) => !archivedRemovedIds.has(c.id),
        ),
        activeProjectId:
          state.activeProjectId === projectId ? null : state.activeProjectId,
        activeConversationId: activeConversationRemoved
          ? null
          : state.activeConversationId,
        messages: activeConversationRemoved ? [] : state.messages,
      };
    });
  },
  renameProject: async (projectId, name) => {
    const project = await tauriClient.renameProject(projectId, name);
    set((state) => ({
      projects: state.projects.map((p) => (p.id === projectId ? project : p)),
    }));
  },
  renameConversation: async (conversationId, title) => {
    const conversation = await tauriClient.renameConversation(
      conversationId,
      title,
    );
    set((state) => ({
      conversations: state.conversations.map((c) =>
        c.id === conversationId ? conversation : c,
      ),
    }));
  },
  selectConversation: async (conversationId) => {
    if (isDraftConversationId(conversationId)) {
      const conversation = get().conversations.find(
        (item) => item.id === conversationId,
      );
      set((state) => ({
        activeConversationId: conversationId,
        activeProjectId: conversation?.project_id ?? null,
        messages: [],
        conversations:
          isDraftConversationId(state.activeConversationId) &&
          state.activeConversationId !== conversationId
            ? state.conversations.filter(
                (item) => item.id !== state.activeConversationId,
              )
            : state.conversations,
      }));
      return;
    }
    const messages = await tauriClient.getConversationMessages(conversationId);
    const conversation = get().conversations.find(
      (item) => item.id === conversationId,
    );
    set((state) => ({
      activeConversationId: conversationId,
      activeProjectId: conversation?.project_id ?? state.activeProjectId,
      messages,
      conversations: isDraftConversationId(state.activeConversationId)
        ? state.conversations.filter(
            (item) => item.id !== state.activeConversationId,
          )
        : state.conversations,
    }));
  },
  archiveConversation: async (conversationId) => {
    if (isDraftConversationId(conversationId)) {
      set((state) => ({
        conversations: state.conversations.filter(
          (item) => item.id !== conversationId,
        ),
        activeConversationId:
          state.activeConversationId === conversationId
            ? null
            : state.activeConversationId,
        messages:
          state.activeConversationId === conversationId ? [] : state.messages,
      }));
      return;
    }
    await tauriClient.archiveConversation(conversationId);
    set((state) => {
      const conversation = state.conversations.find(
        (item) => item.id === conversationId,
      );
      const conversations = state.conversations.filter(
        (item) => item.id !== conversationId,
      );
      return {
        conversations,
        archivedConversations: conversation
          ? [
              { ...conversation, is_archived: true },
              ...state.archivedConversations,
            ]
          : state.archivedConversations,
        activeConversationId:
          state.activeConversationId === conversationId
            ? null
            : state.activeConversationId,
        messages:
          state.activeConversationId === conversationId ? [] : state.messages,
      };
    });
  },
  restoreConversation: async (conversationId) => {
    const restored = await tauriClient.restoreConversation(conversationId);
    set((state) => ({
      archivedConversations: state.archivedConversations.filter(
        (item) => item.id !== conversationId,
      ),
      conversations: [restored, ...state.conversations],
    }));
  },
  deleteConversation: async (conversationId) => {
    if (isDraftConversationId(conversationId)) {
      set((state) => ({
        conversations: state.conversations.filter(
          (item) => item.id !== conversationId,
        ),
        activeConversationId:
          state.activeConversationId === conversationId
            ? null
            : state.activeConversationId,
        messages:
          state.activeConversationId === conversationId ? [] : state.messages,
      }));
      return;
    }
    await tauriClient.deleteConversation(conversationId);
    set((state) => {
      const conversations = state.conversations.filter(
        (item) => item.id !== conversationId,
      );
      return {
        conversations,
        archivedConversations: state.archivedConversations.filter(
          (item) => item.id !== conversationId,
        ),
        activeConversationId:
          state.activeConversationId === conversationId
            ? null
            : state.activeConversationId,
        messages:
          state.activeConversationId === conversationId ? [] : state.messages,
      };
    });
  },
  moveConversationToProject: async (conversationId, projectId) => {
    if (isDraftConversationId(conversationId)) {
      set((state) => ({
        conversations: state.conversations.map((conversation) =>
          conversation.id === conversationId
            ? { ...conversation, project_id: projectId }
            : conversation,
        ),
        activeProjectId:
          state.activeConversationId === conversationId
            ? projectId
            : state.activeProjectId,
      }));
      return;
    }
    const updated = await tauriClient.moveConversationToProject(
      conversationId,
      projectId,
    );
    set((state) => ({
      conversations: state.conversations.map((conversation) =>
        conversation.id === conversationId ? updated : conversation,
      ),
      activeProjectId:
        state.activeConversationId === conversationId
          ? updated.project_id
          : state.activeProjectId,
    }));
  },
  sendMessage: async (content) => {
    const trimmed = content.trim();
    if (!trimmed) {
      return;
    }

    const state = get();
    let conversationId = state.activeConversationId;
    let conversationProjectId = state.activeProjectId;

    try {
      if (!conversationId || isDraftConversationId(conversationId)) {
        const draft = get().conversations.find(
          (item) => item.id === conversationId,
        );
        const conversation = await tauriClient.createConversation(
          titleFromContent(trimmed),
          draft?.project_id ?? state.activeProjectId,
        );
        conversationId = conversation.id;
        conversationProjectId = conversation.project_id;
        set((current) => ({
          conversations: [
            conversation,
            ...current.conversations.filter(
              (item) => !isDraftConversationId(item.id),
            ),
          ],
          activeConversationId: conversation.id,
          activeProjectId: conversation.project_id,
          messages: [],
        }));
      }

      const optimisticUserMessage: ChatMessage = {
        id: `optimistic-${crypto.randomUUID()}`,
        conversation_id: conversationId,
        role: "user",
        content: trimmed,
        created_at: new Date().toISOString(),
      };

      set((current) => ({
        isSending: true,
        error: null,
        messages:
          current.activeConversationId === conversationId
            ? [...current.messages, optimisticUserMessage]
            : current.messages,
      }));

      await sendToLlm(
        set,
        get,
        conversationId,
        trimmed,
        conversationProjectId,
        optimisticUserMessage.id,
      );
    } catch (error) {
      const errorMsg = String(error);
      if (get().cancelRequested || errorMsg === "__CANCELLED__") {
        set({ isSending: false, cancelRequested: false });
        return;
      }
      set({
        isSending: false,
        cancelRequested: false,
        error: errorMsg,
        messages: [
          ...get().messages.filter(
            (message) => !message.id.startsWith("streaming-"),
          ),
          fallbackMessage(t("errors.sendFailed", { error: errorMsg })),
        ],
      });
    }
  },
  setActiveModel: (modelConfigId) => {
    set((state) => ({
      activeModelConfigId: modelConfigId,
      modelSettings: {
        chat_model_config_id: modelConfigId,
        background_model_config_id:
          state.modelSettings?.background_model_config_id ?? null,
        background_model_follows_chat:
          state.modelSettings?.background_model_follows_chat ?? true,
      },
    }));
    const settings = get().modelSettings;
    if (settings) {
      void get().saveModelSettings(settings);
    }
  },
  saveModelSettings: async (settings) => {
    const saved = await tauriClient.saveModelSettings(settings);
    set({
      modelSettings: saved,
      activeModelConfigId:
        saved.chat_model_config_id ?? get().activeModelConfigId,
    });
  },
  loadMemories: async (query) => {
    const memories = await tauriClient.listMemories(query, undefined, false);
    set({ memories });
  },
  createSavedMemory: async (fact) => {
    const memory = await tauriClient.createSavedMemory(fact);
    set((state) => ({
      memories: [memory, ...state.memories],
    }));
  },
  updateMemory: async (id, patch) => {
    const updated = await tauriClient.updateMemory(id, patch);
    set((state) => ({
      memories: state.memories.map((memory) =>
        memory.id === id ? updated : memory,
      ),
    }));
  },
  deleteMemory: async (id) => {
    await tauriClient.deleteMemory(id);
    set((state) => ({
      memories: state.memories.filter((memory) => memory.id !== id),
    }));
  },
  runMemoryCleanup: async () => {
    await tauriClient.runMemoryCleanup();
    await get().loadMemories();
  },
  saveModelConfig: async (config) => {
    const saved = await tauriClient.saveModelConfig(config);
    set((state) => ({
      modelConfigs: state.modelConfigs.some((item) => item.id === saved.id)
        ? state.modelConfigs.map((item) =>
            item.id === saved.id ? saved : item,
          )
        : [saved, ...state.modelConfigs],
      activeModelConfigId: state.activeModelConfigId ?? saved.id,
      modelSettings: state.modelSettings ?? {
        chat_model_config_id: saved.id,
        background_model_config_id: null,
        background_model_follows_chat: true,
      },
    }));
  },
  deleteModelConfig: async (id) => {
    await tauriClient.deleteModelConfig(id);
    set((state) => {
      const modelConfigs = state.modelConfigs.filter(
        (config) => config.id !== id,
      );
      const wasActive = state.activeModelConfigId === id;
      const fallback =
        modelConfigs.find((config) => config.is_default)?.id ??
        modelConfigs[0]?.id ??
        null;
      const activeModelConfigId = wasActive
        ? fallback
        : state.activeModelConfigId;
      const modelSettings = state.modelSettings
        ? {
            ...state.modelSettings,
            chat_model_config_id:
              state.modelSettings.chat_model_config_id === id
                ? fallback
                : state.modelSettings.chat_model_config_id,
            background_model_config_id:
              state.modelSettings.background_model_config_id === id
                ? fallback
                : state.modelSettings.background_model_config_id,
          }
        : state.modelSettings;
      return { modelConfigs, activeModelConfigId, modelSettings };
    });
  },
}));

function titleFromContent(content: string): string {
  const title = Array.from(content.trim()).slice(0, 24).join("");
  return title || t("newConversationTitle");
}

const DRAFT_PREFIX = "draft-";

function isDraftConversationId(
  id: string | null | undefined,
): id is `draft-${string}` {
  return !!id && id.startsWith(DRAFT_PREFIX);
}

function createDraftConversation(projectId: string | null): Conversation {
  const now = new Date().toISOString();
  return {
    id: `${DRAFT_PREFIX}${crypto.randomUUID()}`,
    title: t("newConversationTitle"),
    project_id: projectId,
    is_archived: false,
    created_at: now,
    updated_at: now,
  };
}
