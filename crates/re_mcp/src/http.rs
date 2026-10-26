//! MCP over streamable HTTP on localhost (ADR-0046).
//!
//! rmcp's [`StreamableHttpService`] speaks the protocol; hyper drives it on a
//! loopback socket. In front of it sits one check: every request carries
//! `Authorization: Bearer <token>`, or it gets 401 before rmcp sees it. rmcp
//! then refuses a `Host` that is not this server's loopback authority and
//! any request with an `Origin` header (403), so a web page cannot reach the
//! server even if it learns the port.
//!
//! The same [`RealEngineServer`] answers as over stdio: every HTTP session
//! shares the one engine handle, so the policy, the tiers, the agent session
//! and the one-call-at-a-time order are those of stdio.

use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll};

use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use hyper_util::service::TowerToHyperService;
use re_command::EngineHandle;
use re_core::Diagnostic;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};

use crate::server::RealEngineServer;
use crate::{McpOptions, codes};

/// The environment variable that sets the token (`real mcp --http`).
pub const TOKEN_ENV: &str = "REAL_MCP_TOKEN";

/// The shortest token accepted from [`TOKEN_ENV`].
pub const MIN_TOKEN_LEN: usize = 32;

/// The path the MCP endpoint is served at.
pub const ENDPOINT: &str = "/mcp";

/// The default address: loopback, any free port.
pub const DEFAULT_ADDR: &str = "127.0.0.1:0";

/// Parses an address for `--http` and refuses one that is not loopback
/// (`RE8502`): there is no TLS, so the token must not cross a network.
///
/// # Errors
///
/// `RE8502` for an unparsable or non-loopback address.
pub fn parse_addr(text: &str) -> Result<SocketAddr, Diagnostic> {
    let addr: SocketAddr = match text.parse() {
        Ok(addr) => addr,
        // `localhost:PORT` is common in client configs.
        Err(_) => match text.strip_prefix("localhost:").map(str::parse::<u16>) {
            Some(Ok(port)) => SocketAddr::from(([127, 0, 0, 1], port)),
            _ => {
                return Err(Diagnostic::error(
                    codes::NOT_LOOPBACK,
                    format!("`{text}` is not an address such as 127.0.0.1:8765"),
                )
                .with_hint("pass a loopback address and port, such as 127.0.0.1:8765, or 127.0.0.1:0 for any free port"));
            }
        },
    };
    if !addr.ip().is_loopback() {
        return Err(Diagnostic::error(
            codes::NOT_LOOPBACK,
            format!("`{addr}` is not a loopback address; real mcp --http serves this machine only"),
        )
        .with_hint("use 127.0.0.1 or [::1]; the HTTP transport has no TLS, so the token must not cross a network"));
    }
    Ok(addr)
}

/// The token: [`TOKEN_ENV`] when it is set, otherwise a new random one.
///
/// # Errors
///
/// `RE8503` when [`TOKEN_ENV`] is set but shorter than [`MIN_TOKEN_LEN`].
pub fn token_from_env() -> Result<String, Diagnostic> {
    match std::env::var(TOKEN_ENV) {
        Ok(token) => check_token(&token).map(|()| token),
        Err(std::env::VarError::NotPresent) => Ok(generate_token()),
        Err(std::env::VarError::NotUnicode(_)) => Err(Diagnostic::error(
            codes::WEAK_TOKEN,
            format!("{TOKEN_ENV} is set but is not valid text"),
        )
        .with_hint(format!(
            "unset {TOKEN_ENV} to have real mcp generate a token, or set it to printable ASCII"
        ))),
    }
}

/// Checks a token given by the user (`RE8503`).
///
/// # Errors
///
/// `RE8503` when it is too short or has characters a header cannot carry.
pub fn check_token(token: &str) -> Result<(), Diagnostic> {
    let printable = token.bytes().all(|b| b.is_ascii_graphic());
    if token.len() < MIN_TOKEN_LEN || !printable {
        return Err(Diagnostic::error(
            codes::WEAK_TOKEN,
            format!(
                "{TOKEN_ENV} must be at least {MIN_TOKEN_LEN} printable ASCII characters with no spaces (it has {})",
                token.len()
            ),
        )
        .with_hint(format!("unset {TOKEN_ENV} to have real mcp generate a token, or set a longer random one")));
    }
    Ok(())
}

/// A new random token: 64 hex digits from two v4 UUIDs (244 random bits from
/// the operating system).
pub fn generate_token() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

/// A bound, not yet serving, HTTP listener: the port is known before the
/// server starts, so callers can write it where clients find it. Its
/// `Debug` leaves the token out.
pub struct HttpListener {
    listener: std::net::TcpListener,
    addr: SocketAddr,
    token: Arc<str>,
}

