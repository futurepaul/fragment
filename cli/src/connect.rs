//! `fragment connect chatgpt` (docs/optchat.md, "Your own models"): Sign
//! in with ChatGPT, done here because its redirect is a loopback on this
//! machine (developers.openai.com/siwc/token-sharing-open-source/sign-in,
//! and its self-hosted VMs page: sign in locally, hand the credentials to
//! the host that keeps them, and let it refresh them).
//!
//! It asks the platform for its host id (and the client id a sign-in
//! before registered), reads OpenAI's discovery, listens on
//! 127.0.0.1:1455 (else a free port) at `/auth/callback`, opens the
//! authorization in the browser (PKCE S256, a fresh state and nonce, the
//! plan's scopes and resource, `dynamic_agent_client` the first time),
//! takes the code back once, exchanges it, checks the grant (the plan's
//! scope; the ID token's issuer, audience, nonce and expiry: it came
//! straight from the token endpoint over TLS, so its signature is not
//! needed, OpenID Connect Core 3.1.3.7), and hands the tokens to the
//! platform (`PUT /api/connections/chatgpt/tokens`), which keeps them
//! sealed and refreshes them. Nothing is written on this machine.
//! `FRAGMENT_CHATGPT_ISSUER` names another issuer (dev's and the e2e's
//! fake). `--forget` signs out: the platform revokes the refresh token and
//! forgets the tokens.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use base64::Engine;
use fragment_core::providers::{self as own, ChatgptTokens};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::api::Client;

/// The loopback port the sign-in's examples use; another free one when it is taken.
const PORT: u16 = 1455;
const CALLBACK: &str = "/auth/callback";
/// How long the browser has to come back.
const WAIT: Duration = Duration::from_secs(10 * 60);
/// A callback's request, read at most.
const REQUEST_MAX_BYTES: usize = 16 * 1024;
/// Where it says so, for people (OpenAI's terms, 2026-10-08).
pub const TERMS: &str = "OpenAI offers ChatGPT plan usage to open-source projects, personal projects that run locally, and selected private apps; a paid or remotely hosted app joins its waitlist before offering it. Until fragment is approved, this is for your own testing.";

fn b64url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn random() -> String {
    b64url(&rand::random::<[u8; 32]>())
}

/// The ID token's claims, read (not verified: see the module's doc).
fn claims(id_token: &str) -> Result<Value> {
    let payload = id_token.split('.').nth(1).context("the ID token is no JWT")?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).context("the ID token's claims are no base64url")?;
    serde_json::from_slice(&bytes).context("the ID token's claims are no JSON")
}

/// Why the ID token is not this sign-in's, if it is not.
fn check_identity(c: &Value, issuer: &str, client_id: &str, nonce: &str, now_s: i64) -> Result<(), String> {
    if c["iss"] != issuer {
        return Err(format!("its issuer is {}, not {issuer}", c["iss"]));
    }
    let aud = match &c["aud"] {
        Value::String(a) => a == client_id,
        Value::Array(a) => a.iter().any(|x| x == client_id),
        _ => false,
    };
    if !aud {
        return Err("its audience is not this client".into());
    }
    if c["nonce"] != nonce {
        return Err("its nonce is not this sign-in's".into());
    }
    if c["exp"].as_i64().is_none_or(|exp| exp + 300 < now_s) {
        return Err("it has expired".into());
    }
    Ok(())
}

/// A query string's pairs, decoded.
fn query_of(target: &str) -> (String, Vec<(String, String)>) {
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let pairs = url::form_urlencoded::parse(query.as_bytes()).map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
    (path.to_string(), pairs)
}

