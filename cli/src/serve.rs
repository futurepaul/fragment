//! `fragment model --serve`: a local OpenAI-compatible endpoint on
//! 127.0.0.1 (`POST /v1/chat/completions`, and OpenRouter's path
//! `/api/v1/chat/completions`, streaming or not) that forwards each request
//! to the platform's model endpoint signed with this CLI's key, and relays
//! the answer as it arrives. Anything that speaks to an OpenAI-compatible
//! provider or to OpenRouter (goose on a computer) then needs no key: the
//! platform checks the model and bills the computer's owner. It binds the
//! loopback address only, so it answers this machine alone. Each call is a
//! line on stderr (`call_log`): how long it took, which provider answered,
//! and how much of its prompt was cached.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use fragment_proto::limits::MODEL_BODY_MAX_BYTES;

use crate::api::Client;

/// The port `--serve` listens on unless told another.
pub const PORT: u16 = 8765;
/// Requests at once; past it, 503.
const CONNECTIONS_MAX: usize = 16;
/// A request's head (its line and headers), at most.
const HEAD_MAX_BYTES: u64 = 16 * 1024;
/// How long a forwarded call may take: the platform's own bound (120 s),
/// and room for the hop.
const CALL_TIMEOUT: Duration = Duration::from_secs(150);
const PLATFORM_PATH: &str = "/api/model/chat/completions";
/// The end of an answer kept to read its usage from (a stream's last chunk).
const TAIL_BYTES: usize = 64 * 1024;

pub fn serve(client: Client, port: u16) -> Result<()> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port)).with_context(|| format!("binding 127.0.0.1:{port}"))?;
    let at = listener.local_addr()?;
    assert!(at.ip().is_loopback(), "the model endpoint answers this machine alone");
    println!("serving http://{at}/v1 (POST /v1/chat/completions, or OpenRouter's /api/v1/chat/completions), signed as {}", client.id.npub());
    std::io::stdout().flush()?;
    let (client, open) = (Arc::new(client), Arc::new(AtomicUsize::new(0)));
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        if open.fetch_add(1, Ordering::SeqCst) >= CONNECTIONS_MAX {
            open.fetch_sub(1, Ordering::SeqCst);
            let _ = answer(&mut stream, 503, "application/json", br#"{"error":{"message":"too many requests at once"}}"#);
            continue;
        }
        let (client, open) = (Arc::clone(&client), Arc::clone(&open));
        std::thread::spawn(move || {
            if let Err(e) = one(&client, stream) {
                eprintln!("model --serve: {e:#}");
            }
            open.fetch_sub(1, Ordering::SeqCst);
        });
    }
    Ok(())
}

