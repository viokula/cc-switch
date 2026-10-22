import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClientProvider } from "@tanstack/react-query";
import { beforeEach, describe, expect, it, vi } from "vitest";
import "@/huawei";
import { ProviderForm } from "@/components/providers/forms/ProviderForm";
import { ClaudeDesktopProviderForm } from "@/components/providers/forms/ClaudeDesktopProviderForm";
import {
  getFormExtension,
  type ExtensionAuthSectionProps,
} from "@/config/formExtensions";
import { huaweiDesktopPresets } from "@/huawei/huaweiDesktopPresets";
import { createTestQueryClient } from "../utils/testQueryClient";

const mocks = vi.hoisted(() => ({ fetchModels: vi.fn() }));
vi.mock("@/huawei/huaweiCodeAgent2Api", () => ({
  fetchHuaweiCodeAgent2Models: mocks.fetchModels,
}));
vi.mock("@/huawei/HuaweiCodeAgentOAuthSection", () => ({
  HuaweiCodeAgentOAuthSection: ({
    selectedAccountId,
    onAccountSelect,
  }: ExtensionAuthSectionProps) => (
    <select
      aria-label="Huawei account"
      value={selectedAccountId ?? ""}
      onChange={(e) => onAccountSelect?.(e.target.value || null)}
    >
      <option value="">Default</option>
      <option value="account-a">A</option>
      <option value="account-b">B</option>
    </select>
  ),
}));
vi.mock("@/components/JsonEditor", () => ({
  default: ({
    value,
    onChange,
  }: {
    value: string;
    onChange: (value: string) => void;
  }) => (
    <textarea
      aria-label="Settings JSON"
      value={value}
      onChange={(e) => onChange(e.target.value)}
    />
  ),
}));
vi.mock("@/components/providers/forms/hooks", async (importOriginal) => {
  const actual =
    await importOriginal<typeof import("@/components/providers/forms/hooks")>();
  const signedOut = {
    isAuthenticated: false,
    isStatusSuccess: true,
    isStatusError: false,
    accounts: [],
  };
  return {
    ...actual,
    useCopilotAuth: () => signedOut,
    useCodexOauth: () => signedOut,
    useXaiOauth: () => signedOut,
  };
});
vi.mock("@/lib/api/providers", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/lib/api/providers")>();
  return {
    ...actual,
    providersApi: {
      ...actual.providersApi,
      getClaudeDesktopDefaultRoutes: () => Promise.resolve([]),
    },
  };
});

type App = "claude" | "claude-desktop";
function renderHuawei(app: App, providerType: string, override = false) {
  const onSubmit = vi.fn();
  const props = {
    submitLabel: "save-huawei",
    onSubmit,
    onCancel: vi.fn(),
    initialData: {
      name: "Huawei",
      category: "cloud_provider" as const,
      settingsConfig: {
        env: {
          ANTHROPIC_BASE_URL: "https://example.test/api/v2",
          ANTHROPIC_MODEL: "test-model",
        },
      },
      meta: {
        providerType,
        selectedAccountId: "account-a",
        claudeDesktopMode: "proxy" as const,
        claudeDesktopModelRoutes: {
          "claude-sonnet-5": { model: "test-model" },
        },
        ...(override
          ? { skipTlsVerify: false, stripPathPrefix: "/custom" }
          : {}),
      },
    },
  };
  render(
    <QueryClientProvider client={createTestQueryClient()}>
      {app === "claude" ? (
        <ProviderForm {...props} appId="claude" modeView="direct" />
      ) : (
        <ClaudeDesktopProviderForm {...props} />
      )}
    </QueryClientProvider>,
  );
  return onSubmit;
}

beforeEach(() => {
  mocks.fetchModels
    .mockReset()
    .mockResolvedValue([{ id: "fetched-model", ownedBy: null }]);
});

