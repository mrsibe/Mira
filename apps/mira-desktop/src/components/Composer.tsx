import { ArrowUp, Square } from "lucide-react";
import { FormEvent, useEffect, useRef, useState } from "react";
import { useT } from "../i18n/useT";
import { useAppStore } from "../store/useAppStore";
import { Button } from "./ui/button";
import { Textarea } from "./ui/textarea";

export function Composer() {
  const t = useT();
  const [content, setContent] = useState("");
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const isSending = useAppStore((state) => state.isSending);
  const sendMessage = useAppStore((state) => state.sendMessage);
  const requestCancel = useAppStore((state) => state.requestCancel);

  function autoResize() {
    const el = textareaRef.current;
    if (el) {
      el.style.height = "auto";
      el.style.height = el.scrollHeight + "px";
    }
  }

  useEffect(() => {
    if (!content) {
      const el = textareaRef.current;
      if (el) {
        el.style.height = "auto";
      }
    }
  }, [content]);

  async function onSubmit(event: FormEvent) {
    event.preventDefault();
    const trimmed = content.trim();
    if (!trimmed || isSending) {
      return;
    }
    setContent("");
    await sendMessage(trimmed);
  }

  return (
    <form
      className="shrink-0 bg-[var(--bg)] px-5 pb-5"
      onSubmit={(event) => void onSubmit(event)}
    >
      <div className="mx-auto max-w-3xl">
        <div className="flex min-h-14 items-end gap-2 rounded-xl border border-[var(--border-strong)] bg-[var(--panel)] p-2 shadow-[var(--shadow-raised)]">
          <Textarea
            ref={textareaRef}
            className="max-h-44 min-h-10 flex-1 resize-none border-0 bg-transparent px-2 py-2 shadow-none focus:border-0 focus:ring-0"
            placeholder={t("composer.placeholder")}
            value={content}
            onChange={(event) => setContent(event.currentTarget.value)}
            onInput={() => autoResize()}
            onKeyDown={(event) => {
              if (event.key === "Enter" && !event.shiftKey) {
                event.preventDefault();
                void onSubmit(event);
              }
            }}
          />
          {isSending ? (
            <Button
              type="button"
              aria-label="Stop"
              title="Stop generating"
              size="icon"
              variant="outline"
              className="mb-0.5 h-9 w-9 shrink-0 rounded-xl border-[var(--danger)] text-[var(--danger)] hover:bg-[var(--danger)] hover:text-white"
              onClick={() => requestCancel()}
            >
              <Square className="h-4 w-4" />
            </Button>
          ) : (
            <Button
              aria-label={t("composer.send")}
              title={t("composer.send")}
              size="icon"
              className="mb-0.5 h-9 w-9 shrink-0 rounded-xl"
              disabled={!content.trim()}
            >
              <ArrowUp className="h-4 w-4" />
            </Button>
          )}
        </div>
      </div>
    </form>
  );
}
