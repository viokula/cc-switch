use crate::huawei::claude_config;
use crate::huawei::connectivity_proxy::{ConnectivityProxy, ConnectivityProxyHandle};
use crate::huawei::proxy_processor;
use crate::provider::ProviderMeta;
use crate::proxy::hooks::ProxyHook;
use crate::proxy::ProxyError;
use axum::http::HeaderMap;
use serde_json::Value;
use std::future::Future;
use std::sync::Mutex;
use tauri::AppHandle;

pub struct HuaweiCodeAgentProxyHook {
    proxy_handle: Mutex<Option<ConnectivityProxyHandle>>,
    proxy_port: u16,
}

impl HuaweiCodeAgentProxyHook {
    pub fn new() -> Self {
        Self {
            proxy_handle: Mutex::new(None),
            proxy_port: 8888,
        }
    }

    #[cfg(test)]
    pub fn with_port(port: u16) -> Self {
        Self {
            proxy_handle: Mutex::new(None),
            proxy_port: port,
        }
    }
}

// 生命周期入口既可能在同步命令线程，也可能在 Tokio 当前线程运行时中。
fn lifecycle_block_on<F>(future: F) -> Result<F::Output, String>
where
    F: Future + Send,
    F::Output: Send,
{
    std::thread::scope(|scope| {
        scope
            .spawn(|| tauri::async_runtime::block_on(future))
            .join()
            .map_err(|_| "连通性代理生命周期任务异常退出".to_string())
    })
}

impl Default for HuaweiCodeAgentProxyHook {
    fn default() -> Self {
        Self::new()
    }
}

impl ProxyHook for HuaweiCodeAgentProxyHook {
    fn matches_provider(&self, meta: &ProviderMeta) -> bool {
        matches!(
            meta.provider_type.as_deref(),
            Some("huawei_codeagent_2")
                | Some("huawei_codeagent_3")
                | Some("huawei_w3")
                | Some("huawei_idea")
        )
    }

    fn is_managed_auth(&self) -> bool {
        true
    }

    fn skip_auth_token(&self) -> bool {
        true
    }

    fn force_api_format(&self, meta: &ProviderMeta) -> Option<String> {
        if self.matches_provider(meta) {
            Some("openai_chat".to_string())
        } else {
            None
        }
    }

    fn on_request_body(&self, body: &mut Value, meta: &ProviderMeta) {
        proxy_processor::apply_fingerprint_replacements_to_body(body, &meta.extra);
    }

    fn on_auth_headers(
        &self,
        meta: &ProviderMeta,
        app_handle: Option<&AppHandle>,
        body: Option<&Value>,
    ) -> crate::proxy::hooks::AuthHeadersFuture {
        let extra = meta.extra.clone();
        let app_handle = app_handle.cloned();
        let body = body.cloned();
        Box::pin(async move {
            proxy_processor::resolve_extra_auth_headers(&extra, app_handle.as_ref(), body.as_ref())
                .await
                .map_err(ProxyError::AuthError)
        })
    }

    fn filter_outbound_headers(&self, meta: &ProviderMeta, headers: &mut HeaderMap) {
        let header_mode = proxy_processor::get_header_mode(&meta.extra)
            .unwrap_or_else(|| "passthrough".to_string());
        if header_mode != "whitelist" {
            return;
        }
        let allowed = proxy_processor::get_allowed_headers(&meta.extra).unwrap_or_default();
        if allowed.is_empty() {
            return;
        }
        let mut to_remove = vec![];
        for name in headers.keys() {
            let name_str = name.as_str();
            if !allowed.iter().any(|a| name_str.eq_ignore_ascii_case(a)) {
                to_remove.push(name.clone());
            }
        }
        for name in to_remove {
            headers.remove(name);
        }
    }

    fn on_build_url(&self, meta: &ProviderMeta, url: &mut String) {
        if let Some(prefix) = proxy_processor::get_strip_path_prefix(&meta.extra) {
            let prefix_with_slash = format!("{}/", prefix);
            if url.contains(&prefix_with_slash) {
                *url = url.replace(&prefix_with_slash, "/");
            }
        }
    }

    fn skip_tls_verify(&self, meta: &ProviderMeta) -> bool {
        proxy_processor::get_skip_tls_verify(&meta.extra)
    }

