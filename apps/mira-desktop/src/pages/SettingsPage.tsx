import {
  ArrowLeft,
  Archive,
  Bot,
  Check,
  ExternalLink,
  Eye,
  EyeOff,
  Globe,
  Info,
  KeyRound,
  Laptop,
  Moon,
  Palette,
  Pencil,
  Plus,
  Save,
  Search,
  Server,
  Sun,
  Trash2,
  Undo2,
  X,
} from "lucide-react";
import { FormEvent, useEffect, useMemo, useState } from "react";
import { getVersion } from "@tauri-apps/api/app";
import { check } from "@tauri-apps/plugin-updater";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { openUrl } from "@tauri-apps/plugin-opener";
import { AlertDialog } from "../components/ui/alert-dialog";
import { Badge } from "../components/ui/badge";
import { Button } from "../components/ui/button";
import { Input } from "../components/ui/input";
import { Textarea } from "../components/ui/textarea";
import type {
  Conversation,
  Locale,
  Memory,
  ModelConfig,
  ModelSettings,
  ThemeMode,
} from "../core/types";
import { tauriClient } from "../core/tauriClient";
import { useT } from "../i18n/useT";
import { useAppStore } from "../store/useAppStore";
import { cn } from "../utils/cn";

type SettingsSection =
  | "appearance"
  | "providers"
  | "models"
  | "memories"
  | "archive"
  | "about"
  | "search";
type DeleteTarget =
  | { kind: "memory"; id: number }
  | { kind: "conversation"; conversation: Conversation }
  | { kind: "provider"; id: string; name: string };

const themeOptionKeys: Array<{
  value: ThemeMode;
  labelKey:
    | "settings.appearance.themeLight"
    | "settings.appearance.themeDark"
    | "settings.appearance.themeSystem";
  icon: typeof Sun;
}> = [
  { value: "light", labelKey: "settings.appearance.themeLight", icon: Sun },
  { value: "dark", labelKey: "settings.appearance.themeDark", icon: Moon },
  {
    value: "system",
    labelKey: "settings.appearance.themeSystem",
    icon: Laptop,
  },
];

const localeOptionKeys: Array<{
  value: Locale;
  labelKey:
    | "settings.appearance.langEnglish"
    | "settings.appearance.langChinese";
}> = [
  { value: "en", labelKey: "settings.appearance.langEnglish" },
  { value: "zh", labelKey: "settings.appearance.langChinese" },
];

