/**
 * 表单扩展注册表
 *
 * 允许独立供应商模块（如华为 CodeAgent）在运行时向主表单注入：
 * - 供应商专属的认证区块（AuthSection，懒加载）
 * - 模型列表拉取回调（fetchModels）
 * - 免填 API Key 标记（skipApiKeyRequired）
 * - 保存供应商时写入 meta 的默认字段（defaultMetaFields）
 *
 * 通过运行时注册，共享表单文件不再内嵌具体供应商逻辑，减少与 main 的合并冲突。
 */
import type { ComponentType, LazyExoticComponent } from "react";
import type { ProviderMeta } from "@/types";
import type { FetchedModel } from "@/lib/api/model-fetch";

export interface ExtensionAuthSectionProps {
  selectedAccountId?: string | null;
  onAccountSelect?: (accountId: string | null) => void;
}

/** 供应商专属认证区块使用受控的供应商账号绑定。 */
export type ExtensionAuthSection = LazyExoticComponent<
  ComponentType<ExtensionAuthSectionProps>
>;

export interface FormExtension {
  /** 该扩展适用的 providerType */
  providerType: string;
  /** 使用扩展认证区块时，常规 API Key 校验可跳过 */
  skipApiKeyRequired?: boolean;
  /** 供应商专属认证区块（懒加载组件） */
  AuthSection?: ExtensionAuthSection;
  /** 拉取该供应商可用模型列表 */
  fetchModels?: (accountId?: string | null) => Promise<FetchedModel[]>;
  /** 保存供应商时写入 meta 的默认字段（用户/预设已有值时以已有值为准） */
  defaultMetaFields?: Partial<ProviderMeta>;
}

const extensions = new Map<string, FormExtension>();

/** 注册（或覆盖）一个表单扩展。 */
export function registerFormExtension(extension: FormExtension): void {
  extensions.set(extension.providerType, extension);
}

/** 按 providerType 获取表单扩展。 */
export function getFormExtension(
  providerType: string | null | undefined,
): FormExtension | undefined {
  if (!providerType) return undefined;
  return extensions.get(providerType);
}
