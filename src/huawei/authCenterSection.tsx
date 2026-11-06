import { useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { useSettingsQuery } from "@/lib/query/queries";
import type { Settings } from "@/types";
import { huaweiCodeAgent2Api } from "./huaweiCodeAgent2Api";
import { ProviderIcon } from "@/components/ProviderIcon";
import { HuaweiCodeAgentOAuthSection } from "./HuaweiCodeAgentOAuthSection";

export function HuaweiCodeAgentAuthCenterSection() {
  const { t } = useTranslation();
  const settings = useSettingsQuery();
  const queryClient = useQueryClient();
  const [draft, setDraft] = useState<string>();
  const version =
    draft ?? settings.data?.huaweiCodeagentPluginVersion ?? "cli-1.2615.02.03";
  const saveVersion = useMutation({
    mutationFn: huaweiCodeAgent2Api.setPluginVersion,
    onSuccess: async (savedVersion) => {
      queryClient.setQueryData<Settings>(["settings"], (current) =>
        current
          ? { ...current, huaweiCodeagentPluginVersion: savedVersion }
          : current,
      );
      setDraft(undefined);
      await queryClient.invalidateQueries({ queryKey: ["settings"] });
    },
  });
  const error = saveVersion.error ?? settings.error;
  const disabled = !settings.data || saveVersion.isPending;

  return (
    <section className="rounded-xl border border-border/60 bg-card/60 p-6">
      <div className="mb-4 flex items-center gap-3">
        <div className="flex h-10 w-10 items-center justify-center rounded-xl bg-muted">
          <ProviderIcon icon="generic" name="Huawei" size={20} />
        </div>
        <div>
          <h4 className="font-medium">
            {t("huaweiCodeAgent2.authCenterTitle", {
              defaultValue: "华为 CodeAgent",
            })}
          </h4>
          <p className="text-sm text-muted-foreground">
            {t("huaweiCodeAgent2.authCenterDescription", {
              defaultValue: "管理华为 CodeAgent 账号",
            })}
          </p>
        </div>
      </div>

      <HuaweiCodeAgentOAuthSection />
      <div className="mt-4 space-y-2 border-t pt-4">
        <Label htmlFor="huawei-plugin-version">
          {t("huaweiCodeAgent2.pluginVersion")}
        </Label>
        <p
          id="huawei-plugin-version-description"
          className="text-sm text-muted-foreground"
        >
          {t("huaweiCodeAgent2.pluginVersionDescription")}
        </p>
        <div className="flex flex-wrap items-center gap-2">
          <Input
            id="huawei-plugin-version"
            aria-describedby="huawei-plugin-version-description"
            className="max-w-xs"
            value={version}
            onChange={(event) => setDraft(event.target.value)}
            disabled={disabled}
          />
          <Button
            size="sm"
            disabled={disabled || draft === undefined}
            onClick={() => saveVersion.mutate(version)}
          >
            {saveVersion.isPending
              ? t("common.saving")
              : t("huaweiCodeAgent2.pluginVersionSave")}
          </Button>
          <Button
            size="sm"
            variant="outline"
            disabled={disabled}
            onClick={() => saveVersion.mutate("")}
          >
            {t("huaweiCodeAgent2.pluginVersionReset")}
          </Button>
        </div>
        {settings.isLoading && <p role="status">{t("common.loading")}</p>}
        {error && (
          <p role="alert" className="text-sm text-destructive">
            {String(error)}
          </p>
        )}
      </div>
    </section>
  );
}

export default HuaweiCodeAgentAuthCenterSection;
