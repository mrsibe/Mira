import { Minus, PanelLeftClose, PanelLeftOpen, Square, X } from "lucide-react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import type { MouseEvent, ReactNode } from "react";
import { useT } from "../i18n/useT";
import { useAppStore } from "../store/useAppStore";
import { cn } from "../utils/cn";

export function WindowTitleBar() {
  const t = useT();
  const isSidebarCollapsed = useAppStore((state) => state.isSidebarCollapsed);
  const toggleSidebar = useAppStore((state) => state.toggleSidebar);
  const currentPage = useAppStore((state) => state.currentPage);
  const appWindow = getCurrentWindow();
  const SidebarIcon = isSidebarCollapsed ? PanelLeftOpen : PanelLeftClose;

  function startWindowDrag(event: MouseEvent<HTMLDivElement>) {
    if (event.button !== 0 || event.detail > 1) {
      return;
    }
    void appWindow.startDragging();
  }

  return (
    <header className="flex h-9 shrink-0 items-center border-b border-[var(--border)] bg-[var(--bg)] text-[var(--text)] select-none">
      {currentPage === "settings" ? null : (
        <button
          aria-label={
            isSidebarCollapsed
              ? t("window.expandSidebar")
              : t("window.collapseSidebar")
          }
          title={
            isSidebarCollapsed
              ? t("window.expandSidebar")
              : t("window.collapseSidebar")
          }
          className="ml-2 flex h-7 w-7 items-center justify-center rounded-md text-[var(--subtle)] transition hover:bg-[var(--hover)] hover:text-[var(--text)]"
          onClick={toggleSidebar}
        >
          <SidebarIcon className="h-4 w-4" />
        </button>
      )}

      <div
        data-tauri-drag-region
        className="h-full min-w-0 flex-1"
        onDoubleClick={() => void appWindow.toggleMaximize()}
        onMouseDown={startWindowDrag}
      />

      <WindowButton
        label={t("window.minimize")}
        onClick={() => void appWindow.minimize()}
      >
        <Minus className="h-4 w-4" />
      </WindowButton>
      <WindowButton
        label={t("window.maximize")}
        onClick={() => void appWindow.toggleMaximize()}
      >
        <Square className="h-3.5 w-3.5" />
      </WindowButton>
      <WindowButton
        label={t("window.close")}
        danger
        onClick={() => void appWindow.close()}
      >
        <X className="h-4 w-4" />
      </WindowButton>
    </header>
  );
}

function WindowButton({
  label,
  danger,
  onClick,
  children,
}: {
  label: string;
  danger?: boolean;
  onClick: () => void;
  children: ReactNode;
}) {
  return (
    <button
      aria-label={label}
      title={label}
      className={cn(
        "flex h-9 w-11 items-center justify-center text-[var(--subtle)] transition hover:bg-[var(--hover)] hover:text-[var(--text)]",
        danger && "hover:bg-[var(--danger)] hover:text-white",
      )}
      onClick={onClick}
    >
      {children}
    </button>
  );
}
