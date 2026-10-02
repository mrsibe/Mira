import { normalizeContext } from "@earendil-works/pi-ai";
import { stream } from "@earendil-works/pi-ai/api/openai-completions";
import type { Message, Model } from "@earendil-works/pi-ai";
import type { RuntimeEvent, RuntimeRequest } from "./protocol";

// Existing configurations remain Chat Completions endpoints, even when their
// label is "openai". Provider-native APIs/catalogs need an explicit migration.
function configuredModel(
  config: RuntimeRequest["config"],
): Model<"openai-completions"> {
  const deepseek =
    config.provider.toLowerCase() === "deepseek" ||
    new URL(config.base_url).hostname === "api.deepseek.com";
  const reasoning = deepseek && /v4|reasoner/i.test(config.model);
  return {
    id: config.model,
    name: config.name,
    provider: config.provider,
    api: "openai-completions",
    baseUrl: config.base_url.replace(/\/+$/, ""),
    reasoning,
    input: ["text"],
    cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
    // Metadata only: no guessed output cap or pricing is sent to the provider.
    contextWindow: 128000,
    maxTokens: 8192,
    compat: {
      supportsStore: false,
      supportsUsageInStreaming: false,
      supportsDeveloperRole: false,
      ...(reasoning ? { thinkingFormat: "deepseek" as const } : {}),
    },
  };
}

export async function infer(
  request: RuntimeRequest,
  signal: AbortSignal,
  emit: (event: RuntimeEvent) => Promise<void>,
): Promise<void> {
  const model = configuredModel(request.config);
  const messages: Message[] = request.messages.map((message) => {
    if (message.role !== "assistant") {
      return { role: message.role, content: message.content, timestamp: 0 };
    }
    return {
      role: "assistant",
      content: [{ type: "text", text: message.content }],
      api: model.api,
      provider: model.provider,
      model: model.id,
      stopReason: "stop",
      timestamp: 0,
      usage: {
        input: 0,
        output: 0,
        cacheRead: 0,
        cacheWrite: 0,
        totalTokens: 0,
        cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
      },
    };
  });
  const events = stream(model, normalizeContext({ messages }), {
    apiKey: request.config.api_key,
    signal,
    temperature: request.temperature,
    reasoningEffort: model.reasoning ? "high" : undefined,
    maxRetries: 2,
    // Refuse longer Retry-After waits rather than outliving the host's bound.
    maxRetryDelayMs: 5000,
    timeoutMs: 45000,
    cacheRetention: "none",
  });
  for await (const event of events) {
    if (event.type === "text_delta" || event.type === "thinking_delta") {
      await emit({
        version: 1,
        id: request.id,
        type: event.type,
        delta: event.delta,
      });
    }
    if (event.type === "error") {
      // SDK error messages can include response bodies, URLs, or credentials.
      // Keep provider diagnostics private rather than forwarding raw failures.
      await emit({
        version: 1,
        id: request.id,
        type: "error",
        code: event.reason === "aborted" ? "cancelled" : "provider",
        message:
          event.reason === "aborted"
            ? "Request cancelled"
            : "Model request failed",
      });
      return;
    }
    if (event.type === "done") {
      const content = event.message.content
        .filter((block) => block.type === "text")
        .map((block) => block.text)
        .join("");
      if (event.reason === "toolUse" || !content.trim()) {
        await emit({
          version: 1,
          id: request.id,
          type: "error",
          code: "provider",
          message: "Model returned no chat reply",
        });
      } else {
        await emit({ version: 1, id: request.id, type: "done", content });
      }
      return;
    }
  }
  throw new Error("Missing terminal model event");
}