    fn on_response_body(&self, meta: &ProviderMeta, body: &[u8]) -> Option<Vec<u8>> {
        proxy_processor::apply_fingerprint_replacements_to_response(body, &meta.extra)
    }

    fn transform_upstream_error(&self, meta: &ProviderMeta, body: &Value) -> Option<ProxyError> {
        let error_format = proxy_processor::get_error_format(&meta.extra)
            .unwrap_or_else(|| "passthrough".to_string());
        if error_format != "to_anthropic" {
            return None;
        }
        if body.get("choices").is_some() {
            return None;
        }
        if let Some(error_code) = body.get("error_code").and_then(|v| v.as_str()) {
            let error_msg = body.get("error_msg").and_then(|v| v.as_str()).unwrap_or("");
            let status = if error_code.contains("429") {
                429
            } else if error_code.starts_with("4")
                || error_code.contains("400")
                || error_code.contains("401")
                || error_code.contains("403")
            {
                400
            } else {
                502
            };
            log::warn!("[Transform] 上游返回非标准错误响应 (error_code={error_code}): {error_msg}");
            return Some(ProxyError::UpstreamError {
                status,
                body: Some(format!(
                    "{{\"error_code\":\"{error_code}\",\"error_msg\":\"{error_msg}\"}}"
                )),
            });
        }
        if let Some(error_obj) = body.get("error") {
            let error_str = error_obj.to_string();
            let status = if error_str.contains("429") || error_str.contains("rate_limit") {
                429
            } else {
                502
            };
            log::warn!("[Transform] 上游返回 error 对象: {error_str}");
            return Some(ProxyError::UpstreamError {
                status,
                body: Some(error_str),
            });
        }
        None
    }

    fn on_takeover(&self, meta: &ProviderMeta) -> Result<(), String> {
        if meta.provider_type.as_deref() != Some("huawei_codeagent_3") {
            return Ok(());
        }

        // Desktop 是独立进程，不读 ~/.claude.json 的 HTTPS_PROXY，连通性代理对它无意义 —— 跳过。
        let is_desktop = meta
            .extra
            .get("appTarget")
            .and_then(|v| v.as_str())
            .is_some_and(|s| s == "claude_desktop");
        if is_desktop {
            return Ok(());
        }

        {
            let mut guard = self.proxy_handle.lock().map_err(|e| e.to_string())?;
            if guard.is_some() {
                log::info!("[HuaweiCodeAgentProxyHook] 连通性代理已运行，跳过启动");
            } else {
                let proxy = ConnectivityProxy::with_port(self.proxy_port);
                let handle = lifecycle_block_on(proxy.start())?
                    .map_err(|e| format!("启动连通性代理失败: {e}"))?;
                log::info!(
                    "[HuaweiCodeAgentProxyHook] 连通性代理已启动: {}",
                    handle.port
                );
                *guard = Some(handle);
            }
        }

        if let Err(e) = claude_config::approve_proxy_managed_key() {
            log::error!("[HuaweiCodeAgentProxyHook] 写入 PROXY_MANAGED 失败，回滚连通性代理: {e}");
            let mut guard = self.proxy_handle.lock().map_err(|e| e.to_string())?;
            if let Some(handle) = guard.take() {
                lifecycle_block_on(handle.stop())?;
            }
            return Err(format!("写入 PROXY_MANAGED 到 ~/.claude.json 失败: {e}"));
        }

        Ok(())
    }

    fn on_selection_changed(&self, app: &str, meta: Option<&ProviderMeta>) -> Result<(), String> {
        if app != "claude" {
            return Ok(());
        }
        if let Some(meta) =
            meta.filter(|meta| meta.provider_type.as_deref() == Some("huawei_codeagent_3"))
        {
            let mut meta = meta.clone();
            meta.extra
                .insert("appTarget".into(), Value::String(app.into()));
            return self.on_takeover(&meta);
        }
        let running = self
            .proxy_handle
            .lock()
            .map_err(|e| e.to_string())?
            .is_some();
        if running {
            self.on_restore(&ProviderMeta {
                provider_type: Some("huawei_codeagent_3".into()),
                ..Default::default()
            })?;
        }
        Ok(())
    }