export function SettingsPage() {
  const t = useT();
  const modelConfigs = useAppStore((state) => state.modelConfigs);
  const modelSettings = useAppStore((state) => state.modelSettings);
  const saveModelConfig = useAppStore((state) => state.saveModelConfig);
  const deleteModelConfig = useAppStore((state) => state.deleteModelConfig);
  const saveModelSettings = useAppStore((state) => state.saveModelSettings);
  const themeMode = useAppStore((state) => state.themeMode);
  const setThemeMode = useAppStore((state) => state.setThemeMode);
  const locale = useAppStore((state) => state.locale);
  const setLocale = useAppStore((state) => state.setLocale);
  const memories = useAppStore((state) => state.memories);
  const loadMemories = useAppStore((state) => state.loadMemories);
  const createSavedMemory = useAppStore((state) => state.createSavedMemory);
  const updateMemory = useAppStore((state) => state.updateMemory);
  const deleteMemory = useAppStore((state) => state.deleteMemory);
  const archivedConversations = useAppStore(
    (state) => state.archivedConversations,
  );
  const restoreConversation = useAppStore((state) => state.restoreConversation);
  const deleteConversation = useAppStore((state) => state.deleteConversation);
  const setPage = useAppStore((state) => state.setPage);
  const [section, setSection] = useState<SettingsSection>("providers");
  const [draftId, setDraftId] = useState(modelConfigs[0]?.id ?? "");
  const selected = modelConfigs.find((config) => config.id === draftId);
  const [draft, setDraft] = useState<ModelConfig | null>(selected ?? null);
  const isDraftNew = !modelConfigs.some((config) => config.id === draftId);
  const [showSavedHint, setShowSavedHint] = useState(false);
  const [providerError, setProviderError] = useState<string | null>(null);
  const [newMemoryFact, setNewMemoryFact] = useState("");
  const [isAddMemoryOpen, setIsAddMemoryOpen] = useState(false);
  const [editingMemoryId, setEditingMemoryId] = useState<number | null>(null);
  const [editingFact, setEditingFact] = useState("");
  const [archiveQuery, setArchiveQuery] = useState("");
  const [deleteTarget, setDeleteTarget] = useState<DeleteTarget | null>(null);
  const [appVersion, setAppVersion] = useState("");
  const [updateState, setUpdateState] = useState<
    "idle" | "checking" | "available" | "downloading" | "done" | "error"
  >("idle");
  const [updateVersion, setUpdateVersion] = useState("");
  const [updateError, setUpdateError] = useState("");
  const [showProviderKey, setShowProviderKey] = useState(false);
  const [revealedKey, setRevealedKey] = useState<string | null>(null);
  const [systemPrompt, setSystemPrompt] = useState("");
  const [systemPromptSaved, setSystemPromptSaved] = useState(false);

  useEffect(() => {
    tauriClient.getSystemPrompt().then((p) => setSystemPrompt(p));
  }, []);

  useEffect(() => {
    void getVersion().then(setAppVersion);
  }, []);

  async function checkForUpdates() {
    setUpdateState("checking");
    setUpdateError("");
    try {
      const update = await check();
      if (update) {
        setUpdateVersion(update.version);
        setUpdateState("available");
      } else {
        setUpdateState("idle");
      }
    } catch {
      setUpdateState("idle");
    }
  }

  async function installUpdate() {
    setUpdateState("downloading");
    setUpdateError("");
    try {
      const update = await check();
      if (!update) {
        setUpdateState("idle");
        return;
      }
      await update.downloadAndInstall();
      setUpdateState("done");
    } catch (error) {
      setUpdateError(String(error));
      setUpdateState("error");
    }
  }

  function closeForRestart() {
    void getCurrentWindow().close();
  }

  async function saveSystemPrompt() {
    await tauriClient.saveSystemPrompt(systemPrompt);
    setSystemPromptSaved(true);
    window.setTimeout(() => setSystemPromptSaved(false), 2000);
  }

  useEffect(() => {
    if (!draftId && modelConfigs[0]) {
      setDraftId(modelConfigs[0].id);
    }
  }, [draftId, modelConfigs]);

  useEffect(() => {
    if (selected) {
      setDraft(selected);
    }
  }, [selected]);

  useEffect(() => {
    setProviderError(null);
    setShowSavedHint(false);
    setRevealedKey(null);
  }, [draftId]);

  useEffect(() => {
    void loadMemories();
  }, [loadMemories]);

  const filteredArchivedConversations = useMemo(() => {
    const query = archiveQuery.trim().toLowerCase();
    if (!query) {
      return archivedConversations;
    }
    return archivedConversations.filter((conversation) =>
      conversation.title.toLowerCase().includes(query),
    );
  }, [archivedConversations, archiveQuery]);
  const dateLocale = locale === "zh" ? "zh-CN" : "en-US";

  function updateDraft(patch: Partial<ModelConfig>) {
    setDraft((current) => (current ? { ...current, ...patch } : current));
  }

  function startAddProvider() {
    const id = `custom-${Date.now()}`;
    const config: ModelConfig = {
      id,
      provider: "custom",
      name: "",
      base_url: "",
      model: "",
      api_key: null,
      is_default: false,
    };
    setSection("providers");
    setDraftId(id);
    setDraft(config);
  }

  function cancelAddProvider() {
    const fallback = modelConfigs[0]?.id ?? "";
    setDraftId(fallback);
    setDraft(modelConfigs[0] ?? null);
  }

  function deriveProvider(name: string): string {
    const slug = name
      .trim()
      .toLowerCase()
      .replace(/[^a-z0-9]+/g, "-")
      .replace(/^-+|-+$/g, "");
    return slug || "custom";
  }

  async function handleSaveProvider(event: FormEvent) {
    event.preventDefault();
    if (!draft) {
      return;
    }
    setProviderError(null);
    const provider = isDraftNew ? deriveProvider(draft.name) : draft.provider;
    const toSave: ModelConfig = { ...draft, provider };
    try {
      await saveModelConfig(toSave);
      setShowSavedHint(true);
      window.setTimeout(() => setShowSavedHint(false), 2000);
    } catch (error) {
      setProviderError(String(error));
    }
  }

  function confirmDeleteProvider() {
    if (!draft || isDraftNew) {
      return;
    }
    setDeleteTarget({
      kind: "provider",
      id: draft.id,
      name: draft.name,
    });
  }

  async function runDeleteProvider(id: string) {
    setProviderError(null);
    try {
      await deleteModelConfig(id);
      const remaining = useAppStore.getState().modelConfigs;
      const nextId = remaining[0]?.id ?? "";
      setDraftId(nextId);
      setDraft(remaining[0] ?? null);
    } catch (error) {
      setProviderError(String(error));
    }
  }

  async function submitSavedMemory(event: FormEvent) {
    event.preventDefault();
    const trimmed = newMemoryFact.trim();
    if (!trimmed) {
      return;
    }
    setNewMemoryFact("");
    setIsAddMemoryOpen(false);
    await createSavedMemory(trimmed);
  }

  async function saveEditedMemory(id: number) {
    const trimmed = editingFact.trim();
    if (!trimmed) {
      return;
    }
    await updateMemory(id, { fact: trimmed });
    setEditingMemoryId(null);
    setEditingFact("");
  }

  function confirmDeleteMemory(id: number) {
    setDeleteTarget({ kind: "memory", id });
  }

  function confirmDeleteConversation(conversation: Conversation) {
    setDeleteTarget({ kind: "conversation", conversation });
  }

  function runDeleteTarget() {
    if (!deleteTarget) {
      return;
    }
    if (deleteTarget.kind === "memory") {
      void deleteMemory(deleteTarget.id);
    } else if (deleteTarget.kind === "conversation") {
      void deleteConversation(deleteTarget.conversation.id);
    } else {
      void runDeleteProvider(deleteTarget.id);
    }
  }

  return (
    <>
      <main className="grid h-full min-w-0 grid-cols-[260px_minmax(0,1fr)] overflow-hidden bg-[var(--bg)] text-[var(--text)]">
        <aside className="flex min-h-0 flex-col border-r border-[var(--border)] bg-[var(--panel-soft)] px-3 py-4">
          <div className="mb-5 flex items-center justify-between px-2">
            <h1 className="text-base font-semibold">{t("settings.title")}</h1>
            <button
              aria-label={t("settings.backToChat")}
              title={t("settings.backToChat")}
              className="flex h-8 w-8 items-center justify-center rounded-md text-[var(--muted)] transition hover:bg-[var(--hover)] hover:text-[var(--text)]"
              onClick={() => setPage("chat")}
            >
              <ArrowLeft className="h-4 w-4" />
            </button>
          </div>

          <div className="space-y-1">
            <SettingsNavButton
              active={section === "appearance"}
              icon={Palette}
              label={t("settings.navAppearance")}
              onClick={() => setSection("appearance")}
            />
            <SettingsNavButton
              active={section === "providers"}
              icon={Server}
              label={t("settings.navProviders")}
              onClick={() => setSection("providers")}
            />
            <SettingsNavButton
              active={section === "models"}
              icon={Bot}
              label={t("settings.navModels")}
              onClick={() => setSection("models")}
            />
            <SettingsNavButton
              active={section === "memories"}
              icon={KeyRound}
              label={t("settings.navMemories")}
              onClick={() => setSection("memories")}
            />
            <SettingsNavButton
              active={section === "archive"}
              icon={Archive}
              label={t("settings.navArchive")}
              count={archivedConversations.length}
              onClick={() => setSection("archive")}
            />
            <SettingsNavButton
              active={section === "about"}
              icon={Info}
              label={t("settings.navAbout")}
              onClick={() => setSection("about")}
            />
          </div>
        </aside>

        <section className="min-h-0 overflow-y-auto">
          {section === "appearance"
            ? renderAppearance(
                themeMode,
                setThemeMode,
                locale,
                setLocale,
                t,
                systemPrompt,
                setSystemPrompt,
                systemPromptSaved,
                saveSystemPrompt,
              )
            : null}
          {section === "providers"
            ? renderProviders({
                t,
                configs: modelConfigs,
                draft,
                draftId,
                setDraftId,
                updateDraft,
                isDraftNew,
                showSavedHint,
                showProviderKey,
                setShowProviderKey,
                revealedKey,
                setRevealedKey,
                providerError,
                onSubmit: handleSaveProvider,
                onAdd: startAddProvider,
                onCancelAdd: cancelAddProvider,
                onDelete: confirmDeleteProvider,
              })
            : null}
          {section === "models"
            ? renderModels({
                t,
                configs: modelConfigs,
                settings: modelSettings,
                saveModelSettings,
              })
            : null}
          {section === "memories"
            ? renderMemories({
                t,
                memories,
                newMemoryFact,
                isAddMemoryOpen,
                editingMemoryId,
                editingFact,
                setNewMemoryFact,
                setIsAddMemoryOpen,
                setEditingMemoryId,
                setEditingFact,
                submitSavedMemory,
                saveEditedMemory,
                confirmDeleteMemory,
              })
            : null}
          {section === "archive"
            ? renderArchive({
                t,
                dateLocale,
                query: archiveQuery,
                setQuery: setArchiveQuery,
                conversations: filteredArchivedConversations,
                restoreConversation,
                confirmDeleteConversation,
              })
            : null}
          {section === "about" ? (
            <AboutSection
              t={t}
              version={appVersion}
              actions={{
                updateState,
                updateVersion,
                updateError,
                checkForUpdates,
                installUpdate,
                closeForRestart,
              }}
            />
          ) : null}
        </section>
      </main>
      <AlertDialog
        open={deleteTarget !== null}
        title={
          deleteTarget?.kind === "conversation"
            ? t("settings.deleteDialogConversationTitle")
            : deleteTarget?.kind === "provider"
              ? t("settings.providers.deleteDialogTitle")
              : t("settings.deleteDialogMemoryTitle")
        }
        description={
          deleteTarget?.kind === "conversation"
            ? t("settings.deleteDialogConversationDesc", {
                title: deleteTarget.conversation.title,
              })
            : deleteTarget?.kind === "provider"
              ? t("settings.providers.deleteDialogDesc", {
                  name: deleteTarget.name,
                })
              : t("settings.deleteDialogMemoryDesc")
        }
        confirmLabel={
          deleteTarget?.kind === "provider"
            ? t("settings.providers.deleteDialogConfirm")
            : t("settings.deleteDialogConfirm")
        }
        onConfirm={runDeleteTarget}
        onOpenChange={(open) => {
          if (!open) {
            setDeleteTarget(null);
          }
        }}
      />
    </>
  );
}

