/**
 * 授权中心区块注册表
 *
 * 允许独立供应商模块在运行时向「授权中心」面板追加账号管理区块，
 * 无需修改主面板文件，减少与 main 的合并冲突。
 */
import type { ComponentType, LazyExoticComponent } from "react";

export interface AuthCenterSection {
  /** 唯一标识（用于 key 与排序） */
  id: string;
  /** 区块组件（懒加载） */
  Component: LazyExoticComponent<ComponentType<Record<string, unknown>>>;
}

const sections: AuthCenterSection[] = [];

/** 注册一个授权中心区块（按注册顺序渲染）。 */
export function registerAuthCenterSection(section: AuthCenterSection): void {
  if (sections.some((s) => s.id === section.id)) return;
  sections.push(section);
}

/** 获取所有已注册的授权中心区块。 */
export function getAuthCenterSections(): AuthCenterSection[] {
  return sections;
}
