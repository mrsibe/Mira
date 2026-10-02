import { expect, test } from "@playwright/test";
import { installTauriMock, type MockConversation } from "./tauriMock";

const CREATED_AT = "2024-01-01T00:00:00.000Z";

function conversation(id: string): MockConversation {
  return {
    id,
    title: "Existing conversation",
    project_id: null,
    is_archived: false,
    created_at: CREATED_AT,
    updated_at: CREATED_AT,
  };
}

test.describe("Mira browser smoke (mocked Tauri boundary)", () => {
  test("renders the chat shell against the mocked IPC", async ({ page }) => {
    await installTauriMock(page, {
      modelConfigs: [
        {
          id: "model-1",
          provider: "openai",
          name: "Mock provider",
          base_url: "https://example.test/v1",
          model: "mock-model",
          is_default: true,
        },
      ],
      modelSettings: {
        chat_model_config_id: "model-1",
        background_model_config_id: null,
        background_model_follows_chat: true,
      },
    });

    await page.goto("/");

    await expect(
      page.getByRole("heading", {
        name: "What would you like to talk about today?",
      }),
    ).toBeVisible();
    await expect(page.getByPlaceholder("Ask anything")).toBeVisible();
    await expect(page.getByRole("button", { name: "Minimize" })).toBeVisible();

    await page.waitForFunction(
      () =>
        window.__MIRA_TEST__?.calls.some(
          (call) => call.cmd === "list_conversations",
        ) ?? false,
    );
  });

  test("starts a new draft without persisting an empty conversation", async ({
    page,
  }) => {
    await installTauriMock(page, {
      conversations: [conversation("conv-1")],
      messagesByConversation: {
        "conv-1": [
          {
            id: "old-message",
            conversation_id: "conv-1",
            role: "assistant",
            content: "Previous reply",
            created_at: CREATED_AT,
          },
        ],
      },
    });
    await page.goto("/");
    await expect(
      page.getByText("Previous reply", { exact: true }),
    ).toBeVisible();
    await page
      .getByRole("button", { name: "New conversation", exact: true })
      .click();
    await expect(
      page.getByRole("heading", {
        name: "What would you like to talk about today?",
      }),
    ).toBeVisible();
    await expect(page.getByText("Previous reply", { exact: true })).toHaveCount(
      0,
    );
    expect(
      await page.evaluate(
        () =>
          window.__MIRA_TEST__!.calls.filter(
            (call) => call.cmd === "create_conversation",
          ).length,
      ),
    ).toBe(0);
  });

  test("switches conversations and replaces visible messages", async ({
    page,
  }) => {
    await installTauriMock(page, {
      conversations: [
        { ...conversation("conv-1"), title: "First chat" },
        { ...conversation("conv-2"), title: "Second chat" },
      ],
      messagesByConversation: {
        "conv-1": [
          {
            id: "first-message",
            conversation_id: "conv-1",
            role: "assistant",
            content: "First reply",
            created_at: CREATED_AT,
          },
        ],
        "conv-2": [
          {
            id: "second-message",
            conversation_id: "conv-2",
            role: "assistant",
            content: "Second reply",
            created_at: CREATED_AT,
          },
        ],
      },
    });
    await page.goto("/");
    await expect(page.getByText("First reply", { exact: true })).toBeVisible();
    await page.getByText("Second chat", { exact: true }).click();
    await expect(page.getByText("Second reply", { exact: true })).toBeVisible();
    await expect(page.getByText("First reply", { exact: true })).toHaveCount(0);
    expect(
      await page.evaluate(() =>
        window.__MIRA_TEST__!.calls.some(
          (call) =>
            call.cmd === "get_conversation_messages" &&
            call.args.conversationId === "conv-2",
        ),
      ),
    ).toBe(true);
  });

  test("streams an assistant reply through the mocked event bridge", async ({
    page,
  }) => {
    await installTauriMock(page, { conversations: [conversation("conv-1")] });

    await page.goto("/");
    const composer = page.getByPlaceholder("Ask anything");
    await expect(composer).toBeVisible();

    await composer.fill("Hello Mira");
    await page.getByRole("button", { name: "Send" }).click();

    await page.waitForFunction(() => window.__MIRA_TEST__?.lastSend != null);
    const send = await page.evaluate(() => window.__MIRA_TEST__!.lastSend!);
    expect(send.conversationId).toBe("conv-1");

    await page.evaluate(
      ({ requestId, conversationId }) => {
        const bridge = window.__MIRA_TEST__!;
        bridge.emit("message_stream_delta", {
          request_id: requestId,
          conversation_id: conversationId,
          content: "Hello ",
        });
        bridge.emit("message_stream_delta", {
          request_id: requestId,
          conversation_id: conversationId,
          content: "from mock",
        });
        bridge.emit("message_stream_delta", {
          request_id: requestId,
          conversation_id: conversationId,
          content: "",
          reasoning: "considering",
        });
      },
      { requestId: send.requestId, conversationId: send.conversationId! },
    );

    // The optimistic assistant bubble shows the streamed text before the IPC
    // call settles.
    await expect(page.getByText("Hello from mock")).toBeVisible();

    await page.evaluate(
      ({ requestId, conversationId }) => {
        window.__MIRA_TEST__!.resolveSend({
          conversation: {
            id: conversationId,
            title: "Hello Mira",
            project_id: null,
            is_archived: false,
            created_at: new Date().toISOString(),
            updated_at: new Date().toISOString(),
          },
          user_message: {
            id: "user-1",
            conversation_id: conversationId,
            role: "user",
            content: "Hello Mira",
            created_at: new Date().toISOString(),
          },
          assistant_message: {
            id: "assistant-1",
            conversation_id: conversationId,
            role: "assistant",
            content: "Final reply from backend",
            reasoning: "considering",
            created_at: new Date().toISOString(),
          },
          requestId,
        });
      },
      { requestId: send.requestId, conversationId: send.conversationId! },
    );

    await expect(page.getByRole("button", { name: "Send" })).toBeVisible();
    await expect(page.getByRole("button", { name: "Stop" })).toHaveCount(0);
    await expect(
      page.getByText("Final reply from backend", { exact: true }),
    ).toHaveCount(1);
    await expect(
      page.getByText("Hello from mock", { exact: true }),
    ).toHaveCount(0);
    await expect(page.getByText("Thought")).toBeVisible();
  });

  test("cancels the stream and drops late deltas", async ({ page }) => {
    await installTauriMock(page, { conversations: [conversation("conv-1")] });

    await page.goto("/");
    const composer = page.getByPlaceholder("Ask anything");
    await expect(composer).toBeVisible();

    await composer.fill("Hello Mira");
    await page.getByRole("button", { name: "Send" }).click();

    await page.waitForFunction(() => window.__MIRA_TEST__?.lastSend != null);
    const send = await page.evaluate(() => window.__MIRA_TEST__!.lastSend!);

    await page.evaluate(
      ({ requestId, conversationId }) => {
        window.__MIRA_TEST__!.emit("message_stream_delta", {
          request_id: requestId,
          conversation_id: conversationId,
          content: "partial",
        });
      },
      { requestId: send.requestId, conversationId: send.conversationId! },
    );

    const stop = page.getByRole("button", { name: "Stop" });
    await expect(stop).toBeVisible();
    await expect(page.getByText("partial")).toBeVisible();
    await stop.click();

    await page.evaluate(
      ({ requestId, conversationId }) => {
        const bridge = window.__MIRA_TEST__!;
        bridge.emit("message_stream_delta", {
          request_id: requestId,
          conversation_id: conversationId,
          content: "late chunk",
        });
        bridge.resolveSend({
          conversation: {
            id: conversationId,
            title: "Hello Mira",
            project_id: null,
            is_archived: false,
            created_at: new Date().toISOString(),
            updated_at: new Date().toISOString(),
          },
          user_message: {
            id: "user-1",
            conversation_id: conversationId,
            role: "user",
            content: "Hello Mira",
            created_at: new Date().toISOString(),
          },
          assistant_message: null,
        });
      },
      { requestId: send.requestId, conversationId: send.conversationId! },
    );

    await expect(page.getByRole("button", { name: "Send" })).toBeVisible();
    await expect(page.getByText("late chunk")).toHaveCount(0);

    expect(
      await page.evaluate(
        () =>
          window.__MIRA_TEST__!.calls.filter(
            (call) => call.cmd === "cancel_message",
          ).length,
      ),
    ).toBe(1);
  });
});
