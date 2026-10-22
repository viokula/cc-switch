import i18n from "i18next";
import { initReactI18next } from "react-i18next";

import en from "./locales/en.json";
import ja from "./locales/ja.json";
import tr from "./locales/tr.json";
import zh from "./locales/zh.json";
import zhTW from "./locales/zh-TW.json";

// 华为 CodeAgent 扩展的语言包与主语言包合并（providerForm 命名空间按子键合并）。
import huaweiEn from "@/huawei/locales/en.json";
import huaweiJa from "@/huawei/locales/ja.json";
import huaweiZh from "@/huawei/locales/zh.json";
import huaweiZhTW from "@/huawei/locales/zh-TW.json";

const mergeLocale = (
  base: Record<string, unknown>,
  extra: Record<string, unknown>,
): Record<string, unknown> => {
  const merged: Record<string, unknown> = { ...base, ...extra };
  const baseProviderForm = base.providerForm as
    | Record<string, unknown>
    | undefined;
  const extraProviderForm = extra.providerForm as
    | Record<string, unknown>
    | undefined;
  if (baseProviderForm || extraProviderForm) {
    merged.providerForm = {
      ...baseProviderForm,
      ...extraProviderForm,
      presets: {
        ...(baseProviderForm?.presets as Record<string, unknown> | undefined),
        ...(extraProviderForm?.presets as Record<string, unknown> | undefined),
      },
    };
  }
  return merged;
};

type Language = "zh" | "zh-TW" | "en" | "ja" | "tr";

const DEFAULT_LANGUAGE: Language = "zh";

const isLanguage = (value: unknown): value is Language =>
  value === "zh" ||
  value === "zh-TW" ||
  value === "en" ||
  value === "ja" ||
  value === "tr";

const getInitialLanguage = (): Language => {
  if (typeof window !== "undefined") {
    try {
      const stored = window.localStorage.getItem("language");
      if (isLanguage(stored)) {
        return stored;
      }
    } catch (error) {
      console.warn("[i18n] Failed to read stored language preference", error);
    }
  }

  const navigatorLang =
    typeof navigator !== "undefined"
      ? (navigator.language?.toLowerCase() ??
        navigator.languages?.[0]?.toLowerCase())
      : undefined;

  if (navigatorLang === "zh") {
    return "zh";
  }

  if (
    navigatorLang?.startsWith("zh-tw") ||
    navigatorLang?.startsWith("zh-hk") ||
    navigatorLang?.startsWith("zh-mo") ||
    navigatorLang?.startsWith("zh-hant")
  ) {
    return "zh-TW";
  }

  if (navigatorLang?.startsWith("zh")) {
    return "zh";
  }

  if (navigatorLang?.startsWith("ja")) {
    return "ja";
  }

  if (navigatorLang?.startsWith("tr")) {
    return "tr";
  }

  if (navigatorLang?.startsWith("en")) {
    return "en";
  }

  // 认得出系统语言、但界面没有这种语言（德、法、韩…）时用英文：英文大多数人能读，简中几乎不能
  return navigatorLang ? "en" : DEFAULT_LANGUAGE;
};

const resources = {
  tr: { translation: tr },
  en: {
    translation: mergeLocale(en, huaweiEn),
  },
  ja: {
    translation: mergeLocale(ja, huaweiJa),
  },
  zh: {
    translation: mergeLocale(zh, huaweiZh),
  },
  "zh-TW": {
    translation: mergeLocale(zhTW, huaweiZhTW),
  },
};

i18n.use(initReactI18next).init({
  resources,
  lng: getInitialLanguage(), // 根据本地存储或系统语言选择默认语言
  fallbackLng: "en", // 如果缺少中文翻译则退回英文

  interpolation: {
    escapeValue: false, // React 已经默认转义
  },

  // 开发模式下显示调试信息
  debug: false,
});

/**
 * 用后端 settings.json 里保存的语言校正界面语言。
 *
 * 首屏语言取自 localStorage（同步可读），但它只是缓存：开发版与正式版的
 * WebView 来源不同、WebView 数据被清空时都会读不到或读到旧值，界面就落到
 * 系统语言。启动后拿到后端设置再对一次，不一致时切换并回写缓存。
 */
export const syncLanguageFromSettings = (language?: string | null): void => {
  if (!isLanguage(language)) return;

  if (i18n.language !== language) {
    void i18n.changeLanguage(language);
  }

  try {
    if (window.localStorage.getItem("language") !== language) {
      window.localStorage.setItem("language", language);
    }
  } catch (error) {
    console.warn("[i18n] Failed to persist language preference", error);
  }
};

export default i18n;
