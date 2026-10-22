//! 供应商代理钩子框架
//!
//! `ProxyHook` 允许独立的供应商模块（如华为 CodeAgent）在转发管线中插入
//! 供应商特有的行为：认证头注入、请求/响应体改写、URL 重写、TLS 校验跳过、
//! 上游错误格式转换，以及供应商接管（takeover）/恢复（restore）生命周期。
//!
//! 钩子通过 [`HookRegistry::global`] 在应用启动时注册一次，注册后不可变更，
//! 因此注册表返回的是 `&'static dyn ProxyHook`，可以被转发任务在跨越 await
//! 时安全持有。

use crate::provider::ProviderMeta;
use crate::proxy::ProxyError;
use once_cell::sync::Lazy;
use std::future::Future;
use std::pin::Pin;
use std::sync::RwLock;
use tauri::AppHandle;

pub type AuthHeadersFuture =
    Pin<Box<dyn Future<Output = Result<Vec<(String, String)>, ProxyError>> + Send>>;

/// 供应商代理钩子。
///
/// 所有方法都有默认实现；供应商模块只需覆盖自己关心的方法。
/// 通过 [`ProxyHook::matches_provider`] 判断某个钩子是否适用于给定供应商。
pub trait ProxyHook: Send + Sync {
    /// 该钩子是否适用于给定供应商。注册表用返回值选择唯一的钩子。
    fn matches_provider(&self, meta: &ProviderMeta) -> bool;

    /// 该供应商是否使用托管认证（真实凭据由代理注入，而非写在供应商配置里）。
    fn is_managed_auth(&self) -> bool {
        false
    }

    /// 代理是否应跳过常规的 API Key/Token 注入（凭据由 `on_auth_headers` 提供）。
    fn skip_auth_token(&self) -> bool {
        false
    }

    /// 强制使用的上游 API 格式（覆盖供应商元数据里的 `apiFormat`）。
    fn force_api_format(&self, _meta: &ProviderMeta) -> Option<String> {
        None
    }

    /// 在请求体定稿发往上游前改写它（例如指纹替换）。
    fn on_request_body(&self, _body: &mut serde_json::Value, _meta: &ProviderMeta) {}

    /// 解析需要注入到上游请求的认证头。
    ///
    /// 返回 `(header_name, header_value)` 列表。因为可能涉及异步的 token 刷新，
    /// 返回一个 `Send` 的 future。
    fn on_auth_headers(
        &self,
        _meta: &ProviderMeta,
        _app_handle: Option<&AppHandle>,
        _body: Option<&serde_json::Value>,
    ) -> AuthHeadersFuture {
        Box::pin(async { Ok(Vec::new()) })
    }

    /// 过滤出站请求头（例如白名单模式）。
    fn filter_outbound_headers(&self, _meta: &ProviderMeta, _headers: &mut axum::http::HeaderMap) {}

    /// 在 URL 定稿前改写它（例如剥离路径前缀）。
    fn on_build_url(&self, _meta: &ProviderMeta, _url: &mut String) {}

    /// 该供应商的上游连接是否跳过 TLS 证书校验。
    fn skip_tls_verify(&self, _meta: &ProviderMeta) -> bool {
        false
    }

    /// 改写上游响应体（非流式路径）。返回 `None` 表示不修改。
    fn on_response_body(&self, _meta: &ProviderMeta, _body: &[u8]) -> Option<Vec<u8>> {
        None
    }

    /// 把非标准的上游错误响应转换成 `ProxyError`。返回 `None` 表示沿用默认处理。
    fn transform_upstream_error(
        &self,
        _meta: &ProviderMeta,
        _body: &serde_json::Value,
    ) -> Option<ProxyError> {
        None
    }

    /// 供应商被切换为当前供应商时调用（例如启动连通性代理、写入 live 配置）。
    fn on_takeover(&self, _meta: &ProviderMeta) -> Result<(), String> {
        Ok(())
    }

    /// 供应商被切走或应用退出时调用，用于回滚 [`ProxyHook::on_takeover`] 的副作用。
    fn on_restore(&self, _meta: &ProviderMeta) -> Result<(), String> {
        Ok(())
    }

    /// 统一提交或启动恢复后，按实际生效的供应商协调进程级资源。
    fn on_selection_changed(&self, _app: &str, _meta: Option<&ProviderMeta>) -> Result<(), String> {
        Ok(())
    }
}

/// 全局钩子注册表。
pub struct HookRegistry {
    hooks: RwLock<Vec<&'static dyn ProxyHook>>,
}

impl HookRegistry {
    fn new() -> Self {
        Self {
            hooks: RwLock::new(Vec::new()),
        }
    }

    /// 全局单例。
    pub fn global() -> &'static HookRegistry {
        static REGISTRY: Lazy<HookRegistry> = Lazy::new(HookRegistry::new);
        &REGISTRY
    }

    /// 注册一个钩子。注册后钩子会被泄漏为 `'static`，以便转发任务在跨越 await
    /// 时持有引用。注册只应在应用启动阶段发生一次。
    pub fn register(&self, hook: Box<dyn ProxyHook>) {
        let leaked: &'static dyn ProxyHook = Box::leak(hook);
        match self.hooks.write() {
            Ok(mut hooks) => hooks.push(leaked),
            Err(poisoned) => poisoned.into_inner().push(leaked),
        }
    }

    /// 找到适用于给定供应商的钩子。
    pub fn find_for(&self, meta: &ProviderMeta) -> Option<&'static dyn ProxyHook> {
        let hooks = match self.hooks.read() {
            Ok(hooks) => hooks,
            Err(poisoned) => poisoned.into_inner(),
        };
        hooks
            .iter()
            .copied()
            .find(|hook| hook.matches_provider(meta))
    }

    pub fn reconcile(
        &self,
        db: &crate::database::Database,
        app: &str,
    ) -> Result<(), crate::error::AppError> {
        if self.is_empty() {
            return Ok(());
        }
        let app_type = app.parse()?;
        let provider = crate::mode::current::provider_in_use(db, &app_type)?;
        let hooks = match self.hooks.read() {
            Ok(hooks) => hooks.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        };
        for hook in hooks {
            hook.on_selection_changed(app, provider.as_ref().and_then(|p| p.meta.as_ref()))
                .map_err(crate::error::AppError::Message)?;
        }
        Ok(())
    }

    pub fn release(&self, app: &str) -> Result<(), String> {
        let hooks = match self.hooks.read() {
            Ok(hooks) => hooks.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        };
        for hook in hooks {
            hook.on_selection_changed(app, None)?;
        }
        Ok(())
    }

    /// 是否注册了任何钩子（避免热路径上的无谓查找）。
    pub fn is_empty(&self) -> bool {
        match self.hooks.read() {
            Ok(hooks) => hooks.is_empty(),
            Err(poisoned) => poisoned.into_inner().is_empty(),
        }
    }
}
