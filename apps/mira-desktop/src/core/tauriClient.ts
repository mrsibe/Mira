import { invoke } from "@tauri-apps/api/core";
import type {
  ChatMessage,
  Conversation,
  Memory,
  MemoryPatch,
  ModelConfig,
  ModelSettings,
  Project,
  SendMessageResult,
} from "./types";

export const tauriClient = {
  cancelMessage: () => invoke<void>("cancel_message"),
  listConversations: () => invoke<Conversation[]>("list_conversations"),
  listArchivedConversations: () =>
    invoke<Conversation[]>("list_archived_conversations"),
  createConversation: (title?: string, projectId?: string | null) =>
    invoke<Conversation>("create_conversation", { title, projectId }),
  archiveConversation: (conversationId: string) =>
    invoke<void>("archive_conversation", { conversationId }),
  restoreConversation: (conversationId: string) =>
    invoke<Conversation>("restore_conversation", { conversationId }),
  deleteConversation: (conversationId: string) =>
    invoke<void>("delete_conversation", { conversationId }),
  moveConversationToProject: (
    conversationId: string,
    projectId: string | null,
  ) =>
    invoke<Conversation>("move_conversation_to_project", {
      conversationId,
      projectId,
    }),
  getConversationMessages: (conversationId: string) =>
    invoke<ChatMessage[]>("get_conversation_messages", { conversationId }),
  listProjects: () => invoke<Project[]>("list_projects"),
  createProject: (name: string) => invoke<Project>("create_project", { name }),
  deleteProject: (projectId: string) =>
    invoke<void>("delete_project", { projectId }),
  renameProject: (projectId: string, name: string) =>
    invoke<Project>("rename_project", { projectId, name }),
  renameConversation: (conversationId: string, title: string) =>
    invoke<Conversation>("rename_conversation", { conversationId, title }),
  sendMessage: (
    conversationId: string | null,
    content: string,
    modelConfigId: string | null,
    projectId: string | null,
    requestId: string,
  ) =>
    invoke<SendMessageResult>("send_message", {
      conversationId,
      content,
      modelConfigId,
      projectId,
      requestId,
    }),
  listModelConfigs: () => invoke<ModelConfig[]>("list_model_configs"),
  saveModelConfig: (config: ModelConfig) =>
    invoke<ModelConfig>("save_model_config", { config }),
  deleteModelConfig: (id: string) =>
    invoke<void>("delete_model_config", { id }),
  getModelApiKey: (id: string) =>
    invoke<string | null>("get_model_api_key", { id }),
  getSystemPrompt: () => invoke<string>("get_system_prompt"),
  saveSystemPrompt: (prompt: string) =>
    invoke<void>("save_system_prompt", { prompt }),
  getModelSettings: () => invoke<ModelSettings>("get_model_settings"),
  saveModelSettings: (settings: ModelSettings) =>
    invoke<ModelSettings>("save_model_settings", { settings }),
  listMemories: (query?: string, tags?: string[], archived?: boolean) =>
    invoke<Memory[]>("list_memories", { query, tags, archived }),
  updateMemory: (id: number, patch: MemoryPatch) =>
    invoke<Memory>("update_memory", { id, patch }),
  createSavedMemory: (fact: string) =>
    invoke<Memory>("create_saved_memory", { fact }),
  deleteMemory: (id: number) => invoke<void>("delete_memory", { id }),
  runMemoryCleanup: () => invoke<number>("run_memory_cleanup"),
};
