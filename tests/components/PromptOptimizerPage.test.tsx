import { fireEvent, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import i18n from "i18next";
import PromptOptimizerPage from "@/components/promptOptimizer/PromptOptimizerPage";
import { promptOptimizerApi, settingsApi } from "@/lib/api";
import zh from "@/i18n/locales/zh.json";
import { renderWithQueryClient } from "../utils/testQueryClient";

vi.mock("@/lib/query", () => ({
  useSettingsQuery: () => ({
    data: {
      showInTray: true,
      minimizeToTrayOnClose: true,
      promptOptimizerApp: "claude",
      promptOptimizerProviderId: "provider-1",
      promptOptimizerModel: "model-1",
    },
  }),
  useProvidersQuery: () => ({
    data: {
      providers: {
        "provider-1": { id: "provider-1", name: "Provider One" },
      },
      currentProviderId: "provider-1",
    },
    isLoading: false,
  }),
}));

vi.mock("@/lib/toast", () => ({
  toast: { success: vi.fn(), error: vi.fn() },
}));

beforeEach(() => {
  vi.restoreAllMocks();
  i18n.addResourceBundle("zh", "translation", zh, true, true);
  vi.spyOn(settingsApi, "save").mockResolvedValue(true);
});

const optimizeButton = () =>
  screen.getByRole("button", { name: /优化|Optimize/ });
const undoButton = () => screen.getByRole("button", { name: /撤销|Undo/ });
const textarea = () =>
  document.getElementById("prompt-optimizer-text") as HTMLTextAreaElement;

describe("PromptOptimizerPage", () => {
  it("replaces the prompt with the optimized text", async () => {
    vi.spyOn(promptOptimizerApi, "optimize").mockResolvedValue("改好的提示词");
    renderWithQueryClient(<PromptOptimizerPage />);

    fireEvent.change(textarea(), { target: { value: "帮我写个函数" } });
    fireEvent.click(optimizeButton());

    await waitFor(() => expect(textarea()).toHaveValue("改好的提示词"));
    expect(promptOptimizerApi.optimize).toHaveBeenCalledWith({
      app: "claude",
      providerId: "provider-1",
      model: "model-1",
      text: "帮我写个函数",
    });
    expect(settingsApi.save).toHaveBeenCalledWith(
      expect.objectContaining({
        promptOptimizerApp: "claude",
        promptOptimizerProviderId: "provider-1",
        promptOptimizerModel: "model-1",
      }),
    );
    expect(undoButton()).toBeEnabled();
  });

  it("undoes the last optimization", async () => {
    vi.spyOn(promptOptimizerApi, "optimize").mockResolvedValue("改好的提示词");
    renderWithQueryClient(<PromptOptimizerPage />);

    fireEvent.change(textarea(), { target: { value: "原始提示词" } });
    fireEvent.click(optimizeButton());
    await waitFor(() => expect(textarea()).toHaveValue("改好的提示词"));

    fireEvent.click(undoButton());
    expect(textarea()).toHaveValue("原始提示词");
    expect(undoButton()).toBeDisabled();
  });

  it("refuses empty input without calling the backend", () => {
    const optimize = vi
      .spyOn(promptOptimizerApi, "optimize")
      .mockResolvedValue("unused");
    renderWithQueryClient(<PromptOptimizerPage />);

    fireEvent.click(optimizeButton());

    expect(optimize).not.toHaveBeenCalled();
  });
});
