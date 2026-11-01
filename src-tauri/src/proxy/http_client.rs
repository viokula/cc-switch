//! 全局 HTTP 客户端模块
//!
//! 提供支持全局代理配置的 HTTP 客户端。
//! 所有需要发送 HTTP 请求的模块都应使用此模块提供的客户端。

use base64::{engine::general_purpose::STANDARD, Engine};
use hyper_util::client::proxy::matcher::{Intercept, Matcher};
use once_cell::sync::OnceCell;
use reqwest::Client;
use std::env;
use std::net::IpAddr;
use std::sync::{Arc, LazyLock, Mutex, RwLock};
use std::time::Duration;

#[derive(Clone)]
struct ClientState {
    client: Client,
    policy: Arc<ProxyPolicy>,
}

static GLOBAL_CLIENT: OnceCell<RwLock<ClientState>> = OnceCell::new();

struct ProxyPolicy {
    configured_url: Option<String>,
    matcher: Matcher,
    automatic_disabled: bool,
    windows_bypass: Option<String>,
}

impl ProxyPolicy {
    fn new(proxy_url: Option<&str>) -> Result<Self, String> {
        if let Some(value) = proxy_url {
            let parsed = url::Url::parse(value)
                .map_err(|e| format!("Invalid proxy URL '{}': {e}", mask_url(value)))?;
            if !["http", "https", "socks5", "socks5h"].contains(&parsed.scheme()) {
                return Err(format!(
                    "Invalid proxy scheme '{}' in URL '{}'. Supported: http, https, socks5, socks5h",
                    parsed.scheme(),
                    mask_url(value)
                ));
            }
            reqwest::Proxy::all(parsed.as_str())
                .map_err(|e| format!("Invalid proxy URL '{}': {e}", mask_url(value)))?;
            return Ok(Self {
                configured_url: Some(value.to_owned()),
                matcher: Matcher::builder().all(parsed.as_str()).build(),
                automatic_disabled: false,
                windows_bypass: None,
            });
        }
        Ok(Self {
            configured_url: None,
            matcher: Matcher::from_system(),
            automatic_disabled: system_proxy_points_to_loopback(),
            windows_bypass: system_proxy_bypass_rules(),
        })
    }

    fn proxy_for(&self, url: &str) -> Option<Intercept> {
        if is_loopback_url(url) {
            return None;
        }
        if self.configured_url.is_none()
            && (self.automatic_disabled
                || self
                    .windows_bypass
                    .as_deref()
                    .is_some_and(|rules| matches_windows_proxy_bypass(url, rules)))
        {
            return None;
        }
        self.matcher.intercept(&url.parse().ok()?)
    }

    fn build_client(self: &Arc<Self>, builder: reqwest::ClientBuilder) -> Result<Client, String> {
        let policy = self.clone();
        builder
            .no_proxy()
            .proxy(reqwest::Proxy::custom(move |url| {
                policy
                    .proxy_for(url.as_str())
                    .as_ref()
                    .map(proxy_url_with_auth)
            }))
            .build()
            .map_err(|e| format!("Failed to build HTTP client: {e}"))
    }
}

// Matcher separates credentials from its URI; custom reqwest proxies take a URL.
fn proxy_url_with_auth(proxy: &Intercept) -> url::Url {
    let mut url = url::Url::parse(&proxy.uri().to_string()).expect("matcher returns a proxy URI");
    let basic = proxy.basic_auth().map(|auth| {
        let bytes = STANDARD
            .decode(&auth.as_bytes()[6..])
            .expect("matcher produces Basic authentication");
        String::from_utf8(bytes).expect("matcher decodes credentials as UTF-8")
    });
    let auth = proxy
        .raw_auth()
        .or_else(|| basic.as_deref().and_then(|value| value.split_once(':')));
    if let Some((user, password)) = auth {
        // URL setters preserve percent escapes; credentials here are already decoded.
        url.set_username(&user.replace('%', "%25"))
            .expect("proxy URI has an authority");
        url.set_password(Some(&password.replace('%', "%25")))
            .expect("proxy URI has an authority");
    }
    url
}

