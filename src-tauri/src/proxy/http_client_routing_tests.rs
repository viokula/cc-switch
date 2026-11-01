use super::*;
use crate::proxy::hyper_client::{send_request, OriginalHeaderCases, ProxyResponse};
use base64::Engine;
use http::{Extensions, HeaderMap, HeaderValue, Method, StatusCode, Uri};
use hyper_util::client::proxy::matcher::Matcher;
use std::io;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::task::{JoinHandle, JoinSet};

const TEST_TIMEOUT: Duration = Duration::from_secs(5);
const TARGET: &str = "http://upstream.invalid:12345/request?value=1";
const RESPONSE_BODY: &[u8] = b"via-local-proxy";

fn policy(configured_url: Option<&str>, matcher: Matcher) -> ProxyPolicy {
    ProxyPolicy {
        configured_url: configured_url.map(str::to_owned),
        matcher,
        automatic_disabled: false,
        windows_bypass: None,
    }
}

fn assert_proxy(policy: &ProxyPolicy, target: &str, expected: &str) {
    let selected = policy.proxy_for(target).expect(target);
    let mut expected = url::Url::parse(expected).unwrap();
    expected.set_path("/");
    assert_eq!(
        proxy_url_with_auth(&selected),
        expected,
        "route for {target}"
    );
}

#[test]
fn explicit_proxy_handles_both_schemes_and_ignores_windows_bypass() {
    let proxy = "http://explicit.invalid:18080";
    let policy = ProxyPolicy {
        windows_bypass: Some("*".into()),
        ..policy(Some(proxy), Matcher::builder().all(proxy).build())
    };
    for target in [TARGET, "https://upstream.invalid:12345/request"] {
        assert_proxy(&policy, target, proxy);
    }
}

#[test]
fn automatic_proxy_selects_http_and_https_independently() {
    let policy = policy(
        None,
        Matcher::builder()
            .http("http://http-proxy.invalid:18080")
            .https("http://https-proxy.invalid:18443")
            .build(),
    );
    assert_proxy(&policy, TARGET, "http://http-proxy.invalid:18080");
    assert_proxy(
        &policy,
        "https://upstream.invalid/request",
        "http://https-proxy.invalid:18443",
    );
    assert!(policy.proxy_for("ftp://upstream.invalid/file").is_none());
}

#[test]
fn automatic_all_proxy_is_fallback_for_each_missing_scheme() {
    for https_override in [false, true] {
        let builder = Matcher::builder().all("socks5h://all-proxy.invalid:11080");
        let matcher = if https_override {
            builder.https("http://https-proxy.invalid:18443").build()
        } else {
            builder.http("http://http-proxy.invalid:18080").build()
        };
        let policy = policy(None, matcher);
        assert_proxy(
            &policy,
            TARGET,
            if https_override {
                "socks5h://all-proxy.invalid:11080"
            } else {
                "http://http-proxy.invalid:18080"
            },
        );
        assert_proxy(
            &policy,
            "https://upstream.invalid/request",
            if https_override {
                "http://https-proxy.invalid:18443"
            } else {
                "socks5h://all-proxy.invalid:11080"
            },
        );
    }
}

#[test]
fn automatic_no_proxy_honors_domains_ips_and_cidr_without_overmatching() {
    let proxy = "http://proxy.invalid:18080";
    let policy = policy(
        None,
        Matcher::builder()
            .all(proxy)
            .no(".direct.invalid,192.0.2.8,198.51.100.0/24,2001:db8::1")
            .build(),
    );
    for target in [
        "http://direct.invalid/request",
        "https://child.direct.invalid/request",
        "http://192.0.2.8/request",
        "https://198.51.100.42/request",
        "http://[2001:db8::1]/request",
    ] {
        assert!(policy.proxy_for(target).is_none(), "{target}");
    }
    for target in [
        "http://notdirect.invalid/request",
        "http://direct.invalid.evil.invalid/request",
        "http://192.0.2.9/request",
        "http://198.51.101.42/request",
    ] {
        assert_proxy(&policy, target, proxy);
    }
}

