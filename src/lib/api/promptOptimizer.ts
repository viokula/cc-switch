import { invoke } from "@tauri-apps/api/core";

export interface OptimizePromptParams {
  app: string;
  providerId: string;
  model?: string;
  text: string;
}

export const promptOptimizerApi = {
  /** 用指定供应商把提示词改写一次，返回改写后的文本。 */
  optimize(params: OptimizePromptParams): Promise<string> {
    return invoke("optimize_prompt", {
      app: params.app,
      providerId: params.providerId,
      model: params.model,
      text: params.text,
    });
  },
};