fn build_state(proxy_url: Option<&str>) -> Result<ClientState, String> {
    let policy = Arc::new(ProxyPolicy::new(proxy_url)?);
    let client = policy.build_client(base_builder())?;
    Ok(ClientState { client, policy })
}

pub struct RequestRoute {
    pub client: Client,
    pub proxy: Option<Intercept>,
}

/// 不走任何代理的客户端：上游是本机地址（Ollama、LM Studio 等）时使用
static DIRECT_CLIENT: OnceCell<Client> = OnceCell::new();

// Certificate overrides never share pools with verified clients. At most two
// override pools are retained: direct routes and the current proxy configuration.
// Authentication remains on individual requests, never in client defaults.
static OVERRIDE_CLIENTS: LazyLock<Mutex<TlsOverrideClients>> =
    LazyLock::new(|| Mutex::new(TlsOverrideClients::default()));

#[derive(Default)]
struct TlsOverrideClients {
    direct: Option<Client>,
    configured: Option<(Arc<ProxyPolicy>, Client)>,
}

impl TlsOverrideClients {
    fn get(&mut self, direct: bool, policy: &Arc<ProxyPolicy>) -> Result<Client, String> {
        if direct {
            if self.direct.is_none() {
                self.direct = Some(
                    base_builder()
                        .danger_accept_invalid_certs(true)
                        .no_proxy()
                        .build()
                        .map_err(|e| format!("Failed to build HTTP client: {e}"))?,
                );
            }
            return Ok(self.direct.as_ref().unwrap().clone());
        }
        if let Some((cached_policy, client)) = &self.configured {
            if Arc::ptr_eq(cached_policy, policy) {
                return Ok(client.clone());
            }
        }
        let client = policy.build_client(base_builder().danger_accept_invalid_certs(true))?;
        self.configured = Some((policy.clone(), client.clone()));
        Ok(client)
    }
}

fn invalidate_override_clients() {
    if let Ok(mut clients) = OVERRIDE_CLIENTS.lock() {
        clients.configured = None;
    }
}

/// CC Switch 代理服务器当前监听的端口
static CC_SWITCH_PROXY_PORT: OnceCell<RwLock<u16>> = OnceCell::new();

/// 设置 CC Switch 代理服务器的监听端口
///
/// 应在代理服务器启动时调用，以便系统代理检测能正确识别自己的端口
pub fn set_proxy_port(port: u16) {
    if let Some(lock) = CC_SWITCH_PROXY_PORT.get() {
        if let Ok(mut current_port) = lock.write() {
            *current_port = port;
            log::debug!("[GlobalProxy] Updated CC Switch proxy port to {port}");
        }
    } else {
        let _ = CC_SWITCH_PROXY_PORT.set(RwLock::new(port));
        log::debug!("[GlobalProxy] Initialized CC Switch proxy port to {port}");
    }
    invalidate_override_clients();
}

/// 获取 CC Switch 代理服务器的监听端口
fn get_proxy_port() -> u16 {
    CC_SWITCH_PROXY_PORT
        .get()
        .and_then(|lock| lock.read().ok())
        .map(|port| *port)
        .unwrap_or(15721) // 默认端口作为回退
}

/// 初始化全局 HTTP 客户端
///
/// 应在应用启动时调用一次。
///
/// # Arguments
/// * `proxy_url` - 代理 URL，如 `http://127.0.0.1:7890` 或 `socks5://127.0.0.1:1080`
///   传入 None 或空字符串表示跟随环境／系统代理
pub fn init(proxy_url: Option<&str>) -> Result<(), String> {
    let effective_url = proxy_url.filter(|s| !s.trim().is_empty());
    let state = build_state(effective_url)?;

    // 尝试初始化全局客户端，如果已存在则记录警告并使用 apply_proxy 更新
    if GLOBAL_CLIENT.set(RwLock::new(state)).is_err() {
        log::warn!(
            "[GlobalProxy] [GP-003] Already initialized, updating instead: {}",
            effective_url
                .map(mask_url)
                .unwrap_or_else(|| "automatic proxy policy".to_string())
        );
        // 已初始化，改用 apply_proxy 更新
        return apply_proxy(proxy_url);
    }

    invalidate_override_clients();

    log::info!(
        "[GlobalProxy] Initialized: {}",
        effective_url
            .map(mask_url)
            .unwrap_or_else(|| "automatic proxy policy".to_string())
    );

    Ok(())
}