#[test]
fn automatic_no_proxy_wildcard_disables_both_schemes() {
    let policy = policy(
        None,
        Matcher::builder()
            .all("http://proxy.invalid:18080")
            .no("*")
            .build(),
    );
    for target in [TARGET, "https://upstream.invalid/request"] {
        assert!(policy.proxy_for(target).is_none(), "{target}");
    }
}

#[test]
fn automatic_windows_bypass_honors_local_scheme_port_and_wildcards() {
    let proxy = "http://proxy.invalid:18080";
    let policy = ProxyPolicy {
        windows_bypass: Some("*.corp.invalid;<local>;https://secure.invalid:8443".into()),
        ..policy(None, Matcher::builder().all(proxy).build())
    };
    for target in [
        "http://API.CORP.INVALID./request",
        "https://child.corp.invalid/request",
        "http://intranet/request",
        "https://secure.invalid:8443/request",
    ] {
        assert!(policy.proxy_for(target).is_none(), "{target}");
    }
    for target in [
        TARGET,
        "http://corp.invalid.evil.invalid/request",
        "http://secure.invalid:8443/request",
        "https://secure.invalid:443/request",
    ] {
        assert_proxy(&policy, target, proxy);
    }
}

#[test]
fn loopback_is_direct_before_explicit_or_automatic_proxy_selection() {
    let proxy = "http://proxy.invalid:18080";
    for configured in [None, Some(proxy)] {
        let policy = policy(configured, Matcher::builder().all(proxy).build());
        for target in [
            "http://localhost:12345/request",
            "https://LOCALHOST./request",
            "http://service.localhost:12345/request",
            "http://127.0.0.1:12345/request",
            "http://127.23.45.67:12345/request",
            "http://[::1]:12345/request",
            "http://[::ffff:127.0.0.1]:12345/request",
        ] {
            assert!(policy.proxy_for(target).is_none(), "{target}");
        }
        assert_proxy(&policy, "http://localhost.evil.invalid/request", proxy);
    }
}

#[test]
fn automatic_recursion_guard_disables_an_otherwise_matching_proxy() {
    let policy = ProxyPolicy {
        automatic_disabled: true,
        ..policy(
            None,
            Matcher::builder()
                .http("http://http-proxy.invalid:18080")
                .https("http://https-proxy.invalid:18443")
                .build(),
        )
    };
    for target in [TARGET, "https://upstream.invalid/request"] {
        assert!(policy.proxy_for(target).is_none(), "{target}");
    }
}

#[test]
fn automatic_localhost_proxy_remains_selectable_without_recursion_snapshot() {
    let proxy = "http://127.0.0.1:17890";
    let automatic = policy(None, Matcher::builder().all(proxy).build());
    assert_proxy(&automatic, TARGET, proxy);
}

#[test]
#[serial_test::serial]
fn selected_proxy_pointing_at_the_live_listener_port_goes_direct() {
    // init() builds the policy before ProxyServer::start() publishes the real
    // port, and set_proxy_port must not need to rebuild it: the guard reads the
    // live port on every route decision.
    set_proxy_port(15722);
    let self_listener = "http://127.0.0.1:15722";
    for configured in [None, Some(self_listener)] {
        let policy = policy(configured, Matcher::builder().all(self_listener).build());
        assert!(
            policy.proxy_for(TARGET).is_none(),
            "configured={configured:?}"
        );
    }
    let other_local = "http://127.0.0.1:17890";
    assert_proxy(
        &policy(None, Matcher::builder().all(other_local).build()),
        TARGET,
        other_local,
    );
    set_proxy_port(15721);
}

#[test]
fn http_basic_auth_with_escaped_characters_round_trips_through_proxy_url() {
    let expected = format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode("u@ser/% +:p:a@ss/% +")
    );
    for scheme in ["http", "https"] {
        let proxy =
            format!("{scheme}://u%40ser%2F%25%20%2B:p%3Aa%40ss%2F%25%20%2B@proxy.invalid:18080");
        let policy = policy(Some(&proxy), Matcher::builder().all(proxy.as_str()).build());
        let selected = policy.proxy_for(TARGET).unwrap();
        assert_eq!(selected.basic_auth().unwrap().to_str().unwrap(), expected);
        let restored = proxy_url_with_auth(&selected);
        let round_trip = Matcher::builder()
            .all(restored.as_str())
            .build()
            .intercept(&TARGET.parse::<Uri>().unwrap())
            .unwrap();
        assert_eq!(round_trip.uri(), selected.uri());
        assert_eq!(round_trip.basic_auth(), selected.basic_auth());
        assert!(round_trip.raw_auth().is_none());
    }
}

