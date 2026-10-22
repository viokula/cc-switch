use super::manager::{
    HuaweiCodeAgentAccount, HuaweiCodeAgentAuthManager, HuaweiCodeAgentAuthStatus,
};
use crate::error::AppError;
use crate::services::model_fetch::FetchedModel;
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::process::Command as StdCommand;
use std::sync::Arc;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

pub struct HuaweiCodeAgentAuthState(pub Arc<HuaweiCodeAgentAuthManager>);

#[tauri::command]
pub async fn huawei_codeagent_2_start_login(
    user: String,
    password: String,
    state: tauri::State<'_, HuaweiCodeAgentAuthState>,
) -> Result<HuaweiCodeAgentAuthStatus, AppError> {
    let token = state.0.login(&user, &password).await?;
    Ok(HuaweiCodeAgentAuthStatus {
        authenticated: true,
        user: Some(token.user),
        expires_at: Some(token.expires_at),
        account_id: Some(token.account_id),
    })
}

#[tauri::command]
pub async fn huawei_codeagent_2_get_status(
    state: tauri::State<'_, HuaweiCodeAgentAuthState>,
) -> Result<HuaweiCodeAgentAuthStatus, AppError> {
    Ok(state.0.get_status().await)
}

#[tauri::command]
pub async fn huawei_codeagent_2_logout(
    state: tauri::State<'_, HuaweiCodeAgentAuthState>,
) -> Result<(), AppError> {
    state.0.logout().await
}

#[tauri::command]
pub async fn huawei_codeagent_2_refresh_token(
    account_id: String,
    state: tauri::State<'_, HuaweiCodeAgentAuthState>,
) -> Result<HuaweiCodeAgentAuthStatus, AppError> {
    let token = state.0.refresh_token(&account_id).await?;
    Ok(HuaweiCodeAgentAuthStatus {
        authenticated: true,
        user: Some(token.user),
        expires_at: Some(token.expires_at),
        account_id: Some(token.account_id),
    })
}

#[tauri::command(rename_all = "camelCase")]
pub async fn huawei_codeagent_2_list_accounts(
    state: tauri::State<'_, HuaweiCodeAgentAuthState>,
) -> Result<Vec<HuaweiCodeAgentAccount>, AppError> {
    Ok(state.0.list_accounts().await)
}

#[tauri::command(rename_all = "camelCase")]
pub async fn huawei_codeagent_2_remove_account(
    account_id: String,
    state: tauri::State<'_, HuaweiCodeAgentAuthState>,
) -> Result<(), AppError> {
    state.0.remove_account(&account_id).await
}

#[tauri::command(rename_all = "camelCase")]
pub async fn huawei_codeagent_2_set_default_account(
    account_id: String,
    state: tauri::State<'_, HuaweiCodeAgentAuthState>,
) -> Result<(), AppError> {
    state.0.set_default_account(&account_id).await
}

#[tauri::command(rename_all = "camelCase")]
pub async fn huawei_codeagent_2_fetch_models(
    account_id: Option<String>,
    state: tauri::State<'_, HuaweiCodeAgentAuthState>,
) -> Result<Vec<FetchedModel>, AppError> {
    authenticate_models_account(&state.0, account_id).await?;

    match fetch_models_via_nga().await {
        Ok(models) if !models.is_empty() => Ok(models),
        _ => Ok(fallback_models()),
    }
}

async fn authenticate_models_account(
    manager: &Arc<HuaweiCodeAgentAuthManager>,
    account_id: Option<String>,
) -> Result<String, AppError> {
    // An explicit binding is authoritative. Let get_token_with_refresh reject
    // removed accounts instead of silently using the default account.
    let account_id = match account_id {
        Some(account_id) => account_id,
        None => manager
            .get_status()
            .await
            .account_id
            .ok_or_else(|| AppError::Message("华为 CodeAgent 未登录，请先登录".to_string()))?,
    };
    manager
        .get_token_with_refresh(&account_id)
        .await
        .map_err(|e| AppError::Message(format!("获取华为 CodeAgent Token 失败: {e}")))?;
    Ok(account_id)
}