/// 验证代理配置（不应用）
///
/// 只验证代理 URL 是否有效，不实际更新全局客户端。
/// 用于在持久化之前验证配置的有效性。
///
/// # Arguments
/// * `proxy_url` - 代理 URL，None 或空字符串表示跟随环境／系统代理
///
/// # Returns
/// 验证成功返回 Ok(())，失败返回错误信息
pub fn validate_proxy(proxy_url: Option<&str>) -> Result<(), String> {
    let effective_url = proxy_url.filter(|s| !s.trim().is_empty());
    // 只调用 build_client 来验证，但不应用
    build_client(effective_url)?;
    Ok(())
}

/// 应用代理配置（假设已验证）
///
/// 直接应用代理配置到全局客户端，不做额外验证。
/// 应在 validate_proxy 成功后调用。
///
/// # Arguments
/// * `proxy_url` - 代理 URL，None 或空字符串表示跟随环境／系统代理
pub fn apply_proxy(proxy_url: Option<&str>) -> Result<(), String> {
    let effective_url = proxy_url.filter(|s| !s.trim().is_empty());
    let new_state = build_state(effective_url)?;

    // 更新客户端
    if let Some(lock) = GLOBAL_CLIENT.get() {
        let mut client = lock.write().map_err(|e| {
            log::error!("[GlobalProxy] [GP-001] Failed to acquire write lock: {e}");
            "Failed to update proxy: lock poisoned".to_string()
        })?;
        *client = new_state;
    } else {
        // 如果还没初始化，则初始化
        return init(proxy_url);
    }

    invalidate_override_clients();

    log::info!(
        "[GlobalProxy] Applied: {}",
        effective_url
            .map(mask_url)
            .unwrap_or_else(|| "automatic proxy policy".to_string())
    );

    Ok(())
}

/// 更新代理配置（热更新）
///
/// 可在运行时调用以更改代理设置，无需重启应用。
/// 注意：此函数同时验证和应用，如果需要先验证后持久化再应用，
/// 请使用 validate_proxy + apply_proxy 组合。
///
/// # Arguments
/// * `proxy_url` - 新的代理 URL，None 或空字符串表示跟随环境／系统代理
#[allow(dead_code)]
pub fn update_proxy(proxy_url: Option<&str>) -> Result<(), String> {
    apply_proxy(proxy_url)
}

/// 获取全局 HTTP 客户端
///
/// 返回配置了代理的客户端（如果已配置代理），否则返回跟随系统代理的客户端。
pub fn get() -> Client {
    GLOBAL_CLIENT
        .get()
        .and_then(|lock| lock.read().ok())
        .map(|state| state.client.clone())
        .unwrap_or_else(|| {
            log::warn!("[GlobalProxy] [GP-004] Client not initialized, using fallback");
            build_client(None).unwrap_or_default()
        })
}

/// 按目标地址取客户端，使用与 Raw/Hyper 相同的代理策略。
pub fn get_for_url(url: &str) -> Client {
    get_for_url_with_tls(url, false).unwrap_or_else(|e| {
        log::warn!("[GlobalProxy] Failed to select HTTP client: {e}");
        get()
    })
}

#[cfg(windows)]
fn system_proxy_bypass_rules() -> Option<String> {
    use winreg::{enums::HKEY_CURRENT_USER, RegKey};
    let settings = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Internet Settings")
        .ok()?;
    if settings.get_value::<u32, _>("ProxyEnable").unwrap_or(0) == 0 {
        return None;
    }
    settings.get_value::<String, _>("ProxyOverride").ok()
}