#[test]
fn socks_raw_auth_with_escaped_characters_round_trips_through_proxy_url() {
    for scheme in ["socks5", "socks5h"] {
        let proxy =
            format!("{scheme}://u%3A%40ser%2F%25%20%2B:p%3Aa%40ss%2F%25%20%2B@proxy.invalid:11080");
        let policy = policy(Some(&proxy), Matcher::builder().all(proxy.as_str()).build());
        let selected = policy.proxy_for(TARGET).unwrap();
        assert_eq!(selected.raw_auth(), Some(("u:@ser/% +", "p:a@ss/% +")));
        let restored = proxy_url_with_auth(&selected);
        let round_trip = Matcher::builder()
            .all(restored.as_str())
            .build()
            .intercept(&TARGET.parse::<Uri>().unwrap())
            .unwrap();
        assert_eq!(round_trip.uri(), selected.uri());
        assert_eq!(round_trip.raw_auth(), selected.raw_auth());
        assert!(round_trip.basic_auth().is_none());
    }
}

#[test]
fn proxy_auth_preserves_literal_percent_escapes_and_unicode() {
    for scheme in ["http", "https", "socks5", "socks5h"] {
        let proxy = format!("{scheme}://u%252F%E4%B8%AD:p%2540%E6%96%87@proxy.invalid:18080");
        let selected = Matcher::builder()
            .all(proxy.as_str())
            .build()
            .intercept(&TARGET.parse::<Uri>().unwrap())
            .unwrap();
        let restored = proxy_url_with_auth(&selected);
        let round_trip = Matcher::builder()
            .all(restored.as_str())
            .build()
            .intercept(&TARGET.parse::<Uri>().unwrap())
            .unwrap();
        assert_eq!(round_trip.basic_auth(), selected.basic_auth());
        assert_eq!(round_trip.raw_auth(), selected.raw_auth());
    }
}

#[derive(Clone)]
enum ProxyReply {
    Success,
    Streamed(Arc<tokio::sync::Notify>),
    Reject(u16),
    Disconnect,
    DisconnectFirst(Arc<AtomicUsize>),
}

struct LocalProxy {
    address: std::net::SocketAddr,
    connections: Arc<AtomicUsize>,
    requests: mpsc::UnboundedReceiver<Vec<u8>>,
    task: JoinHandle<()>,
}

impl Drop for LocalProxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl LocalProxy {
    fn url(&self) -> String {
        format!("http://{}", self.address)
    }

    fn authenticated_url(&self) -> String {
        format!(
            "http://u%40ser%2F%25%20%2B:p%3Aa%40ss%2F%25%20%2B@{}",
            self.address
        )
    }

    async fn next_request(&mut self) -> Vec<u8> {
        tokio::time::timeout(TEST_TIMEOUT, self.requests.recv())
            .await
            .expect("local proxy request timeout")
            .expect("local proxy request channel closed")
    }
}

async fn local_proxy(reply: ProxyReply) -> LocalProxy {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let connections = Arc::new(AtomicUsize::new(0));
    let count = connections.clone();
    let (requests_tx, requests) = mpsc::unbounded_channel();
    let task = tokio::spawn(async move {
        // Dropping this set when the parent is aborted cancels every socket task.
        let mut sockets = JoinSet::new();
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let Ok((stream, _)) = accepted else { break };
                    count.fetch_add(1, Ordering::SeqCst);
                    let requests_tx = requests_tx.clone();
                    let reply = reply.clone();
                    sockets.spawn(async move {
                        let _ = tokio::time::timeout(
                            TEST_TIMEOUT,
                            serve_proxy_connection(stream, reply, requests_tx),
                        ).await;
                    });
                }
                _ = sockets.join_next(), if !sockets.is_empty() => {}
            }
        }
    });
    LocalProxy {
        address,
        connections,
        requests,
        task,
    }
}

