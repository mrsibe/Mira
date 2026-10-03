import { Bot, Copy, Check, ChevronDown } from "lucide-react";
import { useCallback, useState } from "react";
import { getCurrentLocale, t } from "../i18n";
import { useAppStore } from "../store/useAppStore";
import type { ChatMessage } from "../core/types";
import { cn } from "../utils/cn";
import { MarkdownMessage } from "./MarkdownMessage";

function formatTime(iso: string): string {
  const locale = getCurrentLocale() === "zh" ? "zh-CN" : "en-US";
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return "";
  return date.toLocaleTimeString(locale, {
    hour: "2-digit",
    minute: "2-digit",
  });
}

export function MessageBubble({ message }: { message: ChatMessage }) {
  const isUser = message.role === "user";
  const isSending = useAppStore((state) => state.isSending);
  const isAssistantStreaming =
    !isUser && !message.content && !message.reasoning && isSending;
  const [copied, setCopied] = useState(false);

  const handleCopy = useCallback(() => {
    navigator.clipboard.writeText(message.content).then(() => {
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    });
  }, [message.content]);

  const timeStr = formatTime(message.created_at);

  if (isUser) {
    return (
      <div className="group relative flex justify-end">
        <div className="max-w-[78%]">
          <article className="rounded-xl bg-[var(--message-user)] px-4 py-2.5 text-sm leading-6 text-[var(--text)]">
            <MarkdownMessage content={message.content} />
          </article>
          {!isSending ? (
            <div className="mt-1 flex items-center justify-end gap-2 pr-1">
              <button
                className="rounded p-0.5 text-[var(--subtle)] opacity-0 transition hover:text-[var(--text)] group-hover:opacity-100"
                onClick={handleCopy}
                aria-label={t("common.copy")}
              >
                {copied ? (
                  <Check className="h-3.5 w-3.5" />
                ) : (
                  <Copy className="h-3.5 w-3.5" />
                )}
              </button>
            </div>
          ) : null}
        </div>
      </div>
    );
  }

  return (
    <div className="group flex gap-3">
      <div className="mt-1 flex h-7 w-7 shrink-0 items-center justify-center rounded-full border border-[var(--border)] bg-[var(--panel)]">
        <Bot className="h-4 w-4 text-[var(--text)]" />
      </div>
      <div className="min-w-0 flex-1">
        {isAssistantStreaming ? (
          <div className="flex min-h-7 items-center gap-2 text-sm text-[var(--muted)]">
            <span className="h-2 w-2 animate-pulse rounded-full bg-[var(--primary)]" />
            {t("chat.thinking")}
          </div>
        ) : (
          <div className="group relative">
            {message.reasoning ? (
              <details className="mb-2 rounded-md bg-[var(--panel-soft)]">
                <summary className="flex cursor-pointer items-center gap-1 px-3 py-1.5 text-xs font-medium text-[var(--subtle)] hover:text-[var(--text)]">
                  <ChevronDown className="h-3 w-3" />
                  {isSending ? t("chat.thinking") : t("chat.thought")}
                </summary>
                <div className="border-t border-[var(--border)] px-3 py-2 text-xs leading-6 text-[var(--muted)]">
                  <MarkdownMessage content={message.reasoning} />
                </div>
              </details>
            ) : null}
            {message.content ? (
              <article
                className={cn(
                  "text-[15px] leading-7 text-[var(--text)]",
                  "[&_p]:mb-3 [&_p:last-child]:mb-0",
                  "[&_pre]:my-3 [&_pre]:bg-[var(--code-block)] [&_pre]:p-4 [&_pre]:text-sm",
                  "[&_code]:rounded [&_code]:bg-[var(--code-inline)] [&_code]:px-1 [&_code]:py-0.5",
                  "[&_pre_code]:bg-transparent [&_pre_code]:p-0",
                )}
              >
                <MarkdownMessage content={message.content} />
              </article>
            ) : null}
            {timeStr && !isSending ? (
              <div className="mt-1 flex items-center gap-2">
                <p className="text-[11px] text-[var(--subtle)]">{timeStr}</p>
                <button
                  className="rounded p-0.5 text-[var(--subtle)] opacity-0 transition hover:text-[var(--text)] group-hover:opacity-100"
                  onClick={handleCopy}
                  aria-label={t("common.copy")}
                >
                  {copied ? (
                    <Check className="h-3.5 w-3.5" />
                  ) : (
                    <Copy className="h-3.5 w-3.5" />
                  )}
                </button>
              </div>
            ) : null}
          </div>
        )}
      </div>
    </div>
  );
}
