//! HTTP-layer hardening for the embedded server: the serve loop (header-read deadline and a
//! connection cap), a request-body read deadline, and rate-limit buckets an unauthenticated
//! caller cannot drain for an authenticated one.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use tokio::sync::{Semaphore, watch};

use crate::auth::RateLimiterState;

/// Largest request body any route accepts (`/mcp` included — rmcp reads that body itself, so
/// axum's `DefaultBodyLimit` never applied to it).
pub const MAX_REQUEST_BODY_BYTES: usize = 2 * 1024 * 1024;

/// How long a client gets to deliver a request body once its headers have arrived.
pub const BODY_READ_TIMEOUT: Duration = Duration::from_secs(30);

/// Connection-level limits for [`serve_hardened`].
#[derive(Clone, Copy, Debug)]
pub struct ServeLimits {
    /// Connections served at once; further clients wait in the listen backlog.
    pub max_connections: usize,
    /// A connection that has not delivered a complete request head within this is closed.
    pub header_read_timeout: Duration,
    /// Accepted connections that have not yet sent a byte (or wait for a request slot).
    pub max_pending: usize,
    /// A connection that sends nothing within this is closed.
    pub first_byte_timeout: Duration,
}

impl ServeLimits {
    pub const DEFAULT: Self = Self {
        max_connections: 256,
        header_read_timeout: Duration::from_secs(30),
        max_pending: 1024,
        first_byte_timeout: Duration::from_secs(3),
    };
}

/// Pause after a failed `accept` (e.g. out of file descriptors) so the loop cannot spin.
const ACCEPT_ERROR_BACKOFF: Duration = Duration::from_millis(50);

/// Serve `app` on `listener` until `shutdown` completes, then drain open connections.
///
/// Replaces `axum::serve`, which builds hyper with no timer (so no header-read timeout: a
/// client trickling header bytes held its connection forever) and accepts connections without
/// limit (slow-loris clients exhausted file descriptors). A connection holds a request slot only
/// once it has sent a byte; before that it holds a slot of the larger pending pool, for at most
/// `first_byte_timeout`.
pub async fn serve_hardened<F>(
    listener: tokio::net::TcpListener,
    app: axum::Router,
    limits: ServeLimits,
    shutdown: F,
) where
    F: Future<Output = ()> + Send,
{
    use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};

    // Two pools. A connection is accepted into `pending` (bounding file descriptors) and only
    // takes one of the `slots` once it has sent its first byte. A connection that never sends
    // anything (a browser's speculative `<link rel=preconnect>`, which no request guard ever
    // sees) is closed at the short first-byte deadline and never holds a request slot; it used to
    // hold one for the whole header deadline, and a page could park every slot that way.
    let slots = Arc::new(Semaphore::new(limits.max_connections));
    let pending = Arc::new(Semaphore::new(limits.max_pending));
    let (signal_tx, _) = watch::channel(false);
    let (close_tx, close_rx) = watch::channel(());
    tokio::pin!(shutdown);

    loop {
        let pending_permit = tokio::select! {
            permit = Arc::clone(&pending).acquire_owned() => match permit {
                Ok(permit) => permit,
                Err(_closed) => break,
            },
            () = &mut shutdown => break,
        };
        let stream = tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => stream,
                Err(e) => {
                    tracing::debug!("Victauri: accept failed: {e}");
                    tokio::time::sleep(ACCEPT_ERROR_BACKOFF).await;
                    continue;
                }
            },
            () = &mut shutdown => break,
        };

        let service = hyper_util::service::TowerToHyperService::new(app.clone());
        let mut signal_rx = signal_tx.subscribe();
        let close_rx = close_rx.clone();
        let slots = Arc::clone(&slots);
        tokio::spawn(async move {
            // Until the first byte, a slot, or shutdown — whichever comes first.
            let admitted = tokio::select! {
                permit = admit(&stream, &slots, limits.first_byte_timeout) => permit,
                _ = signal_rx.changed() => None,
            };
            drop(pending_permit);
            let Some(_permit) = admitted else {
                drop(close_rx);
                return;
            };
            // HTTP/1 only, and no protocol sniffing: the auto builder's HTTP/2-preface
            // detection runs before hyper's header timer starts, so a client that connects and
            // sends nothing sat there untimed.
            let mut builder =
                hyper_util::server::conn::auto::Builder::new(TokioExecutor::new()).http1_only();
            builder
                .http1()
                .timer(TokioTimer::new())
                .header_read_timeout(limits.header_read_timeout);
            let conn = builder.serve_connection(TokioIo::new(stream), service);
            tokio::pin!(conn);
            let mut draining = false;
            loop {
                tokio::select! {
                    result = conn.as_mut() => {
                        if let Err(e) = result {
                            tracing::trace!("Victauri: connection ended: {e}");
                        }
                        break;
                    }
                    changed = signal_rx.changed(), if !draining => {
                        draining = true;
                        if changed.is_ok() {
                            conn.as_mut().graceful_shutdown();
                        }
                    }
                }
            }
            drop(close_rx);
        });
    }

    drop(listener);
    signal_tx.send_replace(true);
    drop(close_rx);
    close_tx.closed().await;
}

