use super::*;
use bytes::Bytes;
use http_body_util::Full;
use hyper::{service::service_fn, Request, Response};
use hyper_util::rt::TokioIo;
use std::convert::Infallible;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use tokio::net::TcpListener;

async fn echo_auth(
    request: Request<hyper::body::Incoming>,
) -> Result<Response<Full<Bytes>>, Infallible> {
    let auth = request
        .headers()
        .get("authorization")
        .map(|value| value.as_bytes().to_vec())
        .unwrap_or_else(|| b"none".to_vec());
    Ok(Response::new(Full::new(Bytes::from(auth))))
}

struct Server {
    url: String,
    connections: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn server(tls: bool) -> Server {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let acceptor = if tls {
        let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let key = rustls::pki_types::PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der());
        let config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![cert.cert.der().clone()], key.into())
            .unwrap();
        Some(tokio_rustls::TlsAcceptor::from(Arc::new(config)))
    } else {
        None
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let connections = Arc::new(AtomicUsize::new(0));
    let count = connections.clone();
    let task = tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            count.fetch_add(1, Ordering::SeqCst);
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                if let Some(acceptor) = acceptor {
                    if let Ok(stream) = acceptor.accept(stream).await {
                        let _ = hyper::server::conn::http1::Builder::new()
                            .serve_connection(TokioIo::new(stream), service_fn(echo_auth))
                            .await;
                    }
                } else {
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(TokioIo::new(stream), service_fn(echo_auth))
                        .await;
                }
            });
        }
    });
    Server {
        url: format!("{}://127.0.0.1:{port}", if tls { "https" } else { "http" }),
        connections,
        task,
    }
}

async fn send(client: Client, url: &str, auth: Option<&str>) -> String {
    let mut request = client.get(url).timeout(Duration::from_secs(5));
    if let Some(auth) = auth {
        request = request.header("authorization", auth);
    }
    request.send().await.unwrap().text().await.unwrap()
}

#[tokio::test]
async fn tls_override_reuses_pool_without_leaking_auth_or_trust() {
    let server = server(true).await;
    for auth in [Some("Bearer account-a"), Some("Bearer account-b"), None] {
        let client = get_for_url_with_tls(&server.url, true).unwrap();
        assert_eq!(
            send(client, &server.url, auth).await,
            auth.unwrap_or("none")
        );
    }
    assert_eq!(
        server.connections.load(Ordering::SeqCst),
        1,
        "reuse TLS connection"
    );
    let verified = get_for_url_with_tls(&server.url, false).unwrap();
    assert!(
        verified
            .get(&server.url)
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .is_err(),
        "verified client must reject the same self-signed endpoint"
    );
    assert_eq!(
        send(
            get_for_url_with_tls(&server.url, true).unwrap(),
            &server.url,
            None
        )
        .await,
        "none"
    );
}

#[tokio::test]
async fn automatic_policy_refresh_replaces_tls_override_proxy_pool() {
    let first = server(false).await;
    let second = server(false).await;
    let policies = [&first, &second].map(|server| {
        Arc::new(ProxyPolicy {
            configured_url: None,
            matcher: Matcher::builder().all(server.url.as_str()).build(),
            automatic_disabled: false,
            windows_bypass: None,
        })
    });
    let mut clients = TlsOverrideClients::default();
    for _ in 0..2 {
        send(
            clients.get(false, &policies[0]).unwrap(),
            "http://upstream.invalid/request",
            None,
        )
        .await;
    }
    send(
        clients.get(false, &policies[1]).unwrap(),
        "http://upstream.invalid/request",
        None,
    )
    .await;
    assert_eq!(first.connections.load(Ordering::SeqCst), 1);
    assert_eq!(second.connections.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn changing_proxy_replaces_pool_and_keeps_direct_routes_separate() {
    let first = server(false).await;
    let second = server(false).await;
    let direct = server(false).await;
    let mut clients = TlsOverrideClients::default();
    let first_policy = Arc::new(ProxyPolicy::new(Some(&first.url)).unwrap());
    let second_policy = Arc::new(ProxyPolicy::new(Some(&second.url)).unwrap());
    for _ in 0..2 {
        let client = clients.get(false, &first_policy).unwrap();
        assert_eq!(
            send(client, "http://upstream.invalid/request", None).await,
            "none"
        );
    }
    assert_eq!(first.connections.load(Ordering::SeqCst), 1);
    let client = clients.get(false, &second_policy).unwrap();
    send(client, "http://upstream.invalid/request", None).await;
    assert_eq!(second.connections.load(Ordering::SeqCst), 1);
    for policy in [&first_policy, &second_policy] {
        send(clients.get(true, policy).unwrap(), &direct.url, None).await;
    }
    assert_eq!(direct.connections.load(Ordering::SeqCst), 1);
    assert_eq!(first.connections.load(Ordering::SeqCst), 1);
    assert_eq!(second.connections.load(Ordering::SeqCst), 1);
    // An invalid update must not poison the last usable pool.
    assert!(ProxyPolicy::new(Some("ftp://invalid.test")).is_err());
    send(
        clients.get(false, &second_policy).unwrap(),
        "http://upstream.invalid/request",
        None,
    )
    .await;
    assert_eq!(second.connections.load(Ordering::SeqCst), 1);
}