#[cfg(not(windows))]
fn system_proxy_bypass_rules() -> Option<String> {
    None
}

fn matches_windows_proxy_bypass(url: &str, rules: &str) -> bool {
    let Ok(url) = url::Url::parse(url) else {
        return false;
    };
    let Some(host) = url.host_str() else {
        return false;
    };
    let host = host.trim_end_matches('.');
    rules.split(';').any(|rule| {
        let rule = rule.trim();
        if rule.eq_ignore_ascii_case("<local>") {
            return !host.contains('.') && !host.contains(':');
        }
        // <-loopback> controls Chromium's implicit bypass, not a host pattern.
        if rule.is_empty() || rule.starts_with('<') {
            return false;
        }
        let (scheme, pattern) = rule
            .split_once("://")
            .map_or((None, rule), |(scheme, pattern)| (Some(scheme), pattern));
        if scheme.is_some_and(|scheme| !scheme.eq_ignore_ascii_case(url.scheme())) {
            return false;
        }
        let (pattern, port) = pattern
            .rsplit_once(':')
            .filter(|(host, port)| !host.contains(':') && port.parse::<u16>().is_ok())
            .map_or((pattern, None), |(host, port)| {
                (host, port.parse::<u16>().ok())
            });
        if port.is_some() && port != url.port_or_known_default() {
            return false;
        }
        let pattern = regex::escape(pattern).replace(r"\*", ".*");
        regex::RegexBuilder::new(&format!("^{pattern}$"))
            .case_insensitive(true)
            .build()
            .map(|pattern| pattern.is_match(host))
            .unwrap_or(false)
    })
}

fn current_state() -> Result<ClientState, String> {
    if let Some(state) = GLOBAL_CLIENT.get() {
        return state
            .read()
            .map(|state| state.clone())
            .map_err(|_| "Failed to read HTTP client state".to_string());
    }
    build_state(None)
}

pub fn route_for_url(url: &str, skip_verify: bool) -> Result<RequestRoute, String> {
    let state = current_state()?;
    let proxy = state.policy.proxy_for(url);
    // Non-local direct routes keep the policy client for per-URL redirect routing.
    let local = is_loopback_url(url);
    let client = if skip_verify {
        OVERRIDE_CLIENTS
            .lock()
            .map_err(|_| "Failed to acquire TLS override client lock".to_string())?
            .get(local, &state.policy)?
    } else if local {
        direct_client()
    } else {
        state.client
    };
    let mode = if proxy.is_none() {
        "direct/bypass"
    } else if state.policy.configured_url.is_some() {
        "explicit proxy"
    } else {
        "automatic proxy"
    };
    log::debug!("[GlobalProxy] Route for {}: {mode}", mask_url(url));
    Ok(RequestRoute { client, proxy })
}

fn direct_client() -> Client {
    DIRECT_CLIENT
        .get_or_init(|| {
            base_builder().no_proxy().build().unwrap_or_else(|e| {
                log::warn!("[GlobalProxy] Failed to build direct client: {e}");
                Client::new()
            })
        })
        .clone()
}

/// 地址的主机是不是本机：`localhost`、`*.localhost`、`127.0.0.0/8`、`::1`
pub fn is_loopback_url(url: &str) -> bool {
    let Ok(parsed) = url::Url::parse(url.trim()) else {
        return false;
    };
    match parsed.host() {
        Some(url::Host::Domain(domain)) => host_is_loopback(domain),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => {
            ip.is_loopback() || ip.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback())
        }
        None => false,
    }
}

fn host_is_loopback(host: &str) -> bool {
    let host = host.trim_end_matches('.');
    if host.eq_ignore_ascii_case("localhost") || host.to_ascii_lowercase().ends_with(".localhost") {
        return true;
    }
    host.trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<IpAddr>()
        .map(|ip| ip.is_loopback())
        .unwrap_or(false)
}

/// 获取当前代理 URL
///
/// 返回当前配置的代理 URL，None 表示跟随环境／系统代理。
pub fn get_current_proxy_url() -> Option<String> {
    GLOBAL_CLIENT
        .get()
        .and_then(|lock| lock.read().ok())
        .and_then(|state| state.policy.configured_url.clone())
}

