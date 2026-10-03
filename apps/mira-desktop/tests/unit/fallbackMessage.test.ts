import { describe, expect, it } from "vitest";
import { fallbackMessage } from "../../src/store/fallbackMessage";

describe("fallbackMessage", () => {
  it("builds a local assistant message", () => {
    const message = fallbackMessage("Backend is not ready");

    expect(message.role).toBe("assistant");
    expect(message.conversation_id).toBe("local");
    expect(message.content).toBe("Backend is not ready");
    expect(message.reasoning).toBeUndefined();
    expect(Number.isNaN(Date.parse(message.created_at))).toBe(false);
  });

  it("gives each fallback message a distinct id", () => {
    expect(fallbackMessage("a").id).not.toBe(fallbackMessage("b").id);
  });
});