impl std::fmt::Debug for HttpListener {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpListener")
            .field("addr", &self.addr)
            .field("token", &"<redacted>")
            .finish()
    }
}

impl HttpListener {
    /// Binds `addr` (which must be loopback) with `token`.
    ///
    /// # Errors
    ///
    /// `RE8502` for a non-loopback address, `RE8503` for a short token,
    /// `RE8504` when the address cannot be bound.
    pub fn bind(addr: SocketAddr, token: &str) -> Result<HttpListener, Diagnostic> {
        if !addr.ip().is_loopback() {
            return Err(Diagnostic::error(
                codes::NOT_LOOPBACK,
                format!("`{addr}` is not a loopback address"),
            ));
        }
        check_token(token)?;
        let listener = std::net::TcpListener::bind(addr)
            .and_then(|l| l.set_nonblocking(true).map(|()| l))
            .map_err(|e| {
                Diagnostic::error(codes::BIND_FAILED, format!("cannot listen on {addr}: {e}"))
                    .with_hint("another program may use this port; pick another, or 127.0.0.1:0 for any free port")
            })?;
        let addr = listener.local_addr().map_err(|e| {
            Diagnostic::error(
                codes::BIND_FAILED,
                format!("cannot read the bound address: {e}"),
            )
        })?;
        Ok(HttpListener {
            listener,
            addr,
            token: token.into(),
        })
    }

    /// The bound address (with the real port when 0 was asked for).
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    /// The URL clients connect to: `http://127.0.0.1:PORT/mcp`.
    pub fn url(&self) -> String {
        format!("http://{}{ENDPOINT}", self.addr)
    }

    /// The token clients send.
    pub fn token(&self) -> &str {
        &self.token
    }
}

/// The `Host` values rmcp accepts: the loopback names with the bound port.
fn allowed_hosts(addr: SocketAddr) -> Vec<String> {
    let port = addr.port();
    let mut hosts = vec![format!("localhost:{port}")];
    match addr.ip() {
        IpAddr::V4(ip) => hosts.push(format!("{ip}:{port}")),
        IpAddr::V6(ip) => hosts.push(format!("[{ip}]:{port}")),
    }
    hosts
}

/// True when `header` is `Bearer <token>` (the scheme in any case, RFC
/// 7235), the token compared in constant time.
fn authorized(header: Option<&hyper::header::HeaderValue>, token: &str) -> bool {
    let Some(given) = header
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split_once(' '))
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
        .map(|(_, given)| given.trim())
    else {
        return false;
    };
    let (a, b) = (given.as_bytes(), token.as_bytes());
    // The length is not secret (it is fixed or chosen by the user).
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// An empty response with `status` (its body type is rmcp's, inferred).
fn refuse<B: Default>(status: hyper::StatusCode) -> hyper::Response<B> {
    let mut response = hyper::Response::new(B::default());
    *response.status_mut() = status;
    if status == hyper::StatusCode::UNAUTHORIZED {
        response.headers_mut().insert(
            hyper::header::WWW_AUTHENTICATE,
            hyper::header::HeaderValue::from_static("Bearer"),
        );
    }
    response
}

/// The most of a refused request's body [`drain`] reads.
const DRAIN_LIMIT: usize = 64 * 1024;

/// Reads and drops a refused request's body, up to [`DRAIN_LIMIT`] bytes
/// or one second, before the refusal is sent. Closing a socket with unread
/// data resets it, and on Windows the client then loses the response
/// (error 10053). A larger or slower body is left unread: that sender gets
/// the reset.
async fn drain(body: Incoming) {
    use hyper::body::Body as _;
    let mut body = std::pin::pin!(body);
    let read_all = async {
        let mut read = 0;
        while read <= DRAIN_LIMIT {
            match std::future::poll_fn(|cx| body.as_mut().poll_frame(cx)).await {
                Some(Ok(frame)) => read += frame.data_ref().map_or(0, |data| data.len()),
                _ => break,
            }
        }
    };
    let _ = tokio::time::timeout(std::time::Duration::from_secs(1), read_all).await;
}

/// A request's body as rmcp reads it, through a handle shared with the
/// connection. rmcp refuses a foreign `Host`, a browser `Origin`, another
/// method or a missing `Accept` without reading the body, so what it leaves
/// unread is drained before its refusal goes out ([`drain`] says why).
struct SharedBody(Arc<Mutex<Incoming>>);

impl hyper::body::Body for SharedBody {
    type Data = hyper::body::Bytes;
    type Error = hyper::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<hyper::body::Frame<Self::Data>, Self::Error>>> {
        let mut body = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        Pin::new(&mut *body).poll_frame(cx)
    }