/// One request, answered and closed.
fn one(client: &Client, stream: TcpStream) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    let mut out = stream.try_clone()?;
    let mut reader = BufReader::new(stream);
    let (line, length) = {
        let mut head = (&mut reader).take(HEAD_MAX_BYTES);
        let mut line = String::new();
        head.read_line(&mut line)?;
        let mut length: Option<usize> = None;
        loop {
            let mut header = String::new();
            if head.read_line(&mut header)? == 0 {
                return answer(&mut out, 431, "text/plain", b"the request's head is too long");
            }
            let header = header.trim_end();
            if header.is_empty() {
                break;
            }
            if let Some((k, v)) = header.split_once(':') {
                if k.eq_ignore_ascii_case("content-length") {
                    length = v.trim().parse().ok();
                }
            }
        }
        (line, length)
    };
    let mut words = line.split_whitespace();
    match (words.next(), words.next()) {
        (Some("GET"), Some("/health")) => return answer(&mut out, 200, "text/plain", b"ok"),
        (Some("POST"), Some("/v1/chat/completions" | "/api/v1/chat/completions")) => {}
        _ => return answer(&mut out, 404, "application/json", br#"{"error":{"message":"POST /v1/chat/completions (or /api/v1/chat/completions) is all this serves"}}"#),
    }
    let Some(length) = length.filter(|n| *n <= MODEL_BODY_MAX_BYTES) else {
        return answer(&mut out, 413, "application/json", br#"{"error":{"message":"a request carries its content-length, at most 8 MiB"}}"#);
    };
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    let t0 = Instant::now();
    let mut forwarded = match client.post_streaming(PLATFORM_PATH, body, CALL_TIMEOUT) {
        Ok(r) => r,
        Err(e) => return answer(&mut out, 502, "application/json", serde_json::json!({ "error": { "message": format!("{e:#}") } }).to_string().as_bytes()),
    };
    // the platform's answer, as it arrives: its status, its type, and its
    // bytes (a stream's chunks), ended by closing the connection
    let status = forwarded.status().as_u16();
    let kind = forwarded.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("application/json").to_string();
    write!(out, "HTTP/1.1 {status} {}\r\ncontent-type: {kind}\r\ncache-control: no-store\r\nconnection: close\r\n\r\n", reason(status))?;
    let (first, mut tail) = (t0.elapsed(), Vec::new());
    let mut piece = [0u8; 16 * 1024];
    loop {
        let n = forwarded.read(&mut piece)?;
        if n == 0 {
            eprintln!("{}", call_log(status, first, t0.elapsed(), &tail));
            return Ok(());
        }
        out.write_all(&piece[..n])?;
        out.flush()?;
        tail.extend_from_slice(&piece[..n]);
        if tail.len() > 2 * TAIL_BYTES {
            tail.drain(..tail.len() - TAIL_BYTES);
        }
    }
}

/// One call, as a line of JSON (on stderr): when it ended, its status, how
/// long to the answer's head and to its end, and what its usage says (the
/// provider that answered, the prompt's tokens and how many were cached),
/// read from the end of the answer (a stream's last chunks).
fn call_log(status: u16, first: Duration, total: Duration, tail: &[u8]) -> serde_json::Value {
    let text = String::from_utf8_lossy(tail);
    let (mut provider, mut model, mut usage) = (serde_json::Value::Null, serde_json::Value::Null, serde_json::Value::Null);
    let lines = text.lines().map(|l| l.strip_prefix("data: ").unwrap_or(l));
    for v in lines.chain([text.as_ref()]).filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok()) {
        for (field, kept) in [("provider", &mut provider), ("model", &mut model), ("usage", &mut usage)] {
            if !v[field].is_null() {
                *kept = v[field].clone();
            }
        }
    }
    let at = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis());
    let call = serde_json::json!({
        "at": at, "status": status, "ms": total.as_millis(), "first_ms": first.as_millis(), "model": model, "provider": provider,
        "prompt_tokens": usage["prompt_tokens"], "cached_tokens": usage["prompt_tokens_details"]["cached_tokens"],
        "completion_tokens": usage["completion_tokens"], "cost": usage["cost"],
    });
    serde_json::json!({ "call": call })
}

fn answer(out: &mut TcpStream, status: u16, kind: &str, body: &[u8]) -> Result<()> {
    write!(out, "HTTP/1.1 {status} {}\r\ncontent-type: {kind}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n", reason(status), body.len())?;
    out.write_all(body)?;
    Ok(())
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400..=499 => "Client Error",
        _ => "Server Error",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_call_is_logged_with_its_provider_and_cache() {
        let stream = concat!(
            "data: {\"id\":\"a\",\"provider\":\"Z.AI\",\"model\":\"z-ai/glm-5.3-flashx\",\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n",
            "data: {\"id\":\"a\",\"provider\":\"Z.AI\",\"choices\":[],\"usage\":{\"prompt_tokens\":11800,\"prompt_tokens_details\":{\"cached_tokens\":11700},\"completion_tokens\":30,\"cost\":0.001}}\n\n",
            "data: [DONE]\n\n",
        );
        let line = call_log(200, Duration::from_millis(400), Duration::from_millis(1400), stream.as_bytes());
        let c = &line["call"];
        assert_eq!((c["ms"].as_u64(), c["first_ms"].as_u64(), c["status"].as_u64()), (Some(1400), Some(400), Some(200)));
        assert_eq!((c["provider"].as_str(), c["model"].as_str()), (Some("Z.AI"), Some("z-ai/glm-5.3-flashx")));
        assert_eq!((c["prompt_tokens"].as_u64(), c["cached_tokens"].as_u64(), c["completion_tokens"].as_u64()), (Some(11800), Some(11700), Some(30)));
        // a whole answer (not streamed), and one with nothing to read
        let whole = br#"{"provider":"Z.AI","usage":{"prompt_tokens":5,"completion_tokens":1}}"#;
        assert_eq!(call_log(200, Duration::ZERO, Duration::ZERO, whole)["call"]["prompt_tokens"].as_u64(), Some(5));
        assert!(call_log(502, Duration::ZERO, Duration::ZERO, b"oops")["call"]["provider"].is_null());
    }
}