    fn on_restore(&self, meta: &ProviderMeta) -> Result<(), String> {
        if meta.provider_type.as_deref() != Some("huawei_codeagent_3") {
            return Ok(());
        }

        // Desktop 从未启动连通性代理，on_restore 也跳过（不停止代理、不动 ~/.claude.json）。
        let is_desktop = meta
            .extra
            .get("appTarget")
            .and_then(|v| v.as_str())
            .is_some_and(|s| s == "claude_desktop");
        if is_desktop {
            return Ok(());
        }

        {
            let mut guard = self.proxy_handle.lock().map_err(|e| e.to_string())?;
            if let Some(handle) = guard.take() {
                lifecycle_block_on(handle.stop())?;
                log::info!("[HuaweiCodeAgentProxyHook] 连通性代理已停止");
            } else {
                log::info!("[HuaweiCodeAgentProxyHook] 连通性代理未运行，跳过停止");
            }
        }

        claude_config::remove_proxy_managed_key()
            .map_err(|e| format!("从 ~/.claude.json 移除 PROXY_MANAGED 失败: {e}"))?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::ProviderMeta;
    use serde_json::json;
    use serial_test::serial;
    use std::env;
    use std::sync::Once;
    use tempfile::TempDir;

    static CRYPTO_INIT: Once = Once::new();

    fn ensure_crypto_provider() {
        CRYPTO_INIT.call_once(|| {
            let _ = rustls::crypto::ring::default_provider().install_default();
        });
    }

    /// Isolate `~/.claude.json` writes by pointing `CC_SWITCH_TEST_HOME` at a
    /// temp dir. Restored on drop.
    struct TempHome {
        #[allow(dead_code)]
        dir: TempDir,
        original_test_home: Option<String>,
    }

    impl TempHome {
        fn new() -> Self {
            let dir = TempDir::new().expect("failed to create temp home");
            let original_test_home = env::var("CC_SWITCH_TEST_HOME").ok();
            env::set_var("CC_SWITCH_TEST_HOME", dir.path());
            Self {
                dir,
                original_test_home,
            }
        }
    }

    impl Drop for TempHome {
        fn drop(&mut self) {
            match &self.original_test_home {
                Some(value) => env::set_var("CC_SWITCH_TEST_HOME", value),
                None => env::remove_var("CC_SWITCH_TEST_HOME"),
            }
        }
    }

    fn meta_with_app_target(provider_type: &str, app_target: &str) -> ProviderMeta {
        let mut meta = ProviderMeta::default();
        meta.provider_type = Some(provider_type.to_string());
        meta.extra
            .insert("appTarget".to_string(), json!(app_target));
        meta
    }

    #[test]
    #[serial]
    fn selection_reconciliation_works_without_runtime_and_isolates_desktop() {
        let _home = TempHome::new();
        crate::settings::reload_settings().unwrap();
        ensure_crypto_provider();
        let hook = HuaweiCodeAgentProxyHook::with_port(0);
        let meta = meta_with_app_target("huawei_codeagent_3", "claude_desktop");
        hook.on_selection_changed("claude", Some(&meta)).unwrap();
        let port = hook.proxy_handle.lock().unwrap().as_ref().unwrap().port;
        assert!(std::net::TcpStream::connect(("127.0.0.1", port)).is_ok());
        hook.on_selection_changed("claude_desktop", None).unwrap();
        assert!(hook.proxy_handle.lock().unwrap().is_some());
        hook.on_selection_changed("claude", None).unwrap();
        assert!(hook.proxy_handle.lock().unwrap().is_none());
        assert!(std::net::TcpListener::bind(("127.0.0.1", port)).is_ok());
    }

    #[test]
    #[serial]
    fn selection_reconciliation_reports_port_conflicts() {
        let _home = TempHome::new();
        crate::settings::reload_settings().unwrap();
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let hook = HuaweiCodeAgentProxyHook::with_port(listener.local_addr().unwrap().port());
        let meta = meta_with_app_target("huawei_codeagent_3", "claude");
        assert!(hook.on_selection_changed("claude", Some(&meta)).is_err());
        assert!(hook.proxy_handle.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn auth_header_errors_are_not_silently_ignored() {
        let hook = HuaweiCodeAgentProxyHook::new();
        let mut meta = meta_with_app_target("huawei_codeagent_2", "claude");
        meta.extra.insert(
            "extraAuthHeaders".into(),
            json!([
                {"name": "Authorization", "tokenSource": "huawei_codeagent_2"}
            ]),
        );
        assert!(matches!(
            hook.on_auth_headers(&meta, None, None).await,
            Err(ProxyError::AuthError(_))
        ));
    }

    /// Desktop providers must NOT start the connectivity proxy or touch
    /// `~/.claude.json`. `on_takeover` returns `Ok(())` and leaves the proxy
    /// handle empty — nothing was started that needs stopping.
    #[tokio::test]
    #[serial]
    async fn on_takeover_skips_connectivity_proxy_for_desktop() {
        let _home = TempHome::new();
        let hook = HuaweiCodeAgentProxyHook::new();
        let meta = meta_with_app_target("huawei_codeagent_3", "claude_desktop");

        let result = hook.on_takeover(&meta);
        assert!(
            result.is_ok(),
            "on_takeover should Ok for desktop: {:?}",
            result.err()
        );

        // No connectivity proxy should have been started: the handle stays None.
        let guard = hook.proxy_handle.lock().expect("lock not poisoned");
        assert!(
            guard.is_none(),
            "desktop must not start the connectivity proxy"
        );
        // `~/.claude.json` must not gain a PROXY_MANAGED key for desktop.
        let claude_path = crate::config::get_claude_mcp_path();
        if claude_path.exists() {
            let content = std::fs::read_to_string(&claude_path).unwrap_or_default();
            assert!(
                !content.contains("PROXY_MANAGED"),
                "~/.claude.json should not be touched for desktop, got: {content}"
            );
        }
    }

    /// `on_restore` for desktop must be a no-op (it never started anything).
    #[tokio::test]
    #[serial]
    async fn on_restore_skips_connectivity_proxy_for_desktop() {
        let _home = TempHome::new();
        let hook = HuaweiCodeAgentProxyHook::new();
        let meta = meta_with_app_target("huawei_codeagent_3", "claude_desktop");

        let result = hook.on_restore(&meta);
        assert!(
            result.is_ok(),
            "on_restore should Ok for desktop: {:?}",
            result.err()
        );
        let guard = hook.proxy_handle.lock().expect("lock not poisoned");
        assert!(
            guard.is_none(),
            "desktop on_restore must not interact with the connectivity proxy"
        );
    }

    /// CLI providers (appTarget="claude") keep the existing behavior:
    /// `on_takeover` starts the connectivity proxy on an ephemeral port and
    /// writes PROXY_MANAGED to `~/.claude.json`. `on_restore` tears it down.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[serial]
    async fn on_takeover_runs_connectivity_proxy_for_cli() {
        let _home = TempHome::new();
        ensure_crypto_provider();
        // Ephemeral port so we don't collide with a real 8888 binding.
        let hook = HuaweiCodeAgentProxyHook::with_port(0);
        let meta = meta_with_app_target("huawei_codeagent_3", "claude");

        let takeover = hook.on_takeover(&meta);
        assert!(
            takeover.is_ok(),
            "on_takeover should Ok for CLI: {:?}",
            takeover.err()
        );
        // The connectivity proxy must have been started.
        {
            let guard = hook.proxy_handle.lock().expect("lock not poisoned");
            assert!(guard.is_some(), "CLI must start the connectivity proxy");
        }
        // `~/.claude.json` must now carry PROXY_MANAGED.
        let claude_path = crate::config::get_claude_mcp_path();
        let content = std::fs::read_to_string(&claude_path).expect("claude.json written");
        assert!(
            content.contains("PROXY_MANAGED"),
            "CLI on_takeover should write PROXY_MANAGED to ~/.claude.json"
        );

        // Teardown: on_restore stops the proxy + removes the key.
        let restore = hook.on_restore(&meta);
        assert!(
            restore.is_ok(),
            "on_restore should Ok for CLI: {:?}",
            restore.err()
        );
        {
            let guard = hook.proxy_handle.lock().expect("lock not poisoned");
            assert!(
                guard.is_none(),
                "on_restore must stop the connectivity proxy"
            );
        }
    }
}