function SettingsNavButton({
  active,
  icon: Icon,
  label,
  count,
  onClick,
}: {
  active: boolean;
  icon: typeof Sun;
  label: string;
  count?: number;
  onClick: () => void;
}) {
  return (
    <button
      className={cn(
        "flex h-10 w-full items-center gap-3 rounded-md px-3 text-sm transition",
        active
          ? "bg-[var(--active)] text-[var(--text)]"
          : "text-[var(--text)] hover:bg-[var(--hover)]",
      )}
      onClick={onClick}
    >
      <Icon className="h-4 w-4 text-[var(--subtle)]" />
      <span className="min-w-0 flex-1 text-left">{label}</span>
      {typeof count === "number" ? (
        <span className="rounded-full bg-[var(--panel)] px-2 py-0.5 text-xs text-[var(--subtle)]">
          {count}
        </span>
      ) : null}
    </button>
  );
}

function renderAppearance(
  themeMode: ThemeMode,
  setThemeMode: (themeMode: ThemeMode) => void,
  locale: Locale,
  setLocale: (locale: Locale) => void,
  t: ReturnType<typeof useT>,
  systemPrompt: string,
  setSystemPrompt: (v: string) => void,
  systemPromptSaved: boolean,
  saveSystemPrompt: () => Promise<void>,
) {
  return (
    <div className="mx-auto flex min-h-full w-full max-w-5xl flex-col px-8 py-8">
      <header className="mb-8">
        <h2 className="text-2xl font-semibold">
          {t("settings.appearance.title")}
        </h2>
        <p className="mt-2 text-sm text-[var(--subtle)]">
          {t("settings.appearance.description")}
        </p>
      </header>
      <div className="inline-flex w-fit rounded-md border border-[var(--border-strong)] bg-[var(--panel-soft)] p-1">
        {themeOptionKeys.map((option) => {
          const Icon = option.icon;
          return (
            <button
              key={option.value}
              type="button"
              className={cn(
                "inline-flex h-10 items-center gap-2 rounded-md px-4 text-sm text-[var(--muted)] transition",
                themeMode === option.value &&
                  "bg-[var(--panel)] text-[var(--text)] shadow-sm",
              )}
              onClick={() => setThemeMode(option.value)}
            >
              <Icon className="h-4 w-4" />
              {t(option.labelKey)}
            </button>
          );
        })}
      </div>

      <div className="mt-10">
        <h3 className="mb-3 text-sm font-semibold text-[var(--text)]">
          <span className="inline-flex items-center gap-2">
            <Globe className="h-4 w-4 text-[var(--subtle)]" />
            {t("settings.appearance.language")}
          </span>
        </h3>
        <p className="mb-4 text-sm text-[var(--subtle)]">
          {t("settings.appearance.languageDescription")}
        </p>
        <div className="inline-flex w-fit rounded-md border border-[var(--border-strong)] bg-[var(--panel-soft)] p-1">
          {localeOptionKeys.map((option) => (
            <button
              key={option.value}
              type="button"
              className={cn(
                "inline-flex h-10 items-center gap-2 rounded-md px-4 text-sm text-[var(--muted)] transition",
                locale === option.value &&
                  "bg-[var(--panel)] text-[var(--text)] shadow-sm",
              )}
              onClick={() => setLocale(option.value)}
            >
              {t(option.labelKey)}
            </button>
          ))}
        </div>
      </div>

      <div className="mt-10 max-w-3xl">
        <h3 className="mb-3 text-sm font-semibold text-[var(--text)]">
          {t("settings.appearance.systemPrompt")}
        </h3>
        <p className="mb-4 text-sm text-[var(--subtle)]">
          {t("settings.appearance.systemPromptDesc")}
        </p>
        <Textarea
          className="min-h-[100px]"
          placeholder={t("settings.appearance.systemPromptPlaceholder")}
          value={systemPrompt}
          onChange={(e) => setSystemPrompt(e.currentTarget.value)}
        />
        <div className="mt-3 flex items-center gap-2">
          <Button onClick={() => void saveSystemPrompt()}>
            <Save className="h-4 w-4" />
            {t("common.save")}
          </Button>
          {systemPromptSaved ? (
            <span className="flex items-center gap-1 text-xs text-[var(--primary)]">
              <Check className="h-3.5 w-3.5" />
              {t("settings.providers.savedHint")}
            </span>
          ) : null}
        </div>
      </div>
    </div>
  );
}