async fn serve_proxy_connection(
    mut stream: TcpStream,
    reply: ProxyReply,
    requests: mpsc::UnboundedSender<Vec<u8>>,
) -> io::Result<()> {
    let first = read_http_message(&mut stream).await?;
    let is_connect = first.starts_with(b"CONNECT ");
    let _ = requests.send(first);
    match reply {
        ProxyReply::Disconnect => return Ok(()),
        ProxyReply::DisconnectFirst(ref count) if count.fetch_add(1, Ordering::SeqCst) == 0 => {
            return Ok(())
        }
        ProxyReply::Reject(status) => {
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 {status} Proxy rejected\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    )
                    .as_bytes(),
                )
                .await?;
            return Ok(());
        }
        ProxyReply::Success | ProxyReply::Streamed(_) | ProxyReply::DisconnectFirst(_) => {}
    }
    if is_connect {
        stream
            .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
            .await?;
        // The socket becomes a tiny origin after CONNECT; no DNS or relay is needed.
        let origin = read_http_message(&mut stream).await?;
        let _ = requests.send(origin);
    }
    if let ProxyReply::Streamed(release) = reply {
        stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\nd\r\ndata: first\n\n\r\n").await?;
        release.notified().await;
        stream
            .write_all(b"e\r\ndata: second\n\n\r\n0\r\n\r\n")
            .await?;
        return stream.shutdown().await;
    }
    stream
        .write_all(
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                RESPONSE_BODY.len()
            )
            .as_bytes(),
        )
        .await?;
    stream.write_all(RESPONSE_BODY).await?;
    stream.shutdown().await
}

async fn read_http_message(stream: &mut TcpStream) -> io::Result<Vec<u8>> {
    let mut message = Vec::new();
    while !message.ends_with(b"\r\n\r\n") {
        if message.len() >= 64 * 1024 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "oversized headers",
            ));
        }
        message.push(stream.read_u8().await?);
    }
    let content_length = header_values(&message, "content-length")
        .first()
        .map(|value| value.parse::<usize>().unwrap())
        .unwrap_or(0);
    if content_length > 64 * 1024 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "oversized body"));
    }
    let header_len = message.len();
    message.resize(header_len + content_length, 0);
    stream.read_exact(&mut message[header_len..]).await?;
    Ok(message)
}

fn header_values(message: &[u8], name: &str) -> Vec<String> {
    let end = message
        .windows(4)
        .position(|part| part == b"\r\n\r\n")
        .unwrap();
    std::str::from_utf8(&message[..end])
        .unwrap()
        .split("\r\n")
        .skip(1)
        .filter_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case(name)
                .then(|| value.trim().to_owned())
        })
        .collect()
}

fn request_extensions(with_cases: bool) -> Extensions {
    let mut extensions = Extensions::new();
    if with_cases {
        extensions.insert(OriginalHeaderCases::from_raw_bytes(
            b"POST /request?value=1 HTTP/1.1\r\nHost: upstream.invalid:12345\r\nX-RePeAt: first\r\nx-rEPEAT: second\r\nAuThOrIzAtIoN: Bearer origin-only\r\n\r\n",
        ));
    }
    extensions
}

async fn assert_response(response: ProxyResponse) {
    assert_eq!(response.status(), StatusCode::OK);
    let body = tokio::time::timeout(TEST_TIMEOUT, response.bytes_with_limit(1024))
        .await
        .expect("response body timeout")
        .unwrap();
    assert_eq!(body.as_ref(), RESPONSE_BODY);
}