/// Wait for `stream`'s first byte (at most `first_byte_timeout`), then for a request slot.
/// `None` closes the connection: it sent nothing in time, hung up, or the pool is closed.
async fn admit(
    stream: &tokio::net::TcpStream,
    slots: &Arc<Semaphore>,
    first_byte_timeout: Duration,
) -> Option<tokio::sync::OwnedSemaphorePermit> {
    let mut probe = [0u8; 1];
    match tokio::time::timeout(first_byte_timeout, stream.peek(&mut probe)).await {
        Ok(Ok(n)) if n > 0 => Arc::clone(slots).acquire_owned().await.ok(),
        _ => None,
    }
}

/// Read the whole request body under [`BODY_READ_TIMEOUT`] and [`MAX_REQUEST_BODY_BYTES`]
/// before any handler (or the `/mcp` transport) sees it.
///
/// A client that sends headers and then trickles its body used to hold a connection — and one
/// of the server's 64 request slots — for as long as it liked. Every route takes a small JSON
/// body, so buffering it up front costs nothing.
pub async fn read_body_with_deadline(request: Request, next: Next) -> Response {
    buffer_body(request, next, BODY_READ_TIMEOUT).await
}

async fn buffer_body(request: Request, next: Next, deadline: Duration) -> Response {
    let (parts, body) = request.into_parts();
    match tokio::time::timeout(deadline, axum::body::to_bytes(body, MAX_REQUEST_BODY_BYTES)).await {
        Ok(Ok(bytes)) => {
            next.run(Request::from_parts(parts, axum::body::Body::from(bytes)))
                .await
        }
        Ok(Err(e)) if e.to_string().contains("length limit") => (
            StatusCode::PAYLOAD_TOO_LARGE,
            "request body exceeds the 2 MiB limit",
        )
            .into_response(),
        Ok(Err(e)) => (
            StatusCode::BAD_REQUEST,
            format!("failed to read request body: {e}"),
        )
            .into_response(),
        Err(_) => (
            StatusCode::REQUEST_TIMEOUT,
            "request body was not received in time",
        )
            .into_response(),
    }
}

/// Rate-limit buckets: callers presenting the valid Bearer token draw from their own bucket,
/// so traffic that cannot authenticate (any web page can make a browser send `GET /health`)
/// can no longer exhaust the budget of the agent that can.
pub struct SplitRateLimit {
    pub token: Option<String>,
    pub public: Arc<RateLimiterState>,
    pub authenticated: Arc<RateLimiterState>,
}

/// Close the connection after a guard refuses a request (401 / 403 / 415 / 429).
///
/// A refused request costs the server microseconds, but its keep-alive connection then held one
/// of the capped connection slots until the header deadline. A web page (cheap no-cors fetches
/// across `*.localhost` names) or a local process could park every slot that way and lock the
/// agent out (round-4 C4). Callers that are refused get nothing from keeping the connection.
pub async fn close_on_rejection(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    if matches!(
        response.status(),
        StatusCode::UNAUTHORIZED
            | StatusCode::FORBIDDEN
            | StatusCode::UNSUPPORTED_MEDIA_TYPE
            | StatusCode::TOO_MANY_REQUESTS
    ) {
        response.headers_mut().insert(
            axum::http::header::CONNECTION,
            axum::http::HeaderValue::from_static("close"),
        );
    }
    response
}