function renderProviders({
  t,
  configs,
  draft,
  draftId,
  setDraftId,
  updateDraft,
  isDraftNew,
  showSavedHint,
  showProviderKey,
  setShowProviderKey,
  revealedKey,
  setRevealedKey,
  providerError,
  onSubmit,
  onAdd,
  onCancelAdd,
  onDelete,
}: {
  t: ReturnType<typeof useT>;
  configs: ModelConfig[];
  draft: ModelConfig | null;
  draftId: string;
  setDraftId: (id: string) => void;
  updateDraft: (patch: Partial<ModelConfig>) => void;
  isDraftNew: boolean;
  showSavedHint: boolean;
  showProviderKey: boolean;
  setShowProviderKey: React.Dispatch<React.SetStateAction<boolean>>;
  revealedKey: string | null;
  setRevealedKey: React.Dispatch<React.SetStateAction<string | null>>;
  providerError: string | null;
  onSubmit: (event: FormEvent) => Promise<void>;
  onAdd: () => void;
  onCancelAdd: () => void;
  onDelete: () => void;
}) {
  const providerItems =
    draft && !configs.some((config) => config.id === draft.id)
      ? [draft, ...configs]
      : configs;

  return (
    <div className="grid h-full min-h-0 grid-cols-[250px_minmax(0,1fr)]">
      <aside className="min-h-0 border-r border-[var(--border)] bg-[var(--panel)] px-3 py-4">
        <div className="mb-3 flex items-center justify-between px-2">
          <span className="text-xs font-medium text-[var(--subtle)]">
            {t("settings.providers.asideLabel")}
          </span>
          <button
            aria-label={t("settings.providers.add")}
            title={t("settings.providers.add")}
            className="rounded p-1 text-[var(--subtle)] hover:bg-[var(--hover)] hover:text-[var(--text)]"
            onClick={onAdd}
          >
            <Plus className="h-4 w-4" />
          </button>
        </div>
        <div className="space-y-1">
          {providerItems.length === 0 ? (
            <div className="px-2 py-3 text-xs leading-5 text-[var(--subtle)]">
              {t("settings.providers.empty")}
            </div>
          ) : (
            providerItems.map((config) => (
              <button
                key={config.id}
                className={cn(
                  "w-full rounded-md px-3 py-2 text-left text-sm transition hover:bg-[var(--hover)]",
                  draftId === config.id
                    ? "bg-[var(--active)] text-[var(--text)]"
                    : "text-[var(--text)]",
                )}
                onClick={() => setDraftId(config.id)}
              >
                <span className="block truncate font-medium">
                  {config.name || t("settings.providers.customDefaultName")}
                </span>
                <span className="block truncate text-xs text-[var(--subtle)]">
                  {config.model || "—"}
                </span>
              </button>
            ))
          )}
        </div>
      </aside>

      <div className="min-h-0 overflow-y-auto px-8 py-8">
        <header className="mb-8">
          <h2 className="text-2xl font-semibold">
            {t("settings.providers.title")}
          </h2>
          <p className="mt-2 text-sm text-[var(--subtle)]">
            {t("settings.providers.description")}
          </p>
        </header>

        {draft ? (
          <form
            className="grid max-w-3xl gap-5"
            onSubmit={(event) => void onSubmit(event)}
          >
            <label className="block text-sm font-medium">
              {t("settings.providers.name")}
              <Input
                className="mt-2"
                placeholder={t("settings.providers.namePlaceholder")}
                value={draft.name}
                onChange={(event) =>
                  updateDraft({ name: event.currentTarget.value })
                }
              />
            </label>
            <label className="block text-sm font-medium">
              {t("settings.providers.baseUrl")}
              <Input
                className="mt-2"
                placeholder={t("settings.providers.baseUrlPlaceholder")}
                value={draft.base_url}
                onChange={(event) =>
                  updateDraft({ base_url: event.currentTarget.value })
                }
              />
            </label>
            <label className="block text-sm font-medium">
              {t("settings.providers.model")}
              <Input
                className="mt-2"
                placeholder={t("settings.providers.modelPlaceholder")}
                value={draft.model}
                onChange={(event) =>
                  updateDraft({ model: event.currentTarget.value })
                }
              />
            </label>
            <div className="block text-sm font-medium">
              {t("settings.providers.apiKey")}
              <div className="relative mt-2">
                <Input
                  className="pr-9"
                  type={showProviderKey ? "text" : "password"}
                  placeholder={t("settings.providers.apiKeyPlaceholderEmpty")}
                  value={
                    draft.credential_status === "stored" && !showProviderKey
                      ? (draft.api_key ?? "******")
                      : (revealedKey ?? draft.api_key ?? "")
                  }
                  onChange={(event) =>
                    updateDraft({ api_key: event.currentTarget.value || null })
                  }
                />
                <button
                  type="button"
                  className="absolute right-2 top-1/2 -translate-y-1/2 rounded p-1 text-[var(--subtle)] hover:bg-[var(--hover)] hover:text-[var(--text)]"
                  onClick={async () => {
                    if (
                      !showProviderKey &&
                      draft?.credential_status === "stored"
                    ) {
                      try {
                        const realKey = await tauriClient.getModelApiKey(
                          draft.id,
                        );
                        if (realKey) setRevealedKey(realKey);
                      } catch {
                        // fallback to masked
                      }
                    }
                    setShowProviderKey(!showProviderKey);
                  }}
                >
                  {showProviderKey ? (
                    <EyeOff className="h-4 w-4" />
                  ) : (
                    <Eye className="h-4 w-4" />
                  )}
                </button>
              </div>
            </div>

            {providerError ? (
              <div className="rounded-md border border-[var(--danger)] bg-[var(--panel-soft)] px-3 py-2 text-xs text-[var(--danger)]">
                {providerError}
              </div>
            ) : null}

            <div className="flex flex-wrap items-center gap-2">
              {isDraftNew ? (
                <>
                  <Button type="button" variant="ghost" onClick={onCancelAdd}>
                    {t("settings.providers.cancel")}
                  </Button>
                  <Button type="submit">
                    <Plus className="h-4 w-4" />
                    {t("settings.providers.addProvider")}
                  </Button>
                </>
              ) : (
                <>
                  <Button
                    type="button"
                    variant="ghost"
                    className="text-[var(--danger)]"
                    onClick={onDelete}
                  >
                    <Trash2 className="h-4 w-4" />
                    {t("settings.providers.deleteProvider")}
                  </Button>
                  <Button type="submit">
                    <Save className="h-4 w-4" />
                    {t("settings.providers.saveChanges")}
                  </Button>
                  {showSavedHint ? (
                    <span className="inline-flex items-center gap-1 text-xs text-[var(--primary)]">
                      <Check className="h-3.5 w-3.5" />
                      {t("settings.providers.savedHint")}
                    </span>
                  ) : null}
                </>
              )}
            </div>
          </form>
        ) : (
          <div className="mx-auto flex max-w-md flex-col items-center justify-center rounded-xl border border-dashed border-[var(--border-strong)] px-6 py-16 text-center">
            <div className="mb-4 flex h-11 w-11 items-center justify-center rounded-xl border border-[var(--border)] bg-[var(--panel)]">
              <Server className="h-5 w-5 text-[var(--subtle)]" />
            </div>
            <h3 className="text-base font-semibold text-[var(--text)]">
              {t("settings.providers.emptyTitle")}
            </h3>
            <p className="mt-2 text-sm text-[var(--subtle)]">
              {t("settings.providers.emptyDescription")}
            </p>
            <Button className="mt-5" onClick={onAdd}>
              <Plus className="h-4 w-4" />
              {t("settings.providers.emptyCta")}
            </Button>
          </div>
        )}
      </div>
    </div>
  );
}

