import {
  useState,
  type CSSProperties,
  type PointerEvent as ReactPointerEvent,
  type ReactNode,
} from "react";
import { ConversationList } from "./ConversationList";
import { useT } from "../i18n/useT";
import { useAppStore } from "../store/useAppStore";
import { cn } from "../utils/cn";

const MIN_SIDEBAR_WIDTH = 220;
const MAX_SIDEBAR_WIDTH = 360;

export function AppShell({ children }: { children: ReactNode }) {
  const t = useT();
  const isSidebarCollapsed = useAppStore((state) => state.isSidebarCollapsed);
  const [sidebarWidth, setSidebarWidth] = useState(280);

  function startResize(event: ReactPointerEvent<HTMLDivElement>) {
    event.preventDefault();
    const startX = event.clientX;
    const startWidth = sidebarWidth;
    function onPointerMove(moveEvent: PointerEvent) {
      const nextWidth = Math.min(
        MAX_SIDEBAR_WIDTH,
        Math.max(MIN_SIDEBAR_WIDTH, startWidth + moveEvent.clientX - startX),
      );
      setSidebarWidth(nextWidth);
    }
    function onPointerUp() {
      window.removeEventListener("pointermove", onPointerMove);
      window.removeEventListener("pointerup", onPointerUp);
    }
    window.addEventListener("pointermove", onPointerMove);
    window.addEventListener("pointerup", onPointerUp);
  }

  return (
    <div className="h-full overflow-hidden bg-[var(--bg)] text-[var(--text)]">
      <div
        className={cn(
          "grid h-full min-h-0",
          isSidebarCollapsed
            ? "grid-cols-[minmax(0,1fr)]"
            : "grid-cols-[var(--sidebar-width)_4px_minmax(0,1fr)]",
        )}
        style={{ "--sidebar-width": `${sidebarWidth}px` } as CSSProperties}
      >
        {isSidebarCollapsed ? null : <ConversationList />}
        {isSidebarCollapsed ? null : (
          <div
            aria-label={t("appShell.resizeSidebar")}
            className="cursor-col-resize bg-transparent transition hover:bg-[var(--border-strong)]"
            role="separator"
            onPointerDown={startResize}
          />
        )}
        <div className="min-h-0 min-w-0 overflow-hidden">{children}</div>
      </div>
    </div>
  );
}
