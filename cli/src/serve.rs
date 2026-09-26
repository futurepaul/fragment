//! `fragment model --serve`: a local OpenAI-compatible endpoint on
//! 127.0.0.1 (`POST /v1/chat/completions`, streaming or not) that forwards
//! each request to the platform's model endpoint signed with this CLI's
//! key, and relays the answer as it arrives. Anything that speaks to an
//! OpenAI-compatible provider (goose on a computer) then needs no key: the
//! platform picks the model and bills the computer's owner. It binds the
//! loopback address only, so it answers this machine alone.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use fragment_proto::limits::BODY_MAX_BYTES;

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

pub fn serve(client: Client, port: u16) -> Result<()> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port)).with_context(|| format!("binding 127.0.0.1:{port}"))?;
    let at = listener.local_addr()?;
    assert!(at.ip().is_loopback(), "the model endpoint answers this machine alone");
    println!("serving http://{at}/v1 (POST /v1/chat/completions), signed as {}", client.id.npub());
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
        (Some("POST"), Some("/v1/chat/completions")) => {}
        _ => return answer(&mut out, 404, "application/json", br#"{"error":{"message":"POST /v1/chat/completions is all this serves"}}"#),
    }
    let Some(length) = length.filter(|n| *n <= BODY_MAX_BYTES) else {
        return answer(&mut out, 413, "application/json", br#"{"error":{"message":"a request carries its content-length, at most 2 MiB"}}"#);
    };
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    let mut forwarded = match client.post_streaming(PLATFORM_PATH, body, CALL_TIMEOUT) {
        Ok(r) => r,
        Err(e) => return answer(&mut out, 502, "application/json", serde_json::json!({ "error": { "message": format!("{e:#}") } }).to_string().as_bytes()),
    };
    // the platform's answer, as it arrives: its status, its type, and its
    // bytes (a stream's chunks), ended by closing the connection
    let status = forwarded.status().as_u16();
    let kind = forwarded.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("application/json").to_string();
    write!(out, "HTTP/1.1 {status} {}\r\ncontent-type: {kind}\r\ncache-control: no-store\r\nconnection: close\r\n\r\n", reason(status))?;
    let mut piece = [0u8; 16 * 1024];
    loop {
        let n = forwarded.read(&mut piece)?;
        if n == 0 {
            return Ok(());
        }
        out.write_all(&piece[..n])?;
        out.flush()?;
    }
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
