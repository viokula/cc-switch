import { act, fireEvent, screen, waitFor } from "@testing-library/react";
import { useQueryClient, type QueryClient } from "@tanstack/react-query";
import { beforeEach, describe, expect, it, vi } from "vitest";
import i18n from "i18next";
import { HuaweiCodeAgentAuthCenterSection } from "@/huawei/authCenterSection";
import { huaweiCodeAgent2Api } from "@/huawei/huaweiCodeAgent2Api";
import { settingsApi } from "@/lib/api/settings";
import zh from "@/huawei/locales/zh.json";
import type { Settings } from "@/types";
import { renderWithQueryClient } from "../utils/testQueryClient";

vi.mock("@/huawei/HuaweiCodeAgentOAuthSection", () => ({
  HuaweiCodeAgentOAuthSection: () => null,
}));

const defaults: Settings = { showInTray: true, minimizeToTrayOnClose: true };
let client: QueryClient;
function Editor() {
  client = useQueryClient();
  return <HuaweiCodeAgentAuthCenterSection />;
}
const input = () => screen.getByLabelText("CodeAgent 插件版本");
const save = () => screen.getByRole("button", { name: "保存" });
const reset = () => screen.getByRole("button", { name: "恢复默认" });

beforeEach(() => {
  vi.restoreAllMocks();
  i18n.addResourceBundle("zh", "translation", zh, true, true);
  vi.spyOn(settingsApi, "get").mockResolvedValue(defaults);
  vi.spyOn(huaweiCodeAgent2Api, "setPluginVersion").mockImplementation(
    async (version) => {
      const normalized = version.trim() || "cli-1.2615.02.03";
      vi.mocked(settingsApi.get).mockResolvedValue({
        ...defaults,
        huaweiCodeagentPluginVersion: normalized,
      });
      return normalized;
    },
  );
});

describe("Huawei plugin version", () => {
  it("shows the default, saves explicitly and resets through the dedicated command", async () => {
    renderWithQueryClient(<Editor />);
    await waitFor(() => expect(input()).toBeEnabled());
    expect(input()).toHaveValue("cli-1.2615.02.03");
    expect(save()).toBeDisabled();
    fireEvent.change(input(), { target: { value: "  custom-version  " } });
    expect(huaweiCodeAgent2Api.setPluginVersion).not.toHaveBeenCalled();
    fireEvent.click(save());
    await waitFor(() => expect(input()).toHaveValue("custom-version"));
    expect(
      vi.mocked(huaweiCodeAgent2Api.setPluginVersion).mock.calls[0][0],
    ).toBe("  custom-version  ");
    await waitFor(() => expect(reset()).toBeEnabled());
    fireEvent.click(reset());
    await waitFor(() => expect(input()).toHaveValue("cli-1.2615.02.03"));
    expect(
      vi.mocked(huaweiCodeAgent2Api.setPluginVersion).mock.calls[1][0],
    ).toBe("");
  });

  it("loads a custom value and preserves a dirty draft during settings refresh", async () => {
    vi.mocked(settingsApi.get).mockResolvedValue({
      ...defaults,
      huaweiCodeagentPluginVersion: "saved",
    });
    renderWithQueryClient(<Editor />);
    await waitFor(() => expect(input()).toHaveValue("saved"));
    fireEvent.change(input(), { target: { value: "unsaved" } });
    vi.mocked(settingsApi.get).mockResolvedValue({
      ...defaults,
      huaweiCodeagentPluginVersion: "refreshed",
    });
    await act(async () => {
      await client.invalidateQueries({ queryKey: ["settings"] });
    });
    expect(input()).toHaveValue("unsaved");
  });

  it("disables editing while loading and saving", async () => {
    let load!: (value: Settings) => void;
    vi.mocked(settingsApi.get).mockReturnValueOnce(
      new Promise((resolve) => {
        load = resolve;
      }),
    );
    renderWithQueryClient(<Editor />);
    expect(input()).toBeDisabled();
    expect(screen.getByRole("status")).toBeInTheDocument();
    await act(async () => load(defaults));
    await waitFor(() => expect(input()).toBeEnabled());
    let finish!: (value: string) => void;
    vi.mocked(huaweiCodeAgent2Api.setPluginVersion).mockReturnValueOnce(
      new Promise((resolve) => {
        finish = resolve;
      }),
    );
    fireEvent.change(input(), { target: { value: "pending" } });
    fireEvent.click(save());
    await waitFor(() => expect(input()).toBeDisabled());
    expect(reset()).toBeDisabled();
    await act(async () => finish("cli-1.2615.02.03"));
    await waitFor(() => expect(input()).toBeEnabled());
  });

  it("shows save failures without losing the draft", async () => {
    vi.mocked(huaweiCodeAgent2Api.setPluginVersion).mockRejectedValueOnce(
      new Error("Invalid header"),
    );
    renderWithQueryClient(<Editor />);
    await waitFor(() => expect(input()).toBeEnabled());
    fireEvent.change(input(), { target: { value: "invalid" } });
    fireEvent.click(save());
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Invalid header",
    );
    expect(input()).toHaveValue("invalid");
    expect(save()).toBeEnabled();
  });

  it("shows settings load errors without allowing a save", async () => {
    vi.mocked(settingsApi.get).mockRejectedValueOnce(
      new Error("Settings unavailable"),
    );
    renderWithQueryClient(<Editor />);
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Settings unavailable",
    );
    expect(input()).toBeDisabled();
    expect(save()).toBeDisabled();
    expect(reset()).toBeDisabled();
  });
});
