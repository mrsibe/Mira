export interface RuntimeRequest {
  version: 1;
  type: "complete";
  id: string;
  config: {
    provider: string;
    name: string;
    base_url: string;
    model: string;
    api_key: string;
  };
  messages: { role: "system" | "user" | "assistant"; content: string }[];
  temperature?: number;
}

export type RuntimeEvent = { version: 1; id: string } & (
  | { type: "text_delta" | "thinking_delta"; delta: string }
  | { type: "done"; content: string }
  | {
      type: "error";
      code: "protocol" | "provider" | "cancelled";
      message: string;
    }
);

export const MAX_FRAME_BYTES = 1024 * 1024;
export const MAX_REQUEST_BYTES = 8 * MAX_FRAME_BYTES;

function record(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

export function parseRequest(value: unknown): RuntimeRequest {
  if (
    !record(value) ||
    value.version !== 1 ||
    value.type !== "complete" ||
    typeof value.id !== "string" ||
    !value.id ||
    value.id.length > 128 ||
    !record(value.config) ||
    !Array.isArray(value.messages) ||
    !value.messages.length
  ) {
    throw new Error("Invalid runtime request");
  }
  for (const field of ["provider", "name", "base_url", "model", "api_key"]) {
    const item = value.config[field];
    if (typeof item !== "string" || !item.trim()) {
      throw new Error("Invalid model configuration");
    }
  }
  if (value.config.api_key === "******") {
    throw new Error("Missing credential");
  }
  const endpoint = new URL(value.config.base_url as string);
  if (
    !["http:", "https:"].includes(endpoint.protocol) ||
    endpoint.username ||
    endpoint.password
  ) {
    throw new Error("Invalid endpoint");
  }
  for (const message of value.messages) {
    if (
      !record(message) ||
      !["system", "user", "assistant"].includes(String(message.role)) ||
      typeof message.content !== "string"
    ) {
      throw new Error("Invalid message");
    }
  }
  if (
    value.temperature !== undefined &&
    (typeof value.temperature !== "number" ||
      !Number.isFinite(value.temperature) ||
      value.temperature < 0 ||
      value.temperature > 2)
  ) {
    throw new Error("Invalid temperature");
  }
  return value as unknown as RuntimeRequest;
}
