import { describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { fetchHuaweiCodeAgent2Models } from "./huaweiCodeAgent2Api";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

describe("Huawei account-scoped model API", () => {
  it.each(["account-a", "account-b", null, undefined])(
    "passes account %s to the command without using another default",
    async (accountId) => {
      vi.mocked(invoke).mockResolvedValueOnce([]);
      await expect(fetchHuaweiCodeAgent2Models(accountId)).resolves.toEqual([]);
      expect(invoke).toHaveBeenLastCalledWith(
        "huawei_codeagent_2_fetch_models",
        { accountId: accountId ?? null },
      );
    },
  );
});
