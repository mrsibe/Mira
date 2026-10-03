import { Sparkles } from "lucide-react";
import { AppShell } from "../components/AppShell";
import { Composer } from "../components/Composer";
import { MessageBubble } from "../components/MessageBubble";
import { useT } from "../i18n/useT";
import { useAppStore } from "../store/useAppStore";
import { cn } from "../utils/cn";

export function ChatPage() {
  const t = useT();
  const messages = useAppStore((state) => state.messages);
  const conversations = useAppStore((state) => state.conversations);
  const activeConversationId = useAppStore(
    (state) => state.activeConversationId,
  );
  const activeProjectId = useAppStore((state) => state.activeProjectId);
  const selectConversation = useAppStore((state) => state.selectConversation);

  const projectConversations =
    activeProjectId && messages.length === 0
      ? conversations.filter(
          (c) =>
            c.project_id === activeProjectId && c.id !== activeConversationId,
        )
      : [];

  return (
    <AppShell>
      <main className="flex h-full min-h-0 min-w-0 flex-col overflow-hidden bg-[var(--bg)]">
        <div className="min-h-0 flex-1 overflow-y-auto px-5">
          <div className="mx-auto flex min-h-full w-full max-w-3xl flex-col">
            {messages.length === 0 ? (
              <div className="flex flex-1 flex-col items-center justify-center pb-24 text-center">
                <div className="mb-5 flex h-11 w-11 items-center justify-center rounded-xl border border-[var(--border)] bg-[var(--panel)] shadow-sm">
                  <Sparkles className="h-5 w-5 text-[var(--text)]" />
                </div>
                <h1 className="text-2xl font-semibold tracking-normal text-[var(--text)]">
                  {t("chat.emptyHeading")}
                </h1>
                {projectConversations.length > 0 ? (
                  <div className="mt-8 w-full max-w-2xl text-left">
                    <div className="mb-2 text-[13px] font-medium text-[var(--subtle)]">
                      {t("conversationList.projectConversations")}
                    </div>
                    <div className="space-y-1">
                      {projectConversations.map((conversation) => (
                        <button
                          key={conversation.id}
                          className={cn(
                            "flex w-full items-center gap-2 rounded-md px-3 py-2 text-left text-sm transition",
                            "text-[var(--text)] hover:bg-[var(--hover)]",
                          )}
                          onClick={() =>
                            void selectConversation(conversation.id)
                          }
                        >
                          <span className="min-w-0 flex-1 truncate">
                            {conversation.title}
                          </span>
                        </button>
                      ))}
                    </div>
                  </div>
                ) : null}
              </div>
            ) : (
              <div className="flex flex-col gap-6 pb-32 pt-4">
                {messages.map((message) => (
                  <MessageBubble key={message.id} message={message} />
                ))}
              </div>
            )}
          </div>
        </div>

        <Composer key={activeConversationId} />
      </main>
    </AppShell>
  );
}
