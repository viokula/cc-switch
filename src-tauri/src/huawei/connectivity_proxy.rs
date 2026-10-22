use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio_rustls::TlsAcceptor;

use crate::error::AppError;
use crate::huawei::tls_cert::TlsCertManager;

const DEFAULT_PORT: u16 = 8888;
const ANTHROPIC_DOMAINS: &[&str] = &["api.anthropic.com", "platform.claude.com"];

pub struct ConnectivityProxy {
    port: u16,
    cert_manager: TlsCertManager,
}

impl Default for ConnectivityProxy {
    fn default() -> Self {
        Self::new()
    }
}

impl ConnectivityProxy {
    pub fn new() -> Self {
        Self {
            port: DEFAULT_PORT,
            cert_manager: TlsCertManager::new(),
        }
    }

    pub fn with_port(port: u16) -> Self {
        Self {
            port,
            cert_manager: TlsCertManager::new(),
        }
    }

    pub async fn start(&self) -> Result<ConnectivityProxyHandle, AppError> {
        let listener = TcpListener::bind(format!("127.0.0.1:{}", self.port))
            .await
            .map_err(|e| AppError::Config(format!("绑定端口 {} 失败: {e}", self.port)))?;

        let actual_port = listener
            .local_addr()
            .map_err(|e| AppError::Config(format!("获取监听地址失败: {e}")))?
            .port();

        log::info!("[ConnectivityProxy] 监听 127.0.0.1:{}", actual_port);

        let tls_config = self.cert_manager.server_config()?;
        let tls_acceptor = Arc::new(TlsAcceptor::from(Arc::new(tls_config)));

        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

        let join_handle = tokio::spawn(async move {
            let mut shutdown_rx = shutdown_rx;
            loop {
                tokio::select! {
                    accept_result = listener.accept() => {
                        match accept_result {
                            Ok((stream, addr)) => {
                                log::debug!("[ConnectivityProxy] 接受连接 from {addr}");
                                let acceptor = tls_acceptor.clone();
                                tokio::spawn(async move {
                                    if let Err(e) = handle_connection(stream, acceptor).await {
                                        log::warn!("[ConnectivityProxy] 连接处理错误: {e}");
                                    }
                                });
                            }
                            Err(e) => {
                                log::warn!("[ConnectivityProxy] 接受连接失败: {e}");
                            }
                        }
                    }
                    _ = &mut shutdown_rx => {
                        log::info!("[ConnectivityProxy] 收到停止信号，正在关闭...");
                        break;
                    }
                }
            }
        });

        Ok(ConnectivityProxyHandle {
            port: actual_port,
            join_handle,
            shutdown_tx,
        })
    }
}

pub struct ConnectivityProxyHandle {
    pub port: u16,
    join_handle: JoinHandle<()>,
    shutdown_tx: oneshot::Sender<()>,
}

impl ConnectivityProxyHandle {
    pub async fn stop(self) {
        let _ = self.shutdown_tx.send(());
        let _ = self.join_handle.await;
    }
}

async fn handle_connection(
    mut stream: TcpStream,
    tls_acceptor: Arc<TlsAcceptor>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut buf = vec![0u8; 4096];
    let n = stream.read(&mut buf).await?;
    if n == 0 {
        return Ok(());
    }

    let request = String::from_utf8_lossy(&buf[..n]);
    let first_line = request.lines().next().unwrap_or("");

    if let Some(rest) = first_line.strip_prefix("CONNECT ") {
        let target = rest.split_whitespace().next().unwrap_or("");
        let host = target.split(':').next().unwrap_or("");

        if ANTHROPIC_DOMAINS.contains(&host) {
            log::debug!("[ConnectivityProxy] MITM CONNECT {target}");
            stream
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await?;
            let tls_stream = tls_acceptor.accept(stream).await?;
            handle_tls_request(tls_stream).await?;
        } else {
            log::debug!("[ConnectivityProxy] 转发 CONNECT {target}");
            forward_connect(stream, target).await?;
        }
    } else {
        log::debug!("[ConnectivityProxy] 非 CONNECT 请求，转发到主代理");
        let mut proxy_stream =
            TcpStream::connect(("127.0.0.1", crate::proxy::http_client::get_proxy_port())).await?;
        proxy_stream.write_all(&buf[..n]).await?;

        // 双向转发：首包已写入 proxy_stream，但客户端可能还有后续字节
        // （POST body 或后续请求）需要继续送到上游。仅做一次 upstream->client
        // 的 copy 会导致 POST body 在首 4KB 之外被丢弃，请求挂住。
        let (mut client_read, mut client_write) = stream.split();
        let (mut proxy_read, mut proxy_write) = proxy_stream.split();

        let c2p = tokio::io::copy(&mut client_read, &mut proxy_write);
        let p2c = tokio::io::copy(&mut proxy_read, &mut client_write);

        tokio::select! {
            _ = c2p => {}
            _ = p2c => {}
        }
    }

    Ok(())
}

