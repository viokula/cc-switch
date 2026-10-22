import { describe, expect, it, vi } from "vitest";
import en from "@/i18n/locales/en.json";
import zh from "@/i18n/locales/zh.json";
import zhTW from "@/i18n/locales/zh-TW.json";
import ja from "@/i18n/locales/ja.json";

// Load the real resource merge, not the empty resources from setupTests.
describe("Huawei locale merge", () => {
  it("preserves every existing preset translation alongside Huawei names", async () => {
    vi.resetModules();
    const { default: i18n } = await import("@/i18n");
    for (const [language, base] of Object.entries({
      en,
      zh,
      "zh-TW": zhTW,
      ja,
    })) {
      const translate = i18n.getFixedT(language);
      for (const [key, value] of Object.entries(base.providerForm.presets)) {
        expect(translate(`providerForm.presets.${key}`)).toBe(value);
      }
      expect(translate("providerForm.presets.huaweiCodeAgent2")).not.toBe(
        "providerForm.presets.huaweiCodeAgent2",
      );
      expect(translate("providerForm.presets.huaweiCodeAgent3")).not.toBe(
        "providerForm.presets.huaweiCodeAgent3",
      );
      expect(translate("providerForm.categoryOfficial")).toBe(
        base.providerForm.categoryOfficial,
      );
    }
  });
});
