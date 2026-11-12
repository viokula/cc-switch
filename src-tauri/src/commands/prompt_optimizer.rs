use crate::app_config::AppType;
use crate::error::AppError;
use crate::services::prompt_optimizer;
use crate::store::AppState;
use tauri::State;

/// 用指定供应商把提示词改写一次，返回改写后的文本。
#[tauri::command(rename_all = "camelCase")]
pub async fn optimize_prompt(
    app_handle: tauri::AppHandle,
    state: State<'_, AppState>,
    app: String,
    provider_id: String,
    model: Option<String>,
    text: String,
) -> Result<String, AppError> {
    let app_type: AppType = serde_json::from_value(serde_json::json!(app))
        .map_err(|_| AppError::InvalidInput(format!("unknown app type: {app}")))?;
    let providers = state.db.get_all_providers(app_type.as_str())?;
    let provider = providers.get(&provider_id).cloned().ok_or_else(|| {
        AppError::InvalidInput(format!(
            "provider not found for {}: {provider_id}",
            app_type.as_str()
        ))
    })?;

    prompt_optimizer::optimize_prompt(
        state.db.clone(),
        app_handle,
        app_type,
        provider,
        model,
        &text,
    )
    .await
}