function renderModels({
  t,
  configs,
  settings,
  saveModelSettings,
}: {
  t: ReturnType<typeof useT>;
  configs: ModelConfig[];
  settings: ModelSettings | null;
  saveModelSettings: (settings: ModelSettings) => Promise<void>;
}) {
  const fallbackChatModelId =
    settings?.chat_model_config_id ?? configs[0]?.id ?? null;
  const followsChat = settings?.background_model_follows_chat ?? true;
  const backgroundModelId =
    settings?.background_model_config_id ??
    fallbackChatModelId ??
    configs[0]?.id ??
    null;

  function updateSettings(patch: Partial<ModelSettings>) {
    const next: ModelSettings = {
      chat_model_config_id: fallbackChatModelId,
      background_model_config_id: settings?.background_model_config_id ?? null,
      background_model_follows_chat: followsChat,
      ...patch,
    };
    void saveModelSettings(next);
  }

  return (
    <div className="mx-auto flex min-h-full w-full max-w-5xl flex-col px-8 py-8">
      <header className="mb-8">
        <h2 className="text-2xl font-semibold">{t("settings.models.title")}</h2>
        <p className="mt-2 text-sm text-[var(--subtle)]">
          {t("settings.models.description")}
        </p>
      </header>

      <div className="grid max-w-3xl gap-5">
        <label className="block text-sm font-medium">
          {t("settings.models.chatModel")}
          <select
            className="mt-2 h-10 w-full rounded-md border border-[var(--border-strong)] bg-[var(--panel)] px-3 text-sm text-[var(--text)] outline-none transition focus-visible:ring-2 focus-visible:ring-[var(--border-strong)]"
            disabled={configs.length === 0}
            value={fallbackChatModelId ?? ""}
            onChange={(event) =>
              updateSettings({
                chat_model_config_id: event.currentTarget.value || null,
                background_model_config_id: followsChat
                  ? null
                  : (settings?.background_model_config_id ?? null),
              })
            }
          >
            {configs.length === 0 ? (
              <option value="">{t("settings.models.noModels")}</option>
            ) : null}
            {configs.map((config) => (
              <option key={config.id} value={config.id}>
                {config.name} · {config.model}
              </option>
            ))}
          </select>
        </label>

        <div className="rounded-xl border border-[var(--border)] bg-[var(--panel-soft)] p-4">
          <label className="flex items-start gap-3 text-sm">
            <input
              type="checkbox"
              className="mt-0.5 h-4 w-4 accent-[var(--primary)]"
              checked={followsChat}
              onChange={(event) =>
                updateSettings({
                  background_model_follows_chat: event.currentTarget.checked,
                  background_model_config_id: event.currentTarget.checked
                    ? null
                    : backgroundModelId,
                })
              }
            />
            <span>
              <span className="block font-medium text-[var(--text)]">
                {t("settings.models.backgroundFollows")}
              </span>
              <span className="mt-1 block leading-5 text-[var(--subtle)]">
                {t("settings.models.backgroundFollowsDesc")}
              </span>
            </span>
          </label>
        </div>

        {!followsChat ? (
          <label className="block text-sm font-medium">
            {t("settings.models.backgroundModel")}
            <select
              className="mt-2 h-10 w-full rounded-md border border-[var(--border-strong)] bg-[var(--panel)] px-3 text-sm text-[var(--text)] outline-none transition focus-visible:ring-2 focus-visible:ring-[var(--border-strong)]"
              disabled={configs.length === 0}
              value={backgroundModelId ?? ""}
              onChange={(event) =>
                updateSettings({
                  background_model_follows_chat: false,
                  background_model_config_id: event.currentTarget.value || null,
                })
              }
            >
              {configs.length === 0 ? (
                <option value="">{t("settings.models.noModels")}</option>
              ) : null}
              {configs.map((config) => (
                <option key={config.id} value={config.id}>
                  {config.name} · {config.model}
                </option>
              ))}
            </select>
          </label>
        ) : null}
      </div>
    </div>
  );
}