async fn handle_tls_request(
    mut tls_stream: tokio_rustls::server::TlsStream<TcpStream>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut buf = vec![0u8; 8192];
    let n = tls_stream.read(&mut buf).await?;
    if n == 0 {
        return Ok(());
    }

    let request = String::from_utf8_lossy(&buf[..n]);
    let first_line = request.lines().next().unwrap_or("");
    let parts: Vec<&str> = first_line.split_whitespace().collect();

    if parts.len() < 2 {
        return Ok(());
    }

    let method = parts[0];
    let path = parts[1];

    log::debug!("[ConnectivityProxy] TLS 请求: {method} {path}");

    match (method, path) {
        ("GET", "/api/hello") | ("GET", "/v1/oauth/hello") => {
            tls_stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\n\
                     Content-Type: application/json\r\n\
                     Content-Length: 11\r\n\
                     \r\n\
                     {\"ok\":true}",
                )
                .await?;
        }
        ("GET", "/api/claude_code/settings") | ("GET", "/api/claude_code/policy_limits") => {
            tls_stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\n\
                     Content-Type: application/json\r\n\
                     Content-Length: 2\r\n\
                     \r\n\
                     {}",
                )
                .await?;
        }
        _ => {
            log::debug!("[ConnectivityProxy] 转发到主代理: {method} {path}");
            forward_to_main_proxy(tls_stream, &buf[..n]).await?;
        }
    }

    Ok(())
}

async fn forward_to_main_proxy(
    tls_stream: tokio_rustls::server::TlsStream<TcpStream>,
    initial_data: &[u8],
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut proxy_stream =
        TcpStream::connect(("127.0.0.1", crate::proxy::http_client::get_proxy_port())).await?;
    proxy_stream.write_all(initial_data).await?;

    let (mut tls_read, mut tls_write) = tokio::io::split(tls_stream);
    let (mut proxy_read, mut proxy_write) = proxy_stream.split();

    let c2p = tokio::io::copy(&mut tls_read, &mut proxy_write);
    let p2c = tokio::io::copy(&mut proxy_read, &mut tls_write);

    tokio::select! {
        _ = c2p => {}
        _ = p2c => {}
    }

    Ok(())
}

async fn forward_connect(
    mut client_stream: TcpStream,
    target: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let upstream_proxy = std::env::var("HTTPS_PROXY")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("HTTP_PROXY").ok().filter(|s| !s.is_empty()));

    let used_upstream = upstream_proxy.is_some();
    let mut target_stream = if let Some(proxy_url) = upstream_proxy {
        let proxy_addr = parse_proxy_url(&proxy_url);
        let mut proxy = TcpStream::connect(&proxy_addr).await?;

        let connect_request = format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n\r\n");
        proxy.write_all(connect_request.as_bytes()).await?;

        let mut resp_buf = vec![0u8; 1024];
        let _ = proxy.read(&mut resp_buf).await?;
        let resp = String::from_utf8_lossy(&resp_buf);
        if !resp.starts_with("HTTP/1.1 200") && !resp.starts_with("HTTP/1.0 200") {
            return Err(format!(
                "上游代理拒绝 CONNECT {target}: {}",
                resp.lines().next().unwrap_or("")
            )
            .into());
        }
        proxy
    } else {
        let target_stream = TcpStream::connect(target).await?;
        client_stream
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await?;
        target_stream
    };

    if used_upstream {
        client_stream
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await?;
    }

    let (mut cr, mut cw) = client_stream.split();
    let (mut tr, mut tw) = target_stream.split();

    let c2t = tokio::io::copy(&mut cr, &mut tw);
    let t2c = tokio::io::copy(&mut tr, &mut cw);

    tokio::select! {
        _ = c2t => {}
        _ = t2c => {}
    }

    Ok(())
}

