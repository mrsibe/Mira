import { beforeEach, describe, expect, it, vi } from "vitest";

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));

vi.mock("@tauri-apps/api/core", () => ({ invoke }));

import { tauriClient } from "../../src/core/tauriClient";

describe("tauriClient command mapping", () => {
  beforeEach(() => {
    invoke.mockReset();
    invoke.mockResolvedValue(undefined);
  });

  it("maps conversation commands to their IPC argument names", async () => {
    await tauriClient.createConversation("Title", "project-1");
    expect(invoke).toHaveBeenLastCalledWith("create_conversation", {
      title: "Title",
      projectId: "project-1",
    });

    await tauriClient.archiveConversation("c1");
    expect(invoke).toHaveBeenLastCalledWith("archive_conversation", {
      conversationId: "c1",
    });

    await tauriClient.moveConversationToProject("c1", null);
    expect(invoke).toHaveBeenLastCalledWith("move_conversation_to_project", {
      conversationId: "c1",
      projectId: null,
    });

    await tauriClient.renameConversation("c1", "Renamed");
    expect(invoke).toHaveBeenLastCalledWith("rename_conversation", {
      conversationId: "c1",
      title: "Renamed",
    });
  });

  it("maps send_message stream arguments", async () => {
    await tauriClient.sendMessage("c1", "hello", "model-1", "p1", "req-1");
    expect(invoke).toHaveBeenLastCalledWith("send_message", {
      conversationId: "c1",
      content: "hello",
      modelConfigId: "model-1",
      projectId: "p1",
      requestId: "req-1",
    });
  });

  it("passes memory filters through unchanged", async () => {
    await tauriClient.listMemories("query", ["tag"], false);
    expect(invoke).toHaveBeenLastCalledWith("list_memories", {
      query: "query",
      tags: ["tag"],
      archived: false,
    });

    await tauriClient.updateMemory(7, { importance: 9 });
    expect(invoke).toHaveBeenLastCalledWith("update_memory", {
      id: 7,
      patch: { importance: 9 },
    });
  });

  it("resolves with the mocked IPC result", async () => {
    invoke.mockResolvedValueOnce(3);
    await expect(tauriClient.runMemoryCleanup()).resolves.toBe(3);
    expect(invoke).toHaveBeenLastCalledWith("run_memory_cleanup");

    await tauriClient.cancelMessage();
    expect(invoke).toHaveBeenLastCalledWith("cancel_message");
  });
});