fn page(stream: &mut TcpStream, status: &str, text: &str) {
    let body = format!("<!doctype html><meta charset=utf-8><title>fragment</title><body style=\"font:16px system-ui;max-width:32rem;margin:18vh auto\"><p>{text}</p></body>");
    let _ = write!(stream, "HTTP/1.1 {status}\r\ncontent-type: text/html; charset=utf-8\r\ncache-control: no-store\r\nreferrer-policy: no-referrer\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len());
}

/// The browser's return: `(code, client_id)` once a request at the callback
/// carries this sign-in's state; other requests are answered and passed.
fn callback(listener: &TcpListener, state: &str, saved: Option<&str>) -> Result<(String, String)> {
    listener.set_nonblocking(true)?;
    let deadline = Instant::now() + WAIT;
    // bounded by WAIT
    while Instant::now() < deadline {
        let mut stream = match listener.accept() {
            Ok((s, _)) => s,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
            Err(e) => return Err(e.into()),
        };
        stream.set_nonblocking(false)?;
        stream.set_read_timeout(Some(Duration::from_secs(10)))?;
        let mut buf = vec![0u8; REQUEST_MAX_BYTES];
        let mut got = 0;
        // bounded by REQUEST_MAX_BYTES
        while got < buf.len() && !buf[..got].windows(4).any(|w| w == b"\r\n\r\n") {
            match stream.read(&mut buf[got..]) {
                Ok(0) | Err(_) => break,
                Ok(n) => got += n,
            }
        }
        let head = String::from_utf8_lossy(&buf[..got]).to_string();
        let target = head.lines().next().and_then(|l| l.strip_prefix("GET ")).and_then(|l| l.split(' ').next()).unwrap_or("");
        let (path, pairs) = query_of(target);
        if path != CALLBACK {
            page(&mut stream, "404 Not Found", "Not found.");
            continue;
        }
        let one = |k: &str| {
            let all: Vec<&String> = pairs.iter().filter(|(n, _)| n == k).map(|(_, v)| v).collect();
            (all.len() == 1).then(|| all[0].clone())
        };
        if one("state").as_deref() != Some(state) {
            page(&mut stream, "400 Bad Request", "This is not the sign-in fragment started: go back to the tab that started it.");
            continue;
        }
        if let Some(err) = one("error") {
            page(&mut stream, "200 OK", "ChatGPT did not connect. You can close this tab.");
            bail!("ChatGPT answered {err}: {}", if err == "access_denied" { "you declined; run `fragment connect chatgpt` again to allow it" } else { "try again" });
        }
        let code = one("code").context("the callback carries no code")?;
        let client = match (one("client_id"), saved) {
            (Some(c), Some(s)) if c != s => bail!("ChatGPT answered another client ({c}) than the one registered ({s})"),
            (Some(c), _) => c,
            (None, Some(s)) => s.to_string(),
            (None, None) => bail!("ChatGPT did not finish registering fragment (no client_id): run it again"),
        };
        if !own::valid_client_id(&client) {
            bail!("ChatGPT answered a client id out of shape: {client:?}");
        }
        page(&mut stream, "200 OK", "ChatGPT is connected to fragment. You can close this tab.");
        return Ok((code, client));
    }
    bail!("the browser did not come back within {} minutes: run it again", WAIT.as_secs() / 60)
}

/// Opens `url` in the person's browser: `$BROWSER`, else the system's.
fn open_browser(url: &str) {
    let opener = std::env::var("BROWSER").ok().filter(|b| !b.trim().is_empty()).unwrap_or_else(|| if cfg!(target_os = "macos") { "open".into() } else { "xdg-open".into() });
    let _ = std::process::Command::new(opener).arg(url).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn();
}

/// `fragment connect chatgpt` (the module's doc): answers the platform's
/// picker after it keeps the tokens.
pub fn chatgpt(c: &Client, no_browser: bool, port: Option<u16>) -> Result<Value> {
    let picker: Value = c.call_as(c.get("/api/models")?)?;
    let row = picker["providers"].as_array().and_then(|l| l.iter().find(|p| p["provider"] == "chatgpt")).cloned().context("this platform does not offer ChatGPT (its catalog has no openai row)")?;
    let host_id = row["hostId"].as_str().context("the platform named no host id: make your computer first (fragment's shell does at your first visit)")?.to_string();
    let saved = row["clientId"].as_str().filter(|id| own::valid_client_id(id)).map(str::to_string);
    let account = row["account"].as_str().map(str::to_string);
    let issuer = std::env::var("FRAGMENT_CHATGPT_ISSUER").ok().map(|s| s.trim_end_matches('/').to_string()).filter(|s| !s.is_empty()).unwrap_or_else(|| own::CHATGPT_ISSUER.to_string());
    let http = reqwest::blocking::Client::builder().timeout(Duration::from_secs(30)).redirect(reqwest::redirect::Policy::none()).build()?;
    let discovery: Value = serde_json::from_slice(&http.get(format!("{issuer}/.well-known/openid-configuration")).send()?.error_for_status()?.bytes()?)?;
    let endpoint = |k: &str| -> Result<String> {
        let e = discovery[k].as_str().with_context(|| format!("OpenAI's discovery names no {k}"))?;
        if !e.starts_with(&format!("{issuer}/")) || discovery["issuer"] != issuer.as_str() {
            bail!("OpenAI's discovery is not {issuer}'s");
        }
        Ok(e.to_string())
    };
    let (authorize, token) = (endpoint("authorization_endpoint")?, endpoint("token_endpoint")?);
    let listener = match TcpListener::bind(("127.0.0.1", port.unwrap_or(PORT))) {
        Ok(l) => l,
        Err(_) if port.is_none() => TcpListener::bind(("127.0.0.1", 0))?,
        Err(e) => bail!("127.0.0.1:{} is taken: {e}", port.unwrap_or(PORT)),
    };
    let redirect = format!("http://127.0.0.1:{}{CALLBACK}", listener.local_addr()?.port());
    let (state, nonce, verifier) = (random(), random(), random());
    let mut url = reqwest::Url::parse(&authorize)?;
    {
        let mut q = url.query_pairs_mut();
        q.append_pair("client_id", saved.as_deref().unwrap_or(own::DYNAMIC_CLIENT))
            .append_pair("response_type", "code")
            .append_pair("redirect_uri", &redirect)
            .append_pair("scope", own::CHATGPT_SCOPES)
            .append_pair("resource", own::CHATGPT_RESOURCE)
            .append_pair("state", &state)
            .append_pair("nonce", &nonce)
            .append_pair("code_challenge_method", "S256")
            .append_pair("code_challenge", &b64url(&Sha256::digest(verifier.as_bytes())))
            .append_pair("ext_agent_host_id", &host_id);
        if saved.is_none() {
            q.append_pair("agent_name_hint", own::CHATGPT_APP_NAME);
        }
        if let Some(email) = &account {
            q.append_pair("login_hint", email);
        }
    }
    eprintln!("{TERMS}\n");
    eprintln!("Sign in with ChatGPT in your browser{}:\n  {url}\n", if no_browser { "" } else { " (if it did not open, open this)" });
    if !no_browser {
        open_browser(url.as_str());
    }
    let (code, client_id) = callback(&listener, &state, saved.as_deref())?;
    let form = [("grant_type", "authorization_code"), ("client_id", client_id.as_str()), ("code", code.as_str()), ("code_verifier", verifier.as_str()), ("redirect_uri", redirect.as_str()), ("resource", own::CHATGPT_RESOURCE)];
    let resp = http.post(&token).form(&form).send()?;
    let status = resp.status();
    let v: Value = serde_json::from_slice(&resp.bytes()?).unwrap_or(Value::Null);
    if !status.is_success() {
        bail!("ChatGPT refused the code ({status}): {}", v["error"].as_str().or(v["error_description"].as_str()).unwrap_or("no reason given"));
    }
    if !v["token_type"].as_str().is_some_and(|t| t.eq_ignore_ascii_case("bearer")) {
        bail!("ChatGPT answered no bearer token");
    }
    let id_token = v["id_token"].as_str().context("ChatGPT answered no ID token")?;
    let c_claims = claims(id_token)?;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs() as i64;
    check_identity(&c_claims, &issuer, &client_id, &nonce, now).map_err(|why| anyhow::anyhow!("ChatGPT's ID token is not this sign-in's: {why}"))?;
    let tokens = ChatgptTokens {
        client_id: client_id.clone(),
        access_token: v["access_token"].as_str().unwrap_or_default().to_string(),
        refresh_token: v["refresh_token"].as_str().unwrap_or_default().to_string(),
        expires_in: v["expires_in"].as_i64().unwrap_or(0),
        scope: v["scope"].as_str().unwrap_or_default().to_string(),
        email: c_claims["email"].as_str().map(str::to_string),
    };
    own::tokens_check(&tokens).map_err(|why| anyhow::anyhow!("{why}"))?;
    let kept: Value = c.call_as(c.put_json("/api/connections/chatgpt/tokens", &tokens)?)?;
    Ok(json!({ "provider": "chatgpt", "state": kept["chatgpt"]["state"], "account": kept["chatgpt"]["account"], "clientId": client_id }))
}

/// `fragment connect chatgpt --forget`: signed out; whether OpenAI
/// confirmed the refresh token's revocation.
pub fn forget(c: &Client) -> Result<Value> {
    let v: Value = c.call_as(c.delete("/api/connections/chatgpt/tokens")?)?;
    Ok(json!({ "provider": "chatgpt", "state": v["chatgpt"]["state"], "revoked": v["revoked"] }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Goal: an ID token is this sign-in's only with its issuer, audience,
    /// nonce and a live expiry. Method: good claims, and each one wrong.
    #[test]
    fn the_id_token_is_this_sign_ins() {
        let good = json!({ "iss": "https://auth.openai.com", "aud": "oaiapp_1", "nonce": "n", "exp": 1000 });
        assert!(check_identity(&good, "https://auth.openai.com", "oaiapp_1", "n", 900).is_ok());
        let listed = json!({ "iss": "https://auth.openai.com", "aud": ["x", "oaiapp_1"], "nonce": "n", "exp": 1000 });
        assert!(check_identity(&listed, "https://auth.openai.com", "oaiapp_1", "n", 900).is_ok(), "an audience among several");
        for bad in [json!({ "iss": "https://evil" }), json!({ "aud": "other" }), json!({ "nonce": "m" }), json!({ "exp": 100 })] {
            let mut c = good.clone();
            c.as_object_mut().unwrap().extend(bad.as_object().unwrap().clone());
            assert!(check_identity(&c, "https://auth.openai.com", "oaiapp_1", "n", 900).is_err(), "{c}");
        }
        let jwt = format!("h.{}.s", b64url(good.to_string().as_bytes()));
        assert_eq!(claims(&jwt).unwrap(), good);
        assert!(claims("no-dots").is_err());
        let (path, pairs) = query_of("/auth/callback?code=a%2Bb&state=s&state=t");
        assert_eq!((path.as_str(), pairs.len(), pairs[0].1.as_str()), ("/auth/callback", 3, "a+b"));
    }
}