/// 检查是否正在使用代理
#[allow(dead_code)]
pub fn is_proxy_enabled() -> bool {
    get_current_proxy_url().is_some()
}

/// 两种客户端共用的参数
fn base_builder() -> reqwest::ClientBuilder {
    Client::builder()
        .timeout(Duration::from_secs(600))
        .connect_timeout(Duration::from_secs(30))
        .pool_max_idle_per_host(10)
        .tcp_keepalive(Duration::from_secs(60))
        // 禁用 reqwest 自动解压：防止 reqwest 覆盖客户端原始 accept-encoding header。
        // 响应解压由 response_processor 根据 content-encoding 手动处理。
        .no_gzip()
        .no_brotli()
        .no_deflate()
        .no_zstd()
}

/// 不跟随重定向的客户端，代理设置与全局客户端一致。带自定义凭据请求头（如 `x-dsh-auth-token`）
/// 的请求用它：reqwest 跨主机重定向时只剥掉标准认证头，自定义头会被带到新主机。
pub fn no_redirect() -> Result<Client, String> {
    current_state()?
        .policy
        .build_client(base_builder().redirect(reqwest::redirect::Policy::none()))
}

/// 构建 HTTP 客户端
fn build_client(proxy_url: Option<&str>) -> Result<Client, String> {
    Arc::new(ProxyPolicy::new(proxy_url)?).build_client(base_builder())
}

/// Apply a provider's TLS policy without sharing its pool with verified clients.
pub fn get_for_url_with_tls(url: &str, skip_verify: bool) -> Result<Client, String> {
    route_for_url(url, skip_verify).map(|route| route.client)
}