fn nga_models_command() -> StdCommand {
    #[cfg(windows)]
    {
        let mut command = StdCommand::new("cmd");
        command.args(["/C", "nga", "models"]);
        command.creation_flags(CREATE_NO_WINDOW);
        command
    }
    #[cfg(not(windows))]
    {
        let mut command = StdCommand::new("nga");
        command.arg("models");
        command
    }
}

async fn fetch_models_via_nga() -> Result<Vec<FetchedModel>, AppError> {
    let output = tokio::task::spawn_blocking(|| nga_models_command().output())
        .await
        .map_err(|e| AppError::Message(format!("执行 nga models 失败: {}", e)))?
        .map_err(|e| AppError::Message(format!("执行 nga models 失败: {}", e)))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(AppError::Message(format!(
            "nga models 命令执行失败: {}",
            stderr.trim()
        )));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut models = Vec::new();

    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('{') {
            continue;
        }
        let model_id = if let Some(slash_pos) = line.find('/') {
            &line[slash_pos + 1..]
        } else {
            line
        };
        if !model_id.is_empty() {
            models.push(FetchedModel {
                id: model_id.to_string(),
                owned_by: Some("Huawei CodeAgent".to_string()),
            });
        }
    }

    models.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(models)
}

fn fallback_models() -> Vec<FetchedModel> {
    vec![FetchedModel {
        id: "MiniMax-M2.7".to_string(),
        owned_by: Some("Huawei CodeAgent".to_string()),
    }]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    // Only synthetic tokens in an isolated directory; no real account store or
    // network request is used by these selection tests.
    fn models_manager(default_expires_at: i64) -> (TempDir, Arc<HuaweiCodeAgentAuthManager>) {
        let dir = TempDir::new().expect("isolated model account store");
        let tokens = json!({
            "idea_default": {
                "account_id": "idea_default",
                "user": "default-user",
                "token": "fake-default-token",
                "expires_at": default_expires_at,
                "is_default": true
            },
            "idea_selected": {
                "account_id": "idea_selected",
                "user": "selected-user",
                "token": "fake-selected-token",
                "expires_at": 9999999999i64,
                "is_default": false
            }
        });
        std::fs::write(
            dir.path().join("huawei_codeagent_2_tokens.json"),
            serde_json::to_vec(&tokens).unwrap(),
        )
        .expect("write synthetic tokens");
        let manager = Arc::new(HuaweiCodeAgentAuthManager::new(dir.path().to_path_buf()));
        manager.load_tokens_sync().expect("load synthetic tokens");
        (dir, manager)
    }

    #[tokio::test]
    async fn models_uses_bound_account_even_when_default_is_expired() {
        let (_dir, manager) = models_manager(1000);
        let account = authenticate_models_account(&manager, Some("idea_selected".to_string()))
            .await
            .expect("valid bound account must not depend on default status");
        assert_eq!(account, "idea_selected");
    }

    #[tokio::test]
    async fn models_without_binding_uses_default_account() {
        let (_dir, manager) = models_manager(9999999999);
        let account = authenticate_models_account(&manager, None)
            .await
            .expect("default account");
        assert_eq!(account, "idea_default");
    }

    #[tokio::test]
    async fn models_missing_binding_does_not_fall_back_to_default() {
        let (_dir, manager) = models_manager(9999999999);
        let error = authenticate_models_account(&manager, Some("idea_removed".to_string()))
            .await
            .expect_err("removed bound account must fail");
        assert!(error.to_string().contains("idea_removed"));
    }

    #[tokio::test]
    async fn models_expired_account_enters_refresh_instead_of_status_gate() {
        let (_dir, manager) = models_manager(1000);
        // No password is stored, so refresh fails locally without network I/O.
        let error = authenticate_models_account(&manager, None)
            .await
            .expect_err("expired default without password cannot refresh");
        assert!(error.to_string().contains("无存储密码"));
    }

    #[test]
    fn models_command_uses_platform_launcher() {
        let command = nga_models_command();
        let args: Vec<_> = command.get_args().collect();
        #[cfg(windows)]
        {
            assert_eq!(command.get_program(), std::ffi::OsStr::new("cmd"));
            assert_eq!(args, ["/C", "nga", "models"]);
        }
        #[cfg(not(windows))]
        {
            assert_eq!(command.get_program(), std::ffi::OsStr::new("nga"));
            assert_eq!(args, ["models"]);
        }
    }
}