async fn connect_success(with_cases: bool) {
    let mut proxy = local_proxy(ProxyReply::Success).await;
    let proxy_url = proxy.authenticated_url();
    let policy = policy(
        Some(&proxy_url),
        Matcher::builder().all(proxy_url.as_str()).build(),
    );
    let intercept = policy.proxy_for(TARGET).unwrap();
    let mut headers = HeaderMap::new();
    headers.insert("host", HeaderValue::from_static("upstream.invalid:12345"));
    headers.append("x-repeat", HeaderValue::from_static("first"));
    headers.append("x-repeat", HeaderValue::from_static("second"));
    headers.insert(
        "authorization",
        HeaderValue::from_static("Bearer origin-only"),
    );
    let body = b"binary\0body\r\n\xff".to_vec();
    let response = tokio::time::timeout(
        TEST_TIMEOUT,
        send_request(
            TARGET.parse().unwrap(),
            "routing-test",
            Method::POST,
            headers,
            request_extensions(with_cases),
            body.clone(),
            TEST_TIMEOUT,
            Some(&intercept),
        ),
    )
    .await
    .expect("CONNECT request timeout")
    .unwrap();
    assert_response(response).await;
    let connect = proxy.next_request().await;
    assert!(connect.starts_with(b"CONNECT upstream.invalid:12345 HTTP/1.1\r\n"));
    assert_eq!(
        header_values(&connect, "proxy-authorization"),
        vec![intercept.basic_auth().unwrap().to_str().unwrap().to_owned()]
    );
    assert!(header_values(&connect, "authorization").is_empty());
    let origin = proxy.next_request().await;
    assert!(origin.starts_with(b"POST /request?value=1 HTTP/1.1\r\n"));
    assert_eq!(header_values(&origin, "x-repeat"), vec!["first", "second"]);
    assert_eq!(
        header_values(&origin, "authorization"),
        vec!["Bearer origin-only"]
    );
    assert!(header_values(&origin, "proxy-authorization").is_empty());
    let body_start = origin
        .windows(4)
        .position(|part| part == b"\r\n\r\n")
        .unwrap()
        + 4;
    assert_eq!(&origin[body_start..], body);
    if with_cases {
        let original =
            b"X-RePeAt: first\r\nx-rEPEAT: second\r\nAuThOrIzAtIoN: Bearer origin-only\r\n";
        assert!(origin.windows(original.len()).any(|part| part == original));
    }
    assert_eq!(proxy.connections.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn connect_preserves_body_repeated_headers_and_original_cases() {
    connect_success(true).await;
}

#[tokio::test]
async fn proxy_without_original_cases_still_uses_connect_and_keeps_auth_private() {
    connect_success(false).await;
}

async fn rejected_connect_never_goes_direct(reply: ProxyReply, with_cases: bool) {
    let mut direct = local_proxy(ProxyReply::Success).await;
    let mut proxy = local_proxy(reply).await;
    let intercept = Matcher::builder()
        .all(proxy.url())
        .build()
        .intercept(&TARGET.parse::<Uri>().unwrap())
        .unwrap();
    let target = format!("{}/must-not-connect", direct.url());
    // Bypass policy deliberately: a reachable origin detects transport fallback.
    let result = tokio::time::timeout(
        TEST_TIMEOUT,
        send_request(
            target.parse().unwrap(),
            "routing-test-no-fallback",
            Method::GET,
            HeaderMap::new(),
            request_extensions(with_cases),
            Vec::new(),
            TEST_TIMEOUT,
            Some(&intercept),
        ),
    )
    .await
    .expect("rejected CONNECT request timeout");
    assert!(
        result.is_err(),
        "proxy failure must not become direct success"
    );
    let connect = proxy.next_request().await;
    assert!(connect.starts_with(format!("CONNECT {} HTTP/1.1\r\n", direct.address).as_bytes()));
    assert!(
        tokio::time::timeout(Duration::from_millis(100), direct.requests.recv())
            .await
            .is_err(),
        "origin received a request after proxy failure"
    );
    assert_eq!(direct.connections.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn connect_407_with_original_cases_never_falls_back_direct() {
    rejected_connect_never_goes_direct(ProxyReply::Reject(407), true).await;
}

#[tokio::test]
async fn connect_407_without_original_cases_never_falls_back_direct() {
    rejected_connect_never_goes_direct(ProxyReply::Reject(407), false).await;
}

#[tokio::test]
async fn connect_503_with_original_cases_never_falls_back_direct() {
    rejected_connect_never_goes_direct(ProxyReply::Reject(503), true).await;
}

#[tokio::test]
async fn connect_503_without_original_cases_never_falls_back_direct() {
    rejected_connect_never_goes_direct(ProxyReply::Reject(503), false).await;
}

#[tokio::test]
async fn disconnected_proxy_with_original_cases_never_falls_back_direct() {
    rejected_connect_never_goes_direct(ProxyReply::Disconnect, true).await;
}

#[tokio::test]
async fn disconnected_proxy_without_original_cases_never_falls_back_direct() {
    rejected_connect_never_goes_direct(ProxyReply::Disconnect, false).await;
}

async fn reqwest_and_raw_share_proxy(explicit: bool) {
    let mut proxy = local_proxy(ProxyReply::Success).await;
    let unselected = local_proxy(ProxyReply::Success).await;
    let proxy_url = proxy.authenticated_url();
    let policy = Arc::new(policy(
        explicit.then_some(proxy_url.as_str()),
        Matcher::builder()
            .all(unselected.url())
            .http(proxy_url.as_str())
            .build(),
    ));
    let intercept = policy.proxy_for(TARGET).unwrap();
    assert_eq!(
        intercept.uri().authority().unwrap().as_str(),
        proxy.address.to_string()
    );
    let client = policy
        .build_client(Client::builder().timeout(TEST_TIMEOUT))
        .unwrap();
    let response = client.get(TARGET).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.bytes().await.unwrap().as_ref(), RESPONSE_BODY);
    let absolute = proxy.next_request().await;
    assert!(absolute.starts_with(format!("GET {TARGET} HTTP/1.1\r\n").as_bytes()));
    assert_eq!(
        header_values(&absolute, "proxy-authorization"),
        vec![intercept.basic_auth().unwrap().to_str().unwrap().to_owned()]
    );
    let response = send_request(
        TARGET.parse().unwrap(),
        "routing-test-shared-policy",
        Method::GET,
        HeaderMap::new(),
        Extensions::new(),
        Vec::new(),
        TEST_TIMEOUT,
        Some(&intercept),
    )
    .await
    .unwrap();
    assert_response(response).await;
    assert!(proxy
        .next_request()
        .await
        .starts_with(b"CONNECT upstream.invalid:12345 HTTP/1.1\r\n"));
    let origin = proxy.next_request().await;
    assert!(origin.starts_with(b"GET /request?value=1 HTTP/1.1\r\n"));
    assert!(header_values(&origin, "proxy-authorization").is_empty());
    assert_eq!(proxy.connections.load(Ordering::SeqCst), 2);
    assert_eq!(unselected.connections.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn explicit_policy_routes_reqwest_and_raw_to_the_same_authenticated_proxy() {
    reqwest_and_raw_share_proxy(true).await;
}

#[tokio::test]
async fn automatic_policy_routes_reqwest_and_raw_to_the_same_authenticated_http_proxy() {
    reqwest_and_raw_share_proxy(false).await;
}

async fn reqwest_direct_route(kind: &str) {
    let mut direct = local_proxy(ProxyReply::Success).await;
    let proxy = local_proxy(ProxyReply::Success).await;
    let proxy_url = proxy.url();
    let target = if kind == "loopback" {
        format!("{}/direct", direct.url())
    } else {
        format!("http://direct.invalid:{}/direct", direct.address.port())
    };
    let matcher = Matcher::builder().all(proxy_url.as_str());
    let policy = Arc::new(ProxyPolicy {
        windows_bypass: (kind == "windows").then(|| "direct.invalid".into()),
        automatic_disabled: kind == "disabled",
        ..policy(
            (kind == "loopback").then_some(proxy_url.as_str()),
            if kind == "no_proxy" {
                matcher.no("direct.invalid").build()
            } else {
                matcher.build()
            },
        )
    });
    assert!(policy.proxy_for(&target).is_none());
    let client = policy
        .build_client(
            Client::builder()
                .timeout(TEST_TIMEOUT)
                .resolve("direct.invalid", direct.address),
        )
        .unwrap();
    let response = client.get(&target).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.bytes().await.unwrap().as_ref(), RESPONSE_BODY);
    assert!(direct
        .next_request()
        .await
        .starts_with(b"GET /direct HTTP/1.1\r\n"));
    assert_eq!(direct.connections.load(Ordering::SeqCst), 1);
    assert_eq!(proxy.connections.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn connect_streams_first_event_before_origin_finishes() {
    use futures::StreamExt;
    let release = Arc::new(tokio::sync::Notify::new());
    let proxy = local_proxy(ProxyReply::Streamed(release.clone())).await;
    let selected = Matcher::builder()
        .all(proxy.url())
        .build()
        .intercept(&TARGET.parse::<Uri>().unwrap())
        .unwrap();
    let response = send_request(
        TARGET.parse().unwrap(),
        "routing-test-stream",
        Method::GET,
        HeaderMap::new(),
        Extensions::new(),
        Vec::new(),
        TEST_TIMEOUT,
        Some(&selected),
    )
    .await
    .unwrap();
    assert_eq!(response.headers()["content-type"], "text/event-stream");
    let stream = response.bytes_stream();
    futures::pin_mut!(stream);
    let first = tokio::time::timeout(TEST_TIMEOUT, stream.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(first.as_ref(), b"data: first\n\n");
    release.notify_one();
    let tail = tokio::time::timeout(TEST_TIMEOUT, async {
        let mut tail = Vec::new();
        while let Some(chunk) = stream.next().await {
            tail.extend_from_slice(&chunk.unwrap());
        }
        tail
    })
    .await
    .unwrap();
    assert_eq!(tail, b"data: second\n\n");
}

#[tokio::test]
async fn unavailable_proxy_never_falls_back_to_reachable_origin() {
    let direct = local_proxy(ProxyReply::Success).await;
    let unavailable = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_url = format!("http://{}", unavailable.local_addr().unwrap());
    drop(unavailable);
    let selected = Matcher::builder()
        .all(proxy_url)
        .build()
        .intercept(&TARGET.parse::<Uri>().unwrap())
        .unwrap();
    for with_cases in [true, false] {
        let result = send_request(
            direct.url().parse().unwrap(),
            "routing-test-connect-failure",
            Method::GET,
            HeaderMap::new(),
            request_extensions(with_cases),
            Vec::new(),
            TEST_TIMEOUT,
            Some(&selected),
        )
        .await;
        assert!(result.is_err());
    }
    assert_eq!(direct.connections.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn direct_raw_failure_keeps_existing_hyper_fallback() {
    let origin = local_proxy(ProxyReply::DisconnectFirst(Arc::new(AtomicUsize::new(0)))).await;
    let response = send_request(
        origin.url().parse().unwrap(),
        "routing-test-direct-fallback",
        Method::GET,
        HeaderMap::new(),
        request_extensions(true),
        Vec::new(),
        TEST_TIMEOUT,
        None,
    )
    .await
    .unwrap();
    assert_response(response).await;
    assert_eq!(origin.connections.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn direct_raw_and_hyper_paths_reach_local_origin() {
    for with_cases in [true, false] {
        let mut origin = local_proxy(ProxyReply::Success).await;
        let response = send_request(
            origin.url().parse().unwrap(),
            "routing-test-direct",
            Method::GET,
            HeaderMap::new(),
            request_extensions(with_cases),
            Vec::new(),
            TEST_TIMEOUT,
            None,
        )
        .await
        .unwrap();
        assert_response(response).await;
        assert!(origin
            .next_request()
            .await
            .starts_with(b"GET / HTTP/1.1\r\n"));
        assert_eq!(origin.connections.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn reqwest_bypasses_explicit_proxy_for_loopback() {
    reqwest_direct_route("loopback").await;
}

#[tokio::test]
async fn reqwest_uses_shared_no_proxy_rule() {
    reqwest_direct_route("no_proxy").await;
}

#[tokio::test]
async fn reqwest_uses_shared_windows_bypass_rule() {
    reqwest_direct_route("windows").await;
}

#[tokio::test]
async fn reqwest_respects_disabled_automatic_proxy() {
    reqwest_direct_route("disabled").await;
}