describe("Huawei real provider forms", () => {
  it.each([
    ["claude", "huawei_codeagent_2"],
    ["claude", "huawei_codeagent_3"],
    ["claude-desktop", "huawei_codeagent_2"],
    ["claude-desktop", "huawei_codeagent_3"],
  ] as const)(
    "%s %s restores and saves the bound account with extension defaults",
    async (app, providerType) => {
      const onSubmit = renderHuawei(app, providerType);
      expect(
        await screen.findByRole("combobox", { name: "Huawei account" }),
      ).toHaveValue("account-a");
      fireEvent.click(
        screen.getByRole("button", {
          name: /获取模型|providerForm.fetchModels/,
        }),
      );
      await waitFor(() =>
        expect(mocks.fetchModels).toHaveBeenLastCalledWith("account-a"),
      );
      fireEvent.change(
        screen.getByRole("combobox", { name: "Huawei account" }),
        { target: { value: "account-b" } },
      );
      fireEvent.click(
        screen.getByRole("button", {
          name: /获取模型|providerForm.fetchModels/,
        }),
      );
      await waitFor(() =>
        expect(mocks.fetchModels).toHaveBeenLastCalledWith("account-b"),
      );
      fireEvent.click(screen.getByRole("button", { name: "save-huawei" }));
      await waitFor(() => expect(onSubmit).toHaveBeenCalledTimes(1));
      expect(onSubmit.mock.calls[0][0].meta).toMatchObject({
        ...getFormExtension(providerType)?.defaultMetaFields,
        selectedAccountId: "account-b",
        providerType,
        appTarget: app === "claude" ? "claude" : "claude_desktop",
      });
    },
  );

  it.each(["claude", "claude-desktop"] as const)(
    "%s preserves explicit meta overrides and clears binding for default",
    async (app) => {
      const onSubmit = renderHuawei(app, "huawei_codeagent_2", true);
      fireEvent.change(
        await screen.findByRole("combobox", { name: "Huawei account" }),
        { target: { value: "" } },
      );
      fireEvent.click(
        screen.getByRole("button", {
          name: /获取模型|providerForm.fetchModels/,
        }),
      );
      await waitFor(() =>
        expect(mocks.fetchModels).toHaveBeenLastCalledWith(null),
      );
      fireEvent.click(screen.getByRole("button", { name: "save-huawei" }));
      await waitFor(() => expect(onSubmit).toHaveBeenCalledTimes(1));
      expect(onSubmit.mock.calls[0][0].meta).toMatchObject({
        skipTlsVerify: false,
        stripPathPrefix: "/custom",
      });
      expect(onSubmit.mock.calls[0][0].meta.selectedAccountId).toBeUndefined();
    },
  );

  it.each(huaweiDesktopPresets)(
    "new Desktop preset $name submits the correct extension metadata",
    async (preset) => {
      const onSubmit = vi.fn();
      render(
        <QueryClientProvider client={createTestQueryClient()}>
          <ClaudeDesktopProviderForm
            submitLabel="save-huawei"
            onSubmit={onSubmit}
            onCancel={vi.fn()}
          />
        </QueryClientProvider>,
      );
      fireEvent.click(
        screen.getByRole("button", {
          name: new RegExp(preset.name.replace(/\./g, "\\.")),
        }),
      );
      fireEvent.change(
        await screen.findByRole("combobox", { name: "Huawei account" }),
        { target: { value: "account-b" } },
      );
      expect(screen.queryByLabelText("API Key")).not.toBeInTheDocument();
      fireEvent.click(screen.getByRole("button", { name: "save-huawei" }));
      await waitFor(() => expect(onSubmit).toHaveBeenCalledTimes(1));
      expect(onSubmit.mock.calls[0][0].meta).toMatchObject({
        ...getFormExtension(preset.providerType)?.defaultMetaFields,
        selectedAccountId: "account-b",
        providerType: preset.providerType,
        appTarget: "claude_desktop",
        claudeDesktopMode: "proxy",
      });
    },
  );

  it.each(huaweiDesktopPresets)(
    "Desktop preset $name declares the Desktop target",
    (preset) => {
      expect(preset.appTarget).toBe("claude_desktop");
    },
  );
});