fn parse_proxy_url(url: &str) -> String {
    url.strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
        .unwrap_or(url)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::huawei::tls_cert::pem_to_der;
    use rustls::pki_types::CertificateDer;
    use rustls::RootCertStore;
    use serial_test::serial;
    use std::env;
    use std::sync::Once;
    use tempfile::TempDir;
    use tokio_rustls::TlsConnector;

    static CRYPTO_INIT: Once = Once::new();

    fn ensure_crypto_provider() {
        CRYPTO_INIT.call_once(|| {
            let _ = rustls::crypto::ring::default_provider().install_default();
        });
    }

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

    fn make_tls_connector() -> TlsConnector {
        ensure_crypto_provider();

        let cert_manager = TlsCertManager::new();
        let (cert_path, _) = cert_manager.ensure_cert().expect("ensure cert");
        let cert_pem = std::fs::read_to_string(cert_path).expect("read cert pem");
        let cert_der = pem_to_der(&cert_pem);

        let mut root_store = RootCertStore::empty();
        root_store
            .add(CertificateDer::from(cert_der))
            .expect("add cert");

        let config = rustls::ClientConfig::builder()
            .with_root_certificates(root_store)
            .with_no_client_auth();

        TlsConnector::from(Arc::new(config))
    }

    #[tokio::test]
    #[serial]
    async fn proxy_starts_and_listens() {
        let _home = TempHome::new();
        ensure_crypto_provider();

        let proxy = ConnectivityProxy::with_port(0);
        let handle = proxy.start().await.expect("start proxy");

        assert!(handle.port > 0, "should get a concrete port");

        handle.stop().await;
    }

    #[tokio::test]
    #[serial]
    async fn proxy_stop_releases_port() {
        let _home = TempHome::new();
        ensure_crypto_provider();

        let proxy = ConnectivityProxy::with_port(0);
        let handle = proxy.start().await.expect("start proxy");
        let port = handle.port;

        handle.stop().await;

        let bind_result = TcpListener::bind(format!("127.0.0.1:{}", port)).await;
        assert!(
            bind_result.is_ok(),
            "port {port} should be released after stop"
        );
    }

    #[tokio::test]
    #[serial]
    async fn connect_api_anthropic_then_get_hello_returns_200() {
        let _home = TempHome::new();
        ensure_crypto_provider();

        let proxy = ConnectivityProxy::with_port(0);
        let handle = proxy.start().await.expect("start proxy");

        let mut stream = TcpStream::connect(format!("127.0.0.1:{}", handle.port))
            .await
            .expect("connect to proxy");

        stream
            .write_all(
                b"CONNECT api.anthropic.com:443 HTTP/1.1\r\nHost: api.anthropic.com:443\r\n\r\n",
            )
            .await
            .expect("send CONNECT");

        let mut buf = vec![0u8; 1024];
        let n = stream.read(&mut buf).await.expect("read CONNECT response");
        let resp = String::from_utf8_lossy(&buf[..n]);
        assert!(
            resp.contains("200 Connection Established"),
            "CONNECT should return 200, got: {resp}"
        );

        let connector = make_tls_connector();
        let dns_name = rustls::pki_types::ServerName::try_from("api.anthropic.com".to_string())
            .expect("parse dns name");
        let mut tls_stream = connector
            .connect(dns_name, stream)
            .await
            .expect("TLS handshake");

        tls_stream
            .write_all(b"GET /api/hello HTTP/1.1\r\nHost: api.anthropic.com\r\n\r\n")
            .await
            .expect("send GET /api/hello");

        let mut resp_buf = vec![0u8; 1024];
        let n = tls_stream.read(&mut resp_buf).await.expect("read response");
        let resp = String::from_utf8_lossy(&resp_buf[..n]);
        assert!(resp.contains("200 OK"), "should return 200 OK, got: {resp}");
        assert!(
            resp.contains("{\"ok\":true}"),
            "should return ok:true, got: {resp}"
        );

        handle.stop().await;
    }

    #[tokio::test]
    #[serial]
    async fn connect_platform_claude_then_get_oauth_hello_returns_200() {
        let _home = TempHome::new();
        ensure_crypto_provider();

        let proxy = ConnectivityProxy::with_port(0);
        let handle = proxy.start().await.expect("start proxy");

        let mut stream = TcpStream::connect(format!("127.0.0.1:{}", handle.port))
            .await
            .expect("connect to proxy");

        stream
            .write_all(b"CONNECT platform.claude.com:443 HTTP/1.1\r\nHost: platform.claude.com:443\r\n\r\n")
            .await
            .expect("send CONNECT");

        let mut buf = vec![0u8; 1024];
        let n = stream.read(&mut buf).await.expect("read CONNECT response");
        let resp = String::from_utf8_lossy(&buf[..n]);
        assert!(
            resp.contains("200 Connection Established"),
            "CONNECT should return 200, got: {resp}"
        );

        let connector = make_tls_connector();
        let dns_name = rustls::pki_types::ServerName::try_from("platform.claude.com".to_string())
            .expect("parse dns name");
        let mut tls_stream = connector
            .connect(dns_name, stream)
            .await
            .expect("TLS handshake");

        tls_stream
            .write_all(b"GET /v1/oauth/hello HTTP/1.1\r\nHost: platform.claude.com\r\n\r\n")
            .await
            .expect("send GET /v1/oauth/hello");

        let mut resp_buf = vec![0u8; 1024];
        let n = tls_stream.read(&mut resp_buf).await.expect("read response");
        let resp = String::from_utf8_lossy(&resp_buf[..n]);
        assert!(resp.contains("200 OK"), "should return 200 OK, got: {resp}");
        assert!(
            resp.contains("{\"ok\":true}"),
            "should return ok:true, got: {resp}"
        );

        handle.stop().await;
    }
}
