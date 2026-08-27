//! One tracing line per request: `{method, path_label, status, bytes_in,
//! bytes_out, us}`. The line is emitted by the response-body wrapper exactly
//! once — at clean stream end, or from `Drop` when a client vanishes
//! mid-stream — so streamed GET answers are measured without buffering
//! their bodies.

use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};

use axum::body::Body;
use http_body::Body as _HttpBody;

/// The erased body type axum responses carry. axum 0.7 does not re-export
/// its own alias at this path, so name the underlying combinator directly.
type BoxBody = http_body_util::combinators::UnsyncBoxBody<bytes::Bytes, axum::Error>;
use axum::http::{Method, Request};
use axum::middleware::Next;
use axum::response::Response;
use bytes::Buf;

/// Per-request counters shared between handlers (bytes in) and the response
/// body wrapper (bytes out), surfaced to handlers as an Extension.
#[derive(Debug)]
pub struct RequestMetrics {
    started_at: std::time::Instant,
    method: String,
    /// Stable path label for logs. Hash-addressed targets shrink to an
    /// 8-hex prefix so lines stay greppable without log files becoming a
    /// content index themselves.
    path_label: String,
    status: AtomicU64,
    pub bytes_in: AtomicU64,
    bytes_out: AtomicU64,
    emitted: AtomicBool,
    /// Set when the response body polled clean end-of-stream. Bodies with an
    /// exact size hint may be dropped by the server once that many bytes are
    /// delivered without a final None poll; the Drop path consults this plus
    /// the delivered count to tell "served" from "client vanished".
    eof: AtomicBool,
    /// Expected exact body size when the size hint is precise, else None.
    exact_len: AtomicU64,
}

impl RequestMetrics {
    /// Handlers streaming request bodies call this while reading; uploads
    /// accumulate their staged size here so the final line reflects truth
    /// even when Content-Length lied.
    pub fn add_bytes_in(&self, n: u64) {
        self.bytes_in.fetch_add(n, Ordering::Relaxed);
    }

    fn mark_eof(&self) {
        self.eof.store(true, Ordering::SeqCst);
    }

    /// u64::MAX sentinel = no exact size hint (streamed answer).
    fn set_exact_len(&self, len: Option<u64>) {
        self.exact_len
            .store(len.unwrap_or(u64::MAX), Ordering::Relaxed);
    }

    fn served_cleanly(&self, delivered: u64) -> bool {
        if self.eof.load(Ordering::SeqCst) {
            return true;
        }
        // Exact-size bodies are complete once delivered bytes reach the
        // declared length, even if no trailing None poll happened.
        match self.exact_len.load(Ordering::Relaxed) {
            u64::MAX => false,
            expected => delivered >= expected,
        }
    }

    fn label_for(method: &Method, path: &str) -> String {
        if path == "/" {
            return "health".to_string();
        }
        let rest = path.strip_prefix('/').unwrap_or(path);
        let mut parts = rest.split('/');
        match (parts.next(), parts.next(), parts.next()) {
            (Some(hashish), None, None) if hashish.len() >= 8 && !hashish.contains('-') => {
                format!("blob/{0}", &hashish[..8.min(hashish.len())])
            }
            (Some("upload"), None, None) => "upload".to_string(),
            (Some("list"), Some(npub), None) if npub.len() >= 10 => {
                format!("list/{0}", &npub[..10])
            }
            _ => {
                let _ = method;
                path.chars().take(32).collect()
            }
        }
    }

    fn init(method: Method, path_label: String, content_length: Option<u64>) -> Self {
        RequestMetrics {
            started_at: std::time::Instant::now(),
            method: method.to_string(),
            path_label,
            status: AtomicU64::new(0),
            bytes_in: AtomicU64::new(content_length.unwrap_or(0)),
            bytes_out: AtomicU64::new(0),
            emitted: AtomicBool::new(false),
            eof: AtomicBool::new(false),
            exact_len: AtomicU64::new(u64::MAX),
        }
    }

    fn elapsed_us(&self) -> u128 {
        self.started_at.elapsed().as_micros()
    }

    /// The one line, guarded by the `emitted` flag so whichever of
    /// {EOF, Drop} happens first wins and only ever happens once.
    fn emit(&self, aborted: bool) {
        if self.emitted.swap(true, Ordering::SeqCst) {
            return;
        }
        tracing::info!(
            method = %self.method,
            path = %self.path_label,
            status = self.status.load(Ordering::Relaxed),
            bytes_in = self.bytes_in.load(Ordering::Relaxed),
            bytes_out = self.bytes_out.load(Ordering::Relaxed),
            us = u64::try_from(self.elapsed_us()).unwrap_or(u64::MAX),
            aborted,
            "request"
        );
    }
}

/// The single middleware wrapping every route.
pub async fn track(mut request: Request<Body>, next: Next) -> Response {
    let content_length = request
        .headers()
        .get(http::header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    let metrics = Arc::new(RequestMetrics::init(
        request.method().clone(),
        RequestMetrics::label_for(request.method(), request.uri().path()),
        content_length,
    ));

    // Downstream handlers that stream bodies push totals back through here.
    request.extensions_mut().insert(metrics.clone());

    let mut response = next.run(request).await;
    // Record early enough that both EOF and Drop paths report the real code.
    metrics.status.store(u64::from(response.status().as_u16()), Ordering::Relaxed);
    metrics.set_exact_len(response.body_mut().size_hint().exact());

    let counted = CountedBody {
        inner: std::mem::take(response.body_mut()),
        metrics,
        delivered: 0,
    };
    let boxed: BoxBody = http_body_util::combinators::UnsyncBoxBody::new(counted);
    *response.body_mut() = Body::new(boxed);
    response
}

/// Forwards frames while counting payload bytes; emits the request line at
/// stream end. Clean EOF reports normally; any other drop (including client
/// disconnects and upstream errors after headers) marks `aborted=true`.
struct CountedBody {
    /// axum's own body wrapper implements http_body::Body, so forwarding is
    /// a plain poll-through; the BoxBody alias matters only for the erased
    /// output assignment.
    inner: Body,
    metrics: Arc<RequestMetrics>,
    delivered: u64,
}

impl Drop for CountedBody {
    fn drop(&mut self) {
        // No trailing None poll: either the client vanished mid-stream or
        // the server stopped once an exact content-length was satisfied.
        self.metrics.emit(!self.metrics.served_cleanly(self.delivered));
    }
}

impl http_body::Body for CountedBody {
    type Data = bytes::Bytes;
    type Error = axum::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
        match Pin::new(&mut self.inner).poll_frame(cx) {
            Poll::Ready(Some(Ok(frame))) => {
                if frame.is_data() {
                    let n = frame.data_ref().map(Buf::remaining).unwrap_or(0);
                    self.delivered += n as u64;
                    self.metrics.bytes_out.fetch_add(u64::try_from(n).unwrap_or(u64::MAX), Ordering::Relaxed);
                }
                Poll::Ready(Some(Ok(frame)))
            }
            // Clean end-of-stream beats the Drop fallback: flip emitted first
            // so the pending Drop observes an already-emitted line.
            Poll::Ready(None) => {
                self.metrics.mark_eof();
                self.metrics.emit(false);
                Poll::Ready(None)
            }
            Poll::Ready(Some(Err(err))) => {
                self.metrics.emit(true);
                Poll::Ready(Some(Err(err)))
            }
            Poll::Pending => Poll::Pending,
        }
    }

    fn size_hint(&self) -> http_body::SizeHint {
        self.inner.size_hint()
    }
}