/// Axum middleware applying [`SplitRateLimit`]: 429 with `Retry-After: 1` when the caller's
/// bucket is empty.
pub async fn split_rate_limit(
    State(limits): State<Arc<SplitRateLimit>>,
    request: Request,
    next: Next,
) -> Response {
    let bucket = match limits.token.as_deref() {
        Some(token) if victauri_core::middleware::bearer_matches(request.headers(), token) => {
            &limits.authenticated
        }
        _ => &limits.public,
    };
    if bucket.try_acquire() {
        next.run(request).await
    } else {
        (
            StatusCode::TOO_MANY_REQUESTS,
            [(axum::http::header::RETRY_AFTER, "1")],
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    async fn spawn_server(limits: ServeLimits) -> std::net::SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = axum::Router::new().route("/", axum::routing::get(|| async { "ok" }));
        tokio::spawn(serve_hardened(
            listener,
            app,
            limits,
            std::future::pending::<()>(),
        ));
        addr
    }

    /// Read until EOF (or a response) — bounded, so a server that never closes fails the test.
    async fn read_to_close(stream: &mut tokio::net::TcpStream, within: Duration) -> Vec<u8> {
        let mut out = Vec::new();
        tokio::time::timeout(within, stream.read_to_end(&mut out))
            .await
            .expect("the server must close a stalled connection")
            .unwrap_or_default();
        out
    }

    /// Limits for tests: generous pending pool and first-byte deadline unless a test is about them.
    fn limits(max_connections: usize, header_read_timeout: Duration) -> ServeLimits {
        ServeLimits {
            max_connections,
            header_read_timeout,
            max_pending: 64,
            first_byte_timeout: Duration::from_secs(10),
        }
    }

    /// Audit N4: a client trickling its request head was never timed out.
    #[tokio::test]
    async fn a_stalled_request_head_is_closed_at_the_header_deadline() {
        let addr = spawn_server(ServeLimits {
            first_byte_timeout: Duration::from_millis(300),
            ..limits(8, Duration::from_millis(300))
        })
        .await;
        let started = Instant::now();
        let mut partial = tokio::net::TcpStream::connect(addr).await.unwrap();
        partial
            .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n")
            .await
            .unwrap();
        let reply = read_to_close(&mut partial, Duration::from_secs(5)).await;
        assert!(
            !String::from_utf8_lossy(&reply).contains("200 OK"),
            "an incomplete head must not be served"
        );
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "{:?}",
            started.elapsed()
        );

        // A silent connection is closed too.
        let mut silent = tokio::net::TcpStream::connect(addr).await.unwrap();
        read_to_close(&mut silent, Duration::from_secs(5)).await;
    }

    /// Audit N4: connections are capped; a waiting client is served once a slot frees up.
    #[tokio::test]
    async fn connections_beyond_the_cap_wait_for_a_free_slot() {
        let addr = spawn_server(limits(1, Duration::from_millis(400))).await;
        // A client that has started a request (and stalls in its head) holds the only slot.
        let mut hog = tokio::net::TcpStream::connect(addr).await.unwrap();
        hog.write_all(b"GET / HTTP/1.1\r\n").await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        let started = Instant::now();
        let mut client = tokio::net::TcpStream::connect(addr).await.unwrap();
        client
            .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let reply = read_to_close(&mut client, Duration::from_secs(5)).await;
        assert!(String::from_utf8_lossy(&reply).contains("200 OK"));
        assert!(
            started.elapsed() >= Duration::from_millis(250),
            "served before the only slot was released ({:?})",
            started.elapsed()
        );
    }

    /// R5-NET1: connections that never send a byte (a page's `<link rel=preconnect>` across
    /// `*.localhost` names — no request, so no guard ever runs) each held a request slot for the
    /// full header deadline, locking the agent out. They must not hold request slots.
    #[tokio::test]
    async fn silent_connections_do_not_block_a_real_request() {
        let addr = spawn_server(limits(2, Duration::from_secs(10))).await;
        let mut silent = Vec::new();
        for _ in 0..20 {
            silent.push(tokio::net::TcpStream::connect(addr).await.unwrap());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        let started = Instant::now();
        let mut client = tokio::net::TcpStream::connect(addr).await.unwrap();
        client
            .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let reply = tokio::time::timeout(
            Duration::from_secs(2),
            read_to_close(&mut client, Duration::from_secs(5)),
        )
        .await
        .expect("a real request must be served while silent connections are open");
        assert!(String::from_utf8_lossy(&reply).contains("200 OK"));
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "{:?}",
            started.elapsed()
        );
        drop(silent);
    }

    /// R5-NET1: a connection that sends nothing is closed at the (short) first-byte deadline,
    /// not the 30 s header deadline.
    #[tokio::test]
    async fn a_silent_connection_is_closed_at_the_first_byte_deadline() {
        let addr = spawn_server(ServeLimits {
            first_byte_timeout: Duration::from_millis(300),
            ..limits(8, Duration::from_secs(30))
        })
        .await;
        let started = Instant::now();
        let mut silent = tokio::net::TcpStream::connect(addr).await.unwrap();
        let reply = read_to_close(&mut silent, Duration::from_secs(5)).await;
        assert!(reply.is_empty(), "{}", String::from_utf8_lossy(&reply));
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
    }

    /// Graceful shutdown does not wait for connections that never sent a byte.
    #[tokio::test]
    async fn shutdown_does_not_wait_for_silent_connections() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = axum::Router::new().route("/", axum::routing::get(|| async { "ok" }));
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(serve_hardened(
            listener,
            app,
            limits(2, Duration::from_secs(30)),
            async move {
                let _ = stop_rx.await;
            },
        ));
        let mut _silent = Vec::new();
        for _ in 0..5 {
            _silent.push(tokio::net::TcpStream::connect(addr).await.unwrap());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        let _ = stop_tx.send(());
        tokio::time::timeout(Duration::from_secs(2), server)
            .await
            .expect("shutdown must not wait for silent connections")
            .unwrap();
    }

    /// R5-NET1: silent connections are still bounded (file descriptors): beyond `max_pending`
    /// a new connection waits in the backlog until the first-byte deadline reaps one.
    #[tokio::test]
    async fn silent_connections_are_bounded_by_the_pending_pool() {
        let addr = spawn_server(ServeLimits {
            max_pending: 2,
            first_byte_timeout: Duration::from_millis(400),
            ..limits(8, Duration::from_secs(30))
        })
        .await;
        let _silent_a = tokio::net::TcpStream::connect(addr).await.unwrap();
        let _silent_b = tokio::net::TcpStream::connect(addr).await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        let started = Instant::now();
        let mut client = tokio::net::TcpStream::connect(addr).await.unwrap();
        client
            .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let reply = read_to_close(&mut client, Duration::from_secs(5)).await;
        assert!(String::from_utf8_lossy(&reply).contains("200 OK"));
        assert!(
            started.elapsed() >= Duration::from_millis(250),
            "accepted beyond the pending pool ({:?})",
            started.elapsed()
        );
    }

    /// A body that never delivers a byte.
    struct StalledBody;

    impl hyper::body::Body for StalledBody {
        type Data = axum::body::Bytes;
        type Error = std::io::Error;
        fn poll_frame(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Result<hyper::body::Frame<Self::Data>, Self::Error>>> {
            std::task::Poll::Pending
        }
    }

    /// Audit N4: a client that sends headers and then stalls its body held a request slot
    /// forever.
    #[tokio::test]
    async fn a_stalled_request_body_times_out() {
        use tower::ServiceExt;
        let app = axum::Router::new()
            .route("/", axum::routing::post(|| async { "ok" }))
            .layer(axum::middleware::from_fn(|req, next| {
                buffer_body(req, next, Duration::from_millis(200))
            }));
        let req = Request::post("/")
            .body(axum::body::Body::new(StalledBody))
            .unwrap();
        let resp = tokio::time::timeout(Duration::from_secs(5), app.oneshot(req))
            .await
            .expect("a stalled body must not hang the request")
            .unwrap();
        assert_eq!(resp.status(), StatusCode::REQUEST_TIMEOUT);
    }

    fn server_request(method: &str, uri: &str, body: Vec<u8>) -> Request {
        Request::builder()
            .method(method)
            .uri(uri)
            .header("host", "127.0.0.1:7373")
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream")
            .body(axum::body::Body::from(body))
            .unwrap()
    }

    fn test_app(limiter: Arc<RateLimiterState>) -> axum::Router {
        super::super::build_app_full(
            Arc::new(crate::VictauriState::for_tests()),
            Arc::new(NoBridge),
            None,
            Some(limiter),
        )
    }

    struct NoBridge;

    impl crate::bridge::WebviewBridge for NoBridge {
        fn eval_webview(&self, _l: Option<&str>, _s: &str) -> Result<(), String> {
            Err("no webview".to_string())
        }
        fn get_window_states(&self, _l: Option<&str>) -> Vec<victauri_core::WindowState> {
            Vec::new()
        }
        fn list_window_labels(&self) -> Vec<String> {
            Vec::new()
        }
        fn get_native_handle(&self, _l: Option<&str>) -> Result<isize, String> {
            Err("no handle".to_string())
        }
        fn manage_window(&self, _l: Option<&str>, _a: &str) -> Result<String, String> {
            Err("no window".to_string())
        }
        fn resize_window(&self, _l: Option<&str>, _w: u32, _h: u32) -> Result<(), String> {
            Ok(())
        }
        fn move_window(&self, _l: Option<&str>, _x: i32, _y: i32) -> Result<(), String> {
            Ok(())
        }
        fn set_window_title(&self, _l: Option<&str>, _t: &str) -> Result<(), String> {
            Ok(())
        }
    }

    /// Audit N5: rmcp reads `/mcp` bodies itself with a 4 MiB default, so the 2 MiB
    /// `DefaultBodyLimit` never applied there.
    #[tokio::test]
    async fn an_oversized_mcp_body_is_refused() {
        use tower::ServiceExt;
        let app = test_app(Arc::new(RateLimiterState::new(100)));
        let mut body = br#"{"jsonrpc":"2.0","id":1,"method":"ping","params":{"pad":""#.to_vec();
        body.extend(std::iter::repeat_n(b'x', 3 * 1024 * 1024));
        body.extend(br#""}}"#);
        let resp = app
            .oneshot(server_request("POST", "/mcp", body))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    /// Round-4 C4: a guard-refused request left its connection open and idle until the header
    /// deadline, so a page (no-cors fetches across `*.localhost` names) or a local process could
    /// park every connection slot with requests that were refused in microseconds.
    #[tokio::test]
    async fn a_refused_request_closes_its_connection() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(serve_hardened(
            listener,
            test_app(Arc::new(RateLimiterState::new(100))),
            limits(8, Duration::from_secs(30)),
            std::future::pending::<()>(),
        ));
        let mut conn = tokio::net::TcpStream::connect(addr).await.unwrap();
        conn.write_all(
            b"GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\nSec-Fetch-Site: cross-site\r\n\r\n",
        )
        .await
        .unwrap();
        let reply = read_to_close(&mut conn, Duration::from_secs(3)).await;
        let reply = String::from_utf8_lossy(&reply);
        assert!(reply.starts_with("HTTP/1.1 403"), "{reply}");
    }

    /// Audit N3, end to end: a page-initiated `GET /health` is refused and spends no token.
    #[tokio::test]
    async fn the_server_refuses_browser_requests_before_rate_limiting() {
        use tower::ServiceExt;
        let limiter = Arc::new(RateLimiterState::new(10));
        let app = test_app(Arc::clone(&limiter));
        for _ in 0..20 {
            let mut req = server_request("GET", "/health", Vec::new());
            req.headers_mut()
                .insert("sec-fetch-site", "cross-site".parse().unwrap());
            let resp = app.clone().oneshot(req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        }
        assert_eq!(limiter.current_tokens(), 10);
        let resp = app
            .oneshot(server_request("GET", "/health", Vec::new()))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn authenticated_callers_have_their_own_rate_limit_bucket() {
        use tower::ServiceExt;
        let limits = Arc::new(SplitRateLimit {
            token: Some("tok".to_string()),
            public: Arc::new(RateLimiterState::new(1)),
            authenticated: Arc::new(RateLimiterState::new(1)),
        });
        let app = axum::Router::new()
            .route("/", axum::routing::get(|| async { "ok" }))
            .layer(axum::middleware::from_fn_with_state(
                Arc::clone(&limits),
                split_rate_limit,
            ));
        let req = |auth: Option<&str>| {
            let mut b = Request::builder().uri("/");
            if let Some(a) = auth {
                b = b.header("authorization", a);
            }
            b.body(axum::body::Body::empty()).unwrap()
        };
        // Unauthenticated traffic drains the public bucket…
        assert_eq!(app.clone().oneshot(req(None)).await.unwrap().status(), 200);
        assert_eq!(app.clone().oneshot(req(None)).await.unwrap().status(), 429);
        // …and a wrong token is unauthenticated traffic…
        assert_eq!(
            app.clone()
                .oneshot(req(Some("Bearer nope")))
                .await
                .unwrap()
                .status(),
            429
        );
        // …but the agent holding the token is unaffected.
        assert_eq!(
            app.clone()
                .oneshot(req(Some("Bearer tok")))
                .await
                .unwrap()
                .status(),
            200
        );
    }
}