fn system_proxy_points_to_loopback() -> bool {
    const KEYS: [&str; 6] = [
        "HTTP_PROXY",
        "http_proxy",
        "HTTPS_PROXY",
        "https_proxy",
        "ALL_PROXY",
        "all_proxy",
    ];

    KEYS.iter()
        .filter_map(|key| env::var(key).ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .any(|value| proxy_points_to_loopback(&value))
}

fn proxy_points_to_loopback(value: &str) -> bool {
    // 检查是否指向 CC Switch 自己的代理端口
    // 只有指向自己的代理才需要跳过，避免递归
    fn is_cc_switch_proxy_port(port: Option<u16>) -> bool {
        let cc_switch_port = get_proxy_port();
        port == Some(cc_switch_port)
    }

    if let Ok(parsed) = url::Url::parse(value) {
        if let Some(host) = parsed.host_str() {
            // 只有当主机是 loopback 且端口是 CC Switch 的端口时才返回 true
            return host_is_loopback(host) && is_cc_switch_proxy_port(parsed.port());
        }
        return false;
    }

    let with_scheme = format!("http://{value}");
    if let Ok(parsed) = url::Url::parse(&with_scheme) {
        if let Some(host) = parsed.host_str() {
            return host_is_loopback(host) && is_cc_switch_proxy_port(parsed.port());
        }
    }

    false
}

/// 隐藏 URL 中的敏感信息（用于日志）
pub fn mask_url(url: &str) -> String {
    if let Ok(parsed) = url::Url::parse(url) {
        // 隐藏用户名和密码，保留 scheme、host 和端口
        let host = parsed.host_str().unwrap_or("?");
        match parsed.port() {
            Some(port) => format!("{}://{}:{}", parsed.scheme(), host, port),
            None => format!("{}://{}", parsed.scheme(), host),
        }
    } else {
        // URL 解析失败，返回部分内容。截断点回退到最近的字符边界，
        // 避免在多字节 UTF-8 字符中间切割导致 panic。
        if url.len() > 20 {
            let cut = (0..=20)
                .rev()
                .find(|&i| url.is_char_boundary(i))
                .unwrap_or(0);
            format!("{}...", &url[..cut])
        } else {
            url.to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    #[test]
    fn test_windows_proxy_bypass_rules() {
        let rules = "*.huawei.com;127.0.0.1*;<local>;<-loopback>";
        assert!(matches_windows_proxy_bypass(
            "https://snapengine.cida.cce.prod-szv-g.dragon.tools.huawei.com/api/v2",
            rules
        ));
        assert!(matches_windows_proxy_bypass(
            "https://API.HUAWEI.COM./v1",
            rules
        ));
        assert!(matches_windows_proxy_bypass(
            "http://127.0.0.1:15721",
            rules
        ));
        assert!(matches_windows_proxy_bypass("http://intranet/api", rules));
        for url in [
            "https://huawei.com.evil.test",
            "https://evilhuawei.com",
            "https://api.example.com",
        ] {
            assert!(!matches_windows_proxy_bypass(url, rules));
        }
        assert!(matches_windows_proxy_bypass(
            "https://api.example.com",
            "https://api.example.com:443"
        ));
        assert!(!matches_windows_proxy_bypass(
            "http://api.example.com",
            "https://api.example.com:443"
        ));
        assert!(!matches_windows_proxy_bypass(
            "https://api.example.com:8443",
            "api.example.com:443"
        ));
        assert!(!matches_windows_proxy_bypass(
            "http://127.0.0.1",
            "<-loopback>"
        ));
        assert!(!matches_windows_proxy_bypass("invalid", "*"));
    }
    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn test_mask_url() {
        assert_eq!(mask_url("http://127.0.0.1:7890"), "http://127.0.0.1:7890");
        assert_eq!(
            mask_url("http://user:pass@127.0.0.1:7890"),
            "http://127.0.0.1:7890"
        );
        assert_eq!(
            mask_url("socks5://admin:secret@proxy.example.com:1080"),
            "socks5://proxy.example.com:1080"
        );
        // 无端口的 URL 不应显示 ":?"
        assert_eq!(
            mask_url("http://proxy.example.com"),
            "http://proxy.example.com"
        );
        assert_eq!(
            mask_url("https://user:pass@proxy.example.com"),
            "https://proxy.example.com"
        );
    }

    #[test]
    fn test_mask_url_does_not_panic_on_multibyte_boundary() {
        // 一个无法被 Url::parse 解析、且在字节 20 处正好切在多字节字符中间的字符串。
        // 回归 https://github.com/farion1231/cc-switch 的 mask_url 越界 panic。
        let bad = "这是一个无效的代理地址不能解析";
        assert!(bad.len() > 20 && !bad.is_char_boundary(20));
        let masked = mask_url(bad);
        assert!(masked.ends_with("..."));
    }

    #[test]
    fn test_is_loopback_url() {
        for url in [
            "http://localhost:11434",
            "http://LOCALHOST",
            "http://localhost./v1",
            "http://127.0.0.1:1234/v1",
            "http://127.1.2.3",
            "http://[::1]:8000",
            "http://[::ffff:127.0.0.1]:8000",
            "http://foo.localhost",
            "https://Api.LocalHost:8443/v1",
        ] {
            assert!(is_loopback_url(url), "{url} should be loopback");
        }
        for url in [
            "http://192.168.1.10:11434",
            "http://10.0.0.2",
            "https://example.com",
            "https://localhost.evil.com",
            "https://evillocalhost",
            "http://[::2]",
            "localhost:11434",
            "",
        ] {
            assert!(!is_loopback_url(url), "{url} should not be loopback");
        }
    }

    #[test]
    fn test_explicit_proxy_skips_loopback() {
        let policy = ProxyPolicy::new(Some("http://127.0.0.1:7890")).unwrap();
        for url in [
            "http://localhost:11434/v1/messages",
            "http://127.0.0.1:1234/v1/chat/completions",
        ] {
            assert!(policy.proxy_for(url).is_none());
        }
        assert_eq!(
            policy
                .proxy_for("https://api.example.com/v1/messages")
                .unwrap()
                .uri(),
            &"http://127.0.0.1:7890/".parse::<http::Uri>().unwrap()
        );
    }

    /// 起一个只回 200 的一次性本机 HTTP 服务，返回它的地址
    fn spawn_ok_server() -> String {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming().take(4) {
                let Ok(mut stream) = stream else { continue };
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                );
            }
        });
        format!("http://{addr}/v1/models")
    }

    #[tokio::test]
    async fn test_loopback_target_bypasses_unreachable_proxy() {
        let url = spawn_ok_server();
        // Loopback bypass also applies to clients built with an explicit proxy.
        let proxied = build_client(Some("http://127.0.0.1:1")).unwrap();
        assert!(proxied
            .get(&url)
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .unwrap()
            .status()
            .is_success());
        // 本机地址选到的直连客户端不受代理影响
        assert!(is_loopback_url(&url));
        let resp = direct_client()
            .get(&url)
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .expect("direct client should reach the local server");
        assert_eq!(resp.status().as_u16(), 200);
    }

    #[test]
    fn test_build_client_direct() {
        let result = build_client(None);
        assert!(result.is_ok());
    }

    #[test]
    fn test_build_client_with_http_proxy() {
        let result = build_client(Some("http://127.0.0.1:7890"));
        assert!(result.is_ok());
    }

    #[test]
    fn test_build_client_with_socks5_proxy() {
        let result = build_client(Some("socks5://127.0.0.1:1080"));
        assert!(result.is_ok());
    }

    #[test]
    fn test_build_client_invalid_url() {
        // reqwest::Proxy::all 对某些无效 URL 不会立即报错
        // 使用明确无效的 scheme 来触发错误
        let result = build_client(Some("invalid-scheme://127.0.0.1:7890"));
        assert!(result.is_err(), "Should reject invalid proxy scheme");
    }

    #[test]
    #[serial_test::serial]
    fn test_proxy_points_to_loopback() {
        // 设置 CC Switch 代理端口为 15721（默认值）
        set_proxy_port(15721);

        // 只有指向 CC Switch 自己端口的 loopback 地址才返回 true
        assert!(proxy_points_to_loopback("http://127.0.0.1:15721"));
        assert!(proxy_points_to_loopback("socks5://localhost:15721"));
        assert!(proxy_points_to_loopback("127.0.0.1:15721"));

        // 其他 loopback 端口不应该被跳过（允许使用其他本地代理工具）
        assert!(!proxy_points_to_loopback("http://127.0.0.1:7890"));
        assert!(!proxy_points_to_loopback("socks5://localhost:1080"));

        // 非 loopback 地址不应该被跳过
        assert!(!proxy_points_to_loopback("http://192.168.1.10:7890"));
        assert!(!proxy_points_to_loopback("http://192.168.1.10:15721"));
    }

    #[test]
    #[serial_test::serial]
    fn test_system_proxy_points_to_loopback() {
        let _guard = env_lock().lock().unwrap();

        // 设置 CC Switch 代理端口
        set_proxy_port(15721);

        let keys = [
            "HTTP_PROXY",
            "http_proxy",
            "HTTPS_PROXY",
            "https_proxy",
            "ALL_PROXY",
            "all_proxy",
        ];

        for key in &keys {
            std::env::remove_var(key);
        }

        // 指向 CC Switch 端口的代理应该被跳过
        std::env::set_var("HTTP_PROXY", "http://127.0.0.1:15721");
        assert!(system_proxy_points_to_loopback());

        // 指向其他端口的本地代理不应该被跳过
        std::env::set_var("HTTP_PROXY", "http://127.0.0.1:7890");
        assert!(!system_proxy_points_to_loopback());

        // 非 loopback 地址不应该被跳过
        std::env::set_var("HTTP_PROXY", "http://10.0.0.2:7890");
        assert!(!system_proxy_points_to_loopback());

        for key in &keys {
            std::env::remove_var(key);
        }
    }
}

#[cfg(test)]
#[path = "http_client_routing_tests.rs"]
mod routing_tests;

#[cfg(test)]
#[path = "http_client_pool_tests.rs"]
mod pool_tests;