function memoryTypeLabel(memory: Memory, t: ReturnType<typeof useT>): string {
  switch (memory.memory_type) {
    case "saved":
      return t("settings.memories.typeSaved");
    case "project":
      return t("settings.memories.typeProject");
    default:
      return t("settings.memories.typeAuto");
  }
}

function renderMemories({
  t,
  memories,
  newMemoryFact,
  isAddMemoryOpen,
  editingMemoryId,
  editingFact,
  setNewMemoryFact,
  setIsAddMemoryOpen,
  setEditingMemoryId,
  setEditingFact,
  submitSavedMemory,
  saveEditedMemory,
  confirmDeleteMemory,
}: {
  t: ReturnType<typeof useT>;
  memories: Memory[];
  newMemoryFact: string;
  isAddMemoryOpen: boolean;
  editingMemoryId: number | null;
  editingFact: string;
  setNewMemoryFact: (value: string) => void;
  setIsAddMemoryOpen: (value: boolean) => void;
  setEditingMemoryId: (value: number | null) => void;
  setEditingFact: (value: string) => void;
  submitSavedMemory: (event: FormEvent) => Promise<void>;
  saveEditedMemory: (id: number) => Promise<void>;
  confirmDeleteMemory: (id: number) => void;
}) {
  return (
    <div className="mx-auto flex min-h-full w-full max-w-5xl flex-col px-8 py-8">
      <header className="mb-6 flex items-start justify-between gap-4">
        <div>
          <h2 className="text-2xl font-semibold">
            {t("settings.memories.title")}
          </h2>
          <p className="mt-2 text-sm text-[var(--subtle)]">
            {t("settings.memories.description")}
          </p>
        </div>
        <Button variant="outline" onClick={() => setIsAddMemoryOpen(true)}>
          <Plus className="h-4 w-4" />
          {t("settings.memories.add")}
        </Button>
      </header>

      {memories.length === 0 ? (
        <div className="rounded-xl border border-dashed border-[var(--border-strong)] px-4 py-10 text-center text-sm text-[var(--subtle)]">
          {t("settings.memories.empty")}
        </div>
      ) : (
        <div className="space-y-2">
          {memories.map((memory) => (
            <article
              key={memory.id}
              className="rounded-xl border border-[var(--border)] bg-[var(--panel-soft)] p-3"
            >
              <div className="mb-2 flex items-center justify-between gap-3">
                <div className="flex min-w-0 items-center gap-2">
                  <Badge>{memoryTypeLabel(memory, t)}</Badge>
                  <span className="truncate text-xs text-[var(--subtle)]">
                    {t("settings.memories.usedCount", { n: memory.use_count })}
                  </span>
                </div>
                <div className="flex shrink-0 items-center gap-1">
                  {memory.memory_type === "saved" ? (
                    editingMemoryId === memory.id ? (
                      <>
                        <Button
                          aria-label={t("settings.memories.save")}
                          title={t("settings.memories.save")}
                          size="icon"
                          variant="ghost"
                          className="h-8 w-8"
                          onClick={() => void saveEditedMemory(memory.id)}
                        >
                          <Check className="h-4 w-4" />
                        </Button>
                        <Button
                          aria-label={t("settings.memories.cancelEdit")}
                          title={t("settings.memories.cancelEdit")}
                          size="icon"
                          variant="ghost"
                          className="h-8 w-8"
                          onClick={() => {
                            setEditingMemoryId(null);
                            setEditingFact("");
                          }}
                        >
                          <X className="h-4 w-4" />
                        </Button>
                      </>
                    ) : (
                      <Button
                        aria-label={t("settings.memories.edit")}
                        title={t("settings.memories.edit")}
                        size="icon"
                        variant="ghost"
                        className="h-8 w-8"
                        onClick={() => {
                          setEditingMemoryId(memory.id);
                          setEditingFact(memory.fact);
                        }}
                      >
                        <Pencil className="h-4 w-4" />
                      </Button>
                    )
                  ) : null}
                  <Button
                    aria-label={t("settings.memories.delete")}
                    title={t("settings.memories.delete")}
                    size="icon"
                    variant="ghost"
                    className="h-8 w-8"
                    onClick={() => confirmDeleteMemory(memory.id)}
                  >
                    <Trash2 className="h-4 w-4" />
                  </Button>
                </div>
              </div>
              {editingMemoryId === memory.id ? (
                <Textarea
                  value={editingFact}
                  onChange={(event) =>
                    setEditingFact(event.currentTarget.value)
                  }
                />
              ) : (
                <p className="text-sm leading-6 text-[var(--text)]">
                  {memory.fact}
                </p>
              )}
            </article>
          ))}
        </div>
      )}

      {isAddMemoryOpen ? (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/30 px-4">
          <form
            className="w-full max-w-sm rounded-xl border border-[var(--border)] bg-[var(--panel)] p-4 shadow-[var(--shadow-modal)]"
            onSubmit={(event) => void submitSavedMemory(event)}
          >
            <div className="mb-4 flex items-center justify-between gap-3">
              <div>
                <h2 className="text-sm font-semibold text-[var(--text)]">
                  {t("settings.memories.addDialogOpen")}
                </h2>
                <p className="mt-1 text-xs text-[var(--subtle)]">
                  {t("settings.memories.addDialogDesc")}
                </p>
              </div>
              <button
                aria-label={t("settings.memories.addDialogCancel")}
                title={t("settings.memories.addDialogCancel")}
                type="button"
                className="rounded p-1 text-[var(--subtle)] hover:bg-[var(--hover)]"
                onClick={() => {
                  setNewMemoryFact("");
                  setIsAddMemoryOpen(false);
                }}
              >
                <X className="h-4 w-4" />
              </button>
            </div>
            <Textarea
              autoFocus
              placeholder={t("settings.memories.addDialogPlaceholder")}
              value={newMemoryFact}
              onChange={(event) => setNewMemoryFact(event.currentTarget.value)}
            />
            <div className="mt-4 flex justify-end gap-2">
              <Button
                type="button"
                variant="ghost"
                onClick={() => {
                  setNewMemoryFact("");
                  setIsAddMemoryOpen(false);
                }}
              >
                {t("settings.memories.addDialogCancel")}
              </Button>
              <Button type="submit" disabled={!newMemoryFact.trim()}>
                <Plus className="h-4 w-4" />
                {t("settings.memories.addDialogConfirm")}
              </Button>
            </div>
          </form>
        </div>
      ) : null}
    </div>
  );
}