    fn is_end_stream(&self) -> bool {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_end_stream()
    }

    fn size_hint(&self) -> hyper::body::SizeHint {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .size_hint()
    }
}

/// Serves MCP over HTTP on `listener` until `stop` completes. Must run inside
/// a multi-threaded tokio runtime.
///
/// # Errors
///
/// When the listener cannot be handed to tokio.
pub async fn serve_http_async(
    handle: EngineHandle,
    options: McpOptions,
    listener: HttpListener,
    stop: impl Future<Output = ()> + Send,
) -> anyhow::Result<()> {
    let HttpListener {
        listener,
        addr,
        token,
    } = listener;
    let listener = tokio::net::TcpListener::from_std(listener)?;
    let server = RealEngineServer::new(handle, options);
    let config = StreamableHttpServerConfig::default()
        .with_allowed_hosts(allowed_hosts(addr))
        .enforce_origin_validation();
    let cancel = config.cancellation_token.clone();
    let sessions = Arc::new(LocalSessionManager::default());
    let mcp = StreamableHttpService::new(move || Ok(server.clone()), Arc::clone(&sessions), config);
    let mut connections = tokio::task::JoinSet::new();
    let mut stop = std::pin::pin!(stop);
    loop {
        let stream = tokio::select! {
            () = &mut stop => break,
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => stream,
                // A connection that failed before it was accepted (reset by the
                // peer, out of file descriptors) ends only itself; pause so a
                // persistent error does not spin.
                Err(e) => {
                    eprintln!("real mcp: accept failed: {e}");
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    continue;
                }
            },
        };
        let mcp = TowerToHyperService::new(mcp.clone());
        let token = Arc::clone(&token);
        connections.spawn(async move {
            let service = service_fn(move |request: hyper::Request<Incoming>| {
                let mcp = mcp.clone();
                let token = Arc::clone(&token);
                async move {
                    use hyper::service::Service as _;
                    let refusal =
                        if !authorized(request.headers().get(hyper::header::AUTHORIZATION), &token)
                        {
                            Some(hyper::StatusCode::UNAUTHORIZED)
                        } else if request.uri().path() != ENDPOINT {
                            Some(hyper::StatusCode::NOT_FOUND)
                        } else {
                            None
                        };
                    if let Some(status) = refusal {
                        drain(request.into_body()).await;
                        return Ok(refuse(status));
                    }
                    let (parts, body) = request.into_parts();
                    let body = Arc::new(Mutex::new(body));
                    let response = mcp
                        .call(hyper::Request::from_parts(
                            parts,
                            SharedBody(Arc::clone(&body)),
                        ))
                        .await;
                    // Whatever rmcp has let go of unread is drained.
                    if let Ok(body) = Arc::try_unwrap(body) {
                        let body = body.into_inner().unwrap_or_else(PoisonError::into_inner);
                        if !hyper::body::Body::is_end_stream(&body) {
                            drain(body).await;
                        }
                    }
                    response
                }
            });
            let _ = hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(stream), service)
                .await;
        });
        // Forget finished connections as we go.
        while connections.try_join_next().is_some() {}
    }
    // Every session's server holds a clone of the engine handle, which must
    // go for the engine to stop. The token ends the SSE streams; closing each
    // session ends its worker and its server (a session left alone would
    // live until rmcp's idle timeout); then the connections go.
    cancel.cancel();
    let open = std::mem::take(&mut *sessions.sessions.write().await);
    for (_, session) in open {
        let _ = session.close().await;
    }
    connections.abort_all();
    while connections.join_next().await.is_some() {}
    Ok(())
}

/// True when stdin is a pipe or socket a parent process holds, so its end
/// means "stop". A terminal is not watched (reading it from a background
/// job would stop the process), nor is a file such as `/dev/null` (it ends
/// at once); those servers run until they are interrupted.
pub fn stdin_is_a_pipe() -> bool {
    #[cfg(unix)]
    {
        use std::os::fd::AsFd;
        use std::os::unix::fs::FileTypeExt;
        // fstat on a duplicate of fd 0, so no `/dev/stdin` is needed.
        std::io::stdin()
            .as_fd()
            .try_clone_to_owned()
            .and_then(|fd| std::fs::File::from(fd).metadata())
            .map(|m| m.file_type().is_fifo() || m.file_type().is_socket())
            .unwrap_or(false)
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        const FILE_TYPE_PIPE: u32 = 0x0003;
        // kernel32 is always linked on Windows; this saves a dependency.
        unsafe extern "system" {
            fn GetFileType(file: *mut std::ffi::c_void) -> u32;
        }
        let handle = std::io::stdin().as_raw_handle();
        if handle.is_null() {
            return false;
        }
        // SAFETY: GetFileType only reads the handle's type; an invalid
        // handle gives FILE_TYPE_UNKNOWN.
        unsafe { GetFileType(handle.cast()) == FILE_TYPE_PIPE }
    }
    #[cfg(not(any(unix, windows)))]
    {
        false
    }
}

