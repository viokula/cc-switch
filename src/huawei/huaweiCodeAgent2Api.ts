import { invoke } from "@tauri-apps/api/core";
import type {
  HuaweiCodeAgentAuthStatus,
  HuaweiCodeAgentAccount,
  FetchedModel,
} from "./types";

export const huaweiCodeAgent2Api = {
  async startLogin(
    user: string,
    password: string,
  ): Promise<HuaweiCodeAgentAuthStatus> {
    return await invoke("huawei_codeagent_2_start_login", { user, password });
  },

  async getStatus(): Promise<HuaweiCodeAgentAuthStatus> {
    return await invoke("huawei_codeagent_2_get_status");
  },

  async logout(): Promise<void> {
    await invoke("huawei_codeagent_2_logout");
  },

  async refreshToken(accountId: string): Promise<HuaweiCodeAgentAuthStatus> {
    return await invoke("huawei_codeagent_2_refresh_token", { accountId });
  },

  async listAccounts(): Promise<HuaweiCodeAgentAccount[]> {
    return await invoke("huawei_codeagent_2_list_accounts");
  },

  async removeAccount(accountId: string): Promise<void> {
    await invoke("huawei_codeagent_2_remove_account", { accountId });
  },

  async setDefaultAccount(accountId: string): Promise<void> {
    await invoke("huawei_codeagent_2_set_default_account", { accountId });
  },
};

export async function fetchHuaweiCodeAgent2Models(
  accountId?: string | null,
): Promise<FetchedModel[]> {
  return invoke("huawei_codeagent_2_fetch_models", {
    accountId: accountId ?? null,
  });
}
