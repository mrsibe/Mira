export type Page = "chat" | "settings";

export type Role = "system" | "user" | "assistant";

export type ThemeMode = "light" | "dark" | "system";

export type Locale = "en" | "zh";

export interface Project {
  id: string;
  name: string;
  created_at: string;
  updated_at: string;
  is_archived: boolean;
}

export interface Conversation {
  id: string;
  title: string;
  project_id: string | null;
  is_archived: boolean;
  created_at: string;
  updated_at: string;
}

export interface ChatMessage {
  id: string;
  conversation_id: string;
  role: Role;
  content: string;
  created_at: string;
  reasoning?: string;
}

export interface ModelConfig {
  id: string;
  provider: string;
  name: string;
  base_url: string;
  model: string;
  api_key?: string | null;
  credential_status?: "stored" | "missing" | "error" | null;
  credential_error?: string | null;
  is_default: boolean;
  created_at?: string | null;
  updated_at?: string | null;
}

export interface ModelSettings {
  chat_model_config_id: string | null;
  background_model_config_id: string | null;
  background_model_follows_chat: boolean;
}

export interface Memory {
  id: number;
  fact: string;
  memory_type: string | null;
  importance: number;
  confidence: number;
  tags: string | null;
  source_conversation_id: string | null;
  created_at: string;
  updated_at: string;
  last_used_at: string | null;
  use_count: number;
  is_archived: boolean;
}

export interface SendMessageResult {
  conversation: Conversation;
  user_message: ChatMessage;
  assistant_message: ChatMessage | null;
}

export interface MessageStreamDelta {
  request_id: string;
  conversation_id: string;
  content: string;
  reasoning?: string;
}

export interface MemoryPatch {
  fact?: string;
  memory_type?: string | null;
  importance?: number;
  confidence?: number;
  tags?: string | null;
  is_archived?: boolean;
}
