import type { ChatMessage } from "../core/types";

export function fallbackMessage(content: string): ChatMessage {
  return {
    id: crypto.randomUUID(),
    conversation_id: "local",
    role: "assistant",
    content,
    created_at: new Date().toISOString(),
  };
}