function renderArchive({
  t,
  dateLocale,
  query,
  setQuery,
  conversations,
  restoreConversation,
  confirmDeleteConversation,
}: {
  t: ReturnType<typeof useT>;
  dateLocale: string;
  query: string;
  setQuery: (value: string) => void;
  conversations: Conversation[];
  restoreConversation: (conversationId: string) => Promise<void>;
  confirmDeleteConversation: (conversation: Conversation) => void;
}) {
  return (
    <div className="mx-auto flex min-h-full w-full max-w-5xl flex-col px-8 py-8">
      <header className="mb-6">
        <h2 className="text-2xl font-semibold">
          {t("settings.archive.title")}
        </h2>
        <p className="mt-2 text-sm text-[var(--subtle)]">
          {t("settings.archive.description")}
        </p>
      </header>

      <label className="relative mb-5 block max-w-2xl">
        <Search className="pointer-events-none absolute left-3 top-2.5 h-4 w-4 text-[var(--subtle)]" />
        <Input
          className="h-9 pl-9"
          placeholder={t("settings.archive.searchPlaceholder")}
          value={query}
          onChange={(event) => setQuery(event.currentTarget.value)}
        />
      </label>

      {conversations.length === 0 ? (
        <div className="rounded-xl border border-dashed border-[var(--border-strong)] px-4 py-10 text-center text-sm text-[var(--subtle)]">
          {t("settings.archive.empty")}
        </div>
      ) : (
        <div className="space-y-2">
          {conversations.map((conversation) => (
            <article
              key={conversation.id}
              className="group rounded-xl border border-[var(--border)] bg-[var(--panel-soft)] p-3"
            >
              <div className="flex items-center gap-3">
                <Archive className="h-4 w-4 shrink-0 text-[var(--subtle)]" />
                <div className="min-w-0 flex-1">
                  <div className="truncate text-sm font-medium">
                    {conversation.title}
                  </div>
                  <div className="mt-0.5 text-xs text-[var(--subtle)]">
                    {new Date(conversation.updated_at).toLocaleString(
                      dateLocale,
                    )}
                  </div>
                </div>
                <Button
                  aria-label={t("settings.archive.restore")}
                  title={t("settings.archive.restore")}
                  size="icon"
                  variant="ghost"
                  className="h-8 w-8"
                  onClick={() => void restoreConversation(conversation.id)}
                >
                  <Undo2 className="h-4 w-4" />
                </Button>
                <Button
                  aria-label={t("settings.archive.delete")}
                  title={t("settings.archive.delete")}
                  size="icon"
                  variant="ghost"
                  className="h-8 w-8"
                  onClick={() => confirmDeleteConversation(conversation)}
                >
                  <Trash2 className="h-4 w-4" />
                </Button>
              </div>
            </article>
          ))}
        </div>
      )}
    </div>
  );
}