/// Completes when this process's stdin reaches its end, if stdin is a pipe
/// ([`stdin_is_a_pipe`]); otherwise never. `real mcp --http` stops then, as
/// `real mcp` over stdio does when its client closes the pipe. The read runs
/// on its own thread.
pub fn stdin_closed() -> impl Future<Output = ()> + Send {
    let (closed, wait) = tokio::sync::oneshot::channel::<()>();
    if stdin_is_a_pipe() {
        std::thread::spawn(move || {
            let _ = std::io::copy(&mut std::io::stdin().lock(), &mut std::io::sink());
            let _ = closed.send(());
        });
    } else {
        // Never sent: the server runs until the process is interrupted.
        std::mem::forget(closed);
    }
    async move {
        let _ = wait.await;
    }
}

/// Serves MCP over HTTP on `listener` until `stop` completes. Runs its own
/// tokio runtime, so call it from a plain thread.
///
/// # Errors
///
/// When the runtime cannot start or the listener cannot be handed to tokio.
pub fn serve_http(
    handle: EngineHandle,
    options: McpOptions,
    listener: HttpListener,
    stop: impl Future<Output = ()> + Send,
) -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let result = runtime.block_on(serve_http_async(handle, options, listener, stop));
    // Engine calls in flight finish on the blocking pool; don't wait forever.
    runtime.shutdown_timeout(std::time::Duration::from_secs(5));
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_loopback_addresses_are_accepted() {
        assert_eq!(parse_addr("127.0.0.1:0").unwrap().port(), 0);
        assert!(parse_addr("[::1]:8765").unwrap().ip().is_loopback());
        assert_eq!(
            parse_addr("localhost:8765").unwrap(),
            "127.0.0.1:8765".parse().unwrap()
        );
        for bad in [
            "0.0.0.0:8765",
            "192.168.1.2:80",
            "[::]:1",
            "example.com:80",
            "8765",
        ] {
            assert_eq!(
                parse_addr(bad).unwrap_err().code,
                codes::NOT_LOOPBACK,
                "{bad}"
            );
        }
    }

    #[test]
    fn tokens_are_long_and_compared_exactly() {
        let token = generate_token();
        assert_eq!(token.len(), 64);
        assert_ne!(token, generate_token());
        check_token(&token).unwrap();
        assert_eq!(check_token("short").unwrap_err().code, codes::WEAK_TOKEN);
        assert_eq!(
            check_token(&format!("{} x", "a".repeat(40)))
                .unwrap_err()
                .code,
            codes::WEAK_TOKEN
        );

        let header = |v: &str| hyper::header::HeaderValue::from_str(v).unwrap();
        assert!(authorized(
            Some(&header(&format!("Bearer {token}"))),
            &token
        ));
        assert!(!authorized(
            Some(&header(&format!("Bearer {token}x"))),
            &token
        ));
        assert!(authorized(
            Some(&header(&format!("bearer {token}"))),
            &token
        ));
        assert!(!authorized(
            Some(&header(&format!("Basic {token}"))),
            &token
        ));
        assert!(!authorized(Some(&header(&token)), &token));
        assert!(!authorized(None, &token));
    }

    #[test]
    fn hosts_name_the_bound_port() {
        assert_eq!(
            allowed_hosts("127.0.0.1:8765".parse().unwrap()),
            ["localhost:8765", "127.0.0.1:8765"]
        );
        assert_eq!(
            allowed_hosts("[::1]:9".parse().unwrap()),
            ["localhost:9", "[::1]:9"]
        );
    }

    #[test]
    fn binding_refuses_other_addresses_and_short_tokens() {
        let token = generate_token();
        let bound = HttpListener::bind("127.0.0.1:0".parse().unwrap(), &token).unwrap();
        assert_ne!(bound.local_addr().port(), 0);
        assert!(bound.url().starts_with("http://127.0.0.1:"));
        assert!(bound.url().ends_with("/mcp"));
        let taken = HttpListener::bind(bound.local_addr(), &token).unwrap_err();
        assert_eq!(taken.code, codes::BIND_FAILED);
        assert_eq!(
            HttpListener::bind("0.0.0.0:0".parse().unwrap(), &token)
                .unwrap_err()
                .code,
            codes::NOT_LOOPBACK
        );
        assert_eq!(
            HttpListener::bind("127.0.0.1:0".parse().unwrap(), "short")
                .unwrap_err()
                .code,
            codes::WEAK_TOKEN
        );
    }
}
