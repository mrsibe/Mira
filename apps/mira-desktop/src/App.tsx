import { Suspense, lazy, useEffect, useState, type ReactNode } from "react";
import { check } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";
import { WindowTitleBar } from "./components/WindowTitleBar";
import { Button } from "./components/ui/button";
import { useAppStore } from "./store/useAppStore";
import { applyThemeMode } from "./utils/theme";

const ChatPage = lazy(() =>
  import("./pages/ChatPage").then((module) => ({ default: module.ChatPage })),
);
const SettingsPage = lazy(() =>
  import("./pages/SettingsPage").then((module) => ({
    default: module.SettingsPage,
  })),
);

export default function App() {
  const currentPage = useAppStore((state) => state.currentPage);
  const bootstrap = useAppStore((state) => state.bootstrap);
  const themeMode = useAppStore((state) => state.themeMode);
  const [updateVersion, setUpdateVersion] = useState("");
  const [showUpdate, setShowUpdate] = useState(false);
  const [showInstallDone, setShowInstallDone] = useState(false);
  const [isDownloading, setIsDownloading] = useState(false);

  useEffect(() => {
    void bootstrap();
  }, [bootstrap]);

  useEffect(() => {
    applyThemeMode(themeMode);
    if (themeMode !== "system") {
      return;
    }
    const media = window.matchMedia("(prefers-color-scheme: dark)");
    const onChange = () => applyThemeMode("system");
    media.addEventListener("change", onChange);
    return () => media.removeEventListener("change", onChange);
  }, [themeMode]);

  useEffect(() => {
    let cancelled = false;
    async function autoCheck() {
      try {
        const update = await check();
        if (cancelled) return;
        if (update) {
          setUpdateVersion(update.version);
          setShowUpdate(true);
        }
      } catch {
        // manifest not ready yet, skip silently
      }
    }
    // delay check slightly to let bootstrap finish
    const timer = setTimeout(() => void autoCheck(), 3000);
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, []);

  async function handleInstall() {
    setIsDownloading(true);
    try {
      const update = await check();
      if (!update) return;
      await update.downloadAndInstall();
      setShowUpdate(false);
      setShowInstallDone(true);
    } catch {
      setIsDownloading(false);
    }
  }

  const page = currentPage === "settings" ? <SettingsPage /> : <ChatPage />;

  return (
    <AppFrame>
      <Suspense fallback={<PageFallback />}>{page}</Suspense>

      {showUpdate ? (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/30 px-4">
          <div className="w-full max-w-sm rounded-xl border border-[var(--border)] bg-[var(--panel)] p-4 shadow-[var(--shadow-modal)]">
            <h2 className="text-sm font-semibold text-[var(--text)]">
              Update available
            </h2>
            <p className="mt-2 text-xs leading-5 text-[var(--subtle)]">
              Version {updateVersion} is ready to install.
            </p>
            <div className="mt-4 flex justify-end gap-2">
              <Button
                type="button"
                variant="ghost"
                disabled={isDownloading}
                onClick={() => setShowUpdate(false)}
              >
                Later
              </Button>
              <Button
                type="button"
                disabled={isDownloading}
                onClick={() => void handleInstall()}
              >
                {isDownloading ? "Downloading..." : "Update now"}
              </Button>
            </div>
          </div>
        </div>
      ) : null}
      {showInstallDone ? (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/30 px-4">
          <div className="w-full max-w-sm rounded-xl border border-[var(--border)] bg-[var(--panel)] p-4 shadow-[var(--shadow-modal)]">
            <h2 className="text-sm font-semibold text-[var(--text)]">
              Update complete
            </h2>
            <p className="mt-2 text-xs leading-5 text-[var(--subtle)]">
              The update has been installed. Please restart the app to apply it.
            </p>
            <div className="mt-4 flex justify-end gap-2">
              <Button
                type="button"
                variant="ghost"
                onClick={() => setShowInstallDone(false)}
              >
                Later
              </Button>
              <Button type="button" onClick={() => void relaunch()}>
                Restart now
              </Button>
            </div>
          </div>
        </div>
      ) : null}
    </AppFrame>
  );
}

function AppFrame({ children }: { children: ReactNode }) {
  return (
    <div className="flex h-dvh max-h-dvh flex-col overflow-hidden bg-[var(--bg)] text-[var(--text)]">
      <WindowTitleBar />
      <div className="min-h-0 flex-1 overflow-hidden">{children}</div>
    </div>
  );
}

function PageFallback() {
  return <main className="h-full overflow-hidden bg-[var(--bg)]" />;
}
