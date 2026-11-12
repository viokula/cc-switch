import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Copy, Loader2, RotateCcw, Sparkles } from "lucide-react";
import { AppPageHeader } from "@/components/shell/AppPageHeader";
import { Button } from "@/components/ui/button";
import { HelpTip } from "@/components/ui/help-tip";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { promptOptimizerApi, settingsApi, type AppId } from "@/lib/api";
import { useProvidersQuery, useSettingsQuery } from "@/lib/query";
import { toast } from "@/lib/toast";

/** 只有支持本地路由/对话协议的应用才谈得上“用它的供应商改写提示词”。 */
const OPTIMIZER_APPS: AppId[] = [
  "claude",
  "claude-desktop",
  "codex",
  "gemini",
  "grokbuild",
];

export default function PromptOptimizerPage() {
  const { t } = useTranslation();
  const { data: settings } = useSettingsQuery();
  const [app, setApp] = useState<AppId>("claude");
  const [providerId, setProviderId] = useState("");
  const [model, setModel] = useState("");
  const [text, setText] = useState("");
  const [previous, setPrevious] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [hydrated, setHydrated] = useState(false);

  const { data: providerData, isLoading: providersLoading } =
    useProvidersQuery(app);
  const providers = useMemo(
    () => Object.values(providerData?.providers ?? {}),
    [providerData],
  );

  useEffect(() => {
    if (!settings || hydrated) return;
    const savedApp = OPTIMIZER_APPS.includes(
      settings.promptOptimizerApp as AppId,
    )
      ? (settings.promptOptimizerApp as AppId)
      : "claude";
    setApp(savedApp);
    setProviderId(settings.promptOptimizerProviderId ?? "");
    setModel(settings.promptOptimizerModel ?? "");
    setHydrated(true);
  }, [settings, hydrated]);

  useEffect(() => {
    if (!hydrated || providerId) return;
    setProviderId(providerData?.currentProviderId ?? "");
  }, [hydrated, providerId, providerData?.currentProviderId]);

  const persist = () => {
    if (!settings) return;
    void settingsApi
      .save({
        ...settings,
        promptOptimizerApp: app,
        promptOptimizerProviderId: providerId,
        promptOptimizerModel: model.trim() || undefined,
      })
      .catch((error) => {
        console.error("Failed to save prompt optimizer settings:", error);
      });
  };

  const handleOptimize = async () => {
    const input = text.trim();
    if (!input) {
      toast.error(t("promptOptimizer.emptyInput"));
      return;
    }
    if (!providerId) {
      toast.error(t("promptOptimizer.providerRequired"));
      return;
    }
    setBusy(true);
    persist();
    try {
      const optimized = await promptOptimizerApi.optimize({
        app,
        providerId,
        model: model.trim() || undefined,
        text: input,
      });
      setPrevious(text);
      setText(optimized);
      toast.success(t("promptOptimizer.succeeded"));
    } catch (error) {
      console.error("Failed to optimize prompt:", error);
      const message =
        error instanceof Error
          ? error.message
          : typeof error === "string"
            ? error
            : JSON.stringify(error);
      toast.error(t("promptOptimizer.failed", { message }));
    } finally {
      setBusy(false);
    }
  };

  const handleUndo = () => {
    if (previous === null) return;
    setText(previous);
    setPrevious(null);
  };

  const handleCopy = async () => {
    try {
      await navigator.clipboard.writeText(text);
      toast.success(t("promptOptimizer.copied"));
    } catch (error) {
      console.error("Failed to copy prompt:", error);
      toast.error(t("promptOptimizer.copyFailed"));
    }
  };

  return (
    <div id="main-content" className="flex min-h-0 flex-1 flex-col">
      <AppPageHeader
        icon={<Sparkles className="h-5 w-5" strokeWidth={1.5} />}
        title={t("promptOptimizer.title")}
        titleExtra={
          <HelpTip title={t("promptOptimizer.helpTitle")}>
            {t("promptOptimizer.help")}
          </HelpTip>
        }
      />
      <div className="min-h-0 flex-1 overflow-y-auto scroll-stable">
        <div className="mx-auto w-full max-w-3xl space-y-4 px-6 pb-10 pt-4">
          <div className="grid gap-3 sm:grid-cols-3">
            <div className="space-y-1.5">
              <label
                className="text-caption font-semibold text-fg-2"
                htmlFor="prompt-optimizer-app"
              >
                {t("promptOptimizer.app")}
              </label>
              <Select
                value={app}
                onValueChange={(value) => {
                  setApp(value as AppId);
                  setProviderId("");
                  setModel("");
                }}
              >
                <SelectTrigger id="prompt-optimizer-app" className="h-9">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent className="z-[70]">
                  {OPTIMIZER_APPS.map((id) => (
                    <SelectItem key={id} value={id}>
                      {t(`apps.${id}`, { defaultValue: id })}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>

            <div className="space-y-1.5">
              <label
                className="text-caption font-semibold text-fg-2"
                htmlFor="prompt-optimizer-provider"
              >
                {t("promptOptimizer.provider")}
              </label>
              <Select
                value={providerId}
                onValueChange={setProviderId}
                disabled={providersLoading || providers.length === 0}
              >
                <SelectTrigger
                  id="prompt-optimizer-provider"
                  className="h-9"
                  aria-label={t("promptOptimizer.provider")}
                >
                  <SelectValue
                    placeholder={t("promptOptimizer.providerPlaceholder")}
                  />
                </SelectTrigger>
                <SelectContent className="z-[70]">
                  {providers.map((provider) => (
                    <SelectItem key={provider.id} value={provider.id}>
                      {provider.name}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>

            <div className="space-y-1.5">
              <label
                className="text-caption font-semibold text-fg-2"
                htmlFor="prompt-optimizer-model"
              >
                {t("promptOptimizer.model")}
              </label>
              <input
                id="prompt-optimizer-model"
                value={model}
                onChange={(event) => setModel(event.target.value)}
                placeholder={t("promptOptimizer.modelPlaceholder")}
                className="h-9 w-full rounded-control border border-border bg-surface px-3 text-body text-fg-1 outline-none focus:border-action"
              />
            </div>
          </div>

          <div className="space-y-1.5">
            <label
              className="text-caption font-semibold text-fg-2"
              htmlFor="prompt-optimizer-text"
            >
              {t("promptOptimizer.prompt")}
            </label>
            <textarea
              id="prompt-optimizer-text"
              value={text}
              onChange={(event) => setText(event.target.value)}
              rows={12}
              placeholder={t("promptOptimizer.placeholder")}
              className="w-full resize-y rounded-card border border-border bg-surface px-3 py-2.5 font-mono text-body text-fg-1 outline-none focus:border-action"
            />
          </div>

          <div className="flex flex-wrap items-center gap-2">
            <Button onClick={() => void handleOptimize()} disabled={busy}>
              {busy ? (
                <Loader2 className="h-4 w-4 animate-spin" strokeWidth={1.5} />
              ) : (
                <Sparkles className="h-4 w-4" strokeWidth={1.5} />
              )}
              {busy
                ? t("promptOptimizer.optimizing")
                : t("promptOptimizer.action")}
            </Button>
            <Button
              variant="outline"
              onClick={handleUndo}
              disabled={busy || previous === null}
            >
              <RotateCcw className="h-4 w-4" strokeWidth={1.5} />
              {t("promptOptimizer.undo")}
            </Button>
            <Button
              variant="outline"
              onClick={() => void handleCopy()}
              disabled={!text}
            >
              <Copy className="h-4 w-4" strokeWidth={1.5} />
              {t("promptOptimizer.copy")}
            </Button>
          </div>

          <p className="text-caption text-fg-3">{t("promptOptimizer.note")}</p>
        </div>
      </div>
    </div>
  );
}