const GITHUB_URL = "https://github.com/MrSibe/Mira";

function AboutSection({
  t,
  version,
  actions,
}: {
  t: ReturnType<typeof useT>;
  version: string;
  actions: {
    updateState:
      | "idle"
      | "checking"
      | "available"
      | "downloading"
      | "done"
      | "error";
    updateVersion: string;
    updateError: string;
    checkForUpdates: () => Promise<void>;
    installUpdate: () => Promise<void>;
    closeForRestart: () => void;
  };
}) {
  const {
    updateState,
    updateVersion,
    updateError,
    checkForUpdates,
    installUpdate,
    closeForRestart,
  } = actions;
  const [showDialog, setShowDialog] = useState(false);

  useEffect(() => {
    if (updateState === "available" || updateState === "done") {
      setShowDialog(true);
    }
  }, [updateState]);

  return (
    <div className="mx-auto flex min-h-full w-full max-w-5xl flex-col px-8 py-8">
      <header className="mb-8">
        <h2 className="text-2xl font-semibold">{t("settings.about.title")}</h2>
        <p className="mt-2 text-sm text-[var(--subtle)]">
          {t("settings.about.description")}
        </p>
      </header>

      <div className="max-w-md space-y-4">
        <div className="flex items-center justify-between rounded-xl border border-[var(--border)] bg-[var(--panel-soft)] px-4 py-3">
          <span className="text-sm text-[var(--text)]">
            {t("settings.about.version")}
          </span>
          <span className="text-sm font-medium text-[var(--text)]">
            {version || "—"}
          </span>
        </div>

        <div className="flex items-center justify-between rounded-xl border border-[var(--border)] bg-[var(--panel-soft)] px-4 py-3">
          <span className="text-sm text-[var(--text)]">
            {t("settings.about.license")}
          </span>
          <span className="text-sm font-medium text-[var(--text)]">MIT</span>
        </div>

        <button
          className="flex w-full items-center justify-between rounded-xl border border-[var(--border)] bg-[var(--panel-soft)] px-4 py-3 text-left text-sm transition hover:bg-[var(--hover)]"
          onClick={() => void openUrl(GITHUB_URL)}
        >
          <span className="text-[var(--text)]">
            {t("settings.about.website")}
          </span>
          <ExternalLink className="h-4 w-4 text-[var(--subtle)]" />
        </button>

        <Button
          className="w-full"
          disabled={updateState === "checking" || updateState === "downloading"}
          onClick={() => void checkForUpdates()}
        >
          {updateState === "checking"
            ? t("settings.about.checking")
            : updateState === "available"
              ? t("settings.about.updateAvailable", {
                  version: updateVersion,
                })
              : t("settings.about.checkUpdates")}
        </Button>

        {updateState === "error" ? (
          <div className="rounded-md border border-[var(--danger)] bg-[var(--panel-soft)] px-3 py-2 text-xs text-[var(--danger)]">
            {updateError}
          </div>
        ) : null}
      </div>

      {showDialog ? (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/30 px-4">
          <div className="w-full max-w-sm rounded-xl border border-[var(--border)] bg-[var(--panel)] p-4 shadow-[var(--shadow-modal)]">
            <h2 className="text-sm font-semibold text-[var(--text)]">
              {updateState === "available"
                ? t("settings.about.dialogTitle")
                : updateState === "downloading"
                  ? t("settings.about.dialogDownloading")
                  : t("settings.about.dialogDone")}
            </h2>
            <p className="mt-2 text-xs leading-5 text-[var(--subtle)]">
              {updateState === "available"
                ? t("settings.about.dialogDesc", { version: updateVersion })
                : updateState === "downloading"
                  ? t("settings.about.dialogDownloadingDesc")
                  : t("settings.about.dialogDoneDesc")}
            </p>
            <div className="mt-4 flex justify-end gap-2">
              {updateState === "available" ? (
                <>
                  <Button
                    type="button"
                    variant="ghost"
                    onClick={() => setShowDialog(false)}
                  >
                    {t("settings.about.dialogLater")}
                  </Button>
                  <Button type="button" onClick={() => void installUpdate()}>
                    {t("settings.about.dialogUpdate")}
                  </Button>
                </>
              ) : updateState === "done" ? (
                <Button type="button" onClick={closeForRestart}>
                  {t("settings.about.dialogRestart")}
                </Button>
              ) : null}
            </div>
          </div>
        </div>
      ) : null}
    </div>
  );
}
