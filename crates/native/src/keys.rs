//! `native:keys`: the fleet's keys, used on a cell's behalf and never handed
//! to it (docs/hardening.md, H1). They come from the node's environment
//! (Fly secrets on the fleet): no Worker `vars`, no JS or wasm heap, no
//! deployment manifest in the bucket.
//!
//! Authority comes from the caller's scope, which the host attests. Only a
//! cell may call (`GET /whoami` excepted). Every route is `POST` with a JSON
//! body; byte fields are base64.
//!
//! - `seal {plaintext}` → `{sealed}`: sealed for the calling cell only.
//! - `open {sealed, legacySalt?}` → `{plaintext, resealed?}`: opens only
//!   what was sealed for the caller. `resealed` is the same value sealed
//!   again under the current host secret, when it was sealed under a
//!   previous one (or before `KEYS`): the caller stores it instead.
//! - `nostr/keypair` → `{pubkey, sealed}`: a secp256k1 key made here; its
//!   secret leaves only sealed for the caller.
//! - `nostr/sign {sealed, legacySalt?, kind, method, url, payload?, signer?,
//!   createdAt}` → `{header, pubkey, resealed?}`: a NIP-98 `Authorization`
//!   value (`kind: "header"`, `payload` the body's SHA-256 hex) or a key
//!   proof (`kind: "proof"`, `signer` the hex key it joins).
//! - `codestorage/token {repo, sub, scopes, ttlS}` → `{token, expiresAt}`:
//!   a code.storage JWT, for `Fragment` cells only.
//! - `workos/authenticate {clientId, code}` → `{status, body}`: WorkOS's
//!   code exchange with the API key added, for the `Registry` only; the
//!   answer's refresh token is dropped.
//! - `openrouter/keys {method, hash?, body?}` → `{status, body}`:
//!   OpenRouter's key API with the management key, for `Ledger` cells only.
//! - `sprites {op, cmd?, stdin?}` → `{status, body, sprite}` (`body` the
//!   answer's text): the Sprites API with the fleet's token, for `Computer`
//!   cells only, on one Sprite each: `fragment-` and the first 24 hex of
//!   the caller's id (`sprite_of`), so no cell reaches another's Sprite or
//!   anything else in the org. `op` is `create`, `get`, `delete`, or
//!   `exec` (`cmd` its argv, `stdin` its input).
//!
//! The outbound calls go straight from the node to the operator-configured
//! hosts (`FRAGMENT_KEYS_*_URL`), not through a Worker's egress.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use base64::Engine;
use fragment_core::codestorage::{Claims, OrgKey};
use fragment_nip98::Keys as NostrKeys;
use serde_json::{json, Value};

use crate::seal::{self, SealError};
use crate::{Reply, Request, Response, Service};

/// The longest code.storage token signed, as for an editor's storage token.
const JWT_TTL_MAX_S: i64 = 900;
/// The largest value sealed (a fragment secret is at most 64 KiB).
const PLAINTEXT_MAX_BYTES: usize = 64 * 1024;
const OUTBOUND_TIMEOUT: Duration = Duration::from_secs(30);
const CODESTORAGE_SCOPES: [&str; 4] = ["git:read", "git:write", "repo:write", "org:read"];

/// The cell classes each fleet credential serves.
const CODESTORAGE_CLASS: &str = "Fragment";
const WORKOS_CLASS: &str = "Registry";
const OPENROUTER_CLASS: &str = "Ledger";
const SPRITES_CLASS: &str = "Computer";
/// An exec runs until its command ends (a computer's first boot installs
/// the CLI and pairs), and its answer is kept to this much text.
const SPRITES_TIMEOUT: Duration = Duration::from_secs(180);
const SPRITES_ANSWER_MAX_BYTES: usize = 64 * 1024;
const SPRITES_ARGV_MAX: usize = 32;

/// A credential for one operator-configured host.
pub struct Outbound {
    pub key: String,
    /// Its base URL, without a trailing slash.
    pub base: String,
}

pub struct Config {
    /// The current host secret, then previous ones during a rotation.
    pub host_secrets: Vec<String>,
    /// The code.storage org and its signing key, or why they are unusable.
    pub codestorage: Option<Result<(String, OrgKey), String>>,
    pub workos: Option<Outbound>,
    pub openrouter: Option<Outbound>,
    pub sprites: Option<Outbound>,
}

pub struct Keys {
    cfg: Arc<Config>,
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

fn b64() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

fn now_s() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("clock after 1970").as_secs() as i64
}

fn random<const N: usize>() -> [u8; N] {
    let mut out = [0u8; N];
    getrandom::fill(&mut out).expect("the OS's randomness");
    out
}

fn class_of(caller: &str) -> &str {
    caller.split(':').next().unwrap_or("")
}

fn only(caller: &str, class: &str, what: &str) -> Result<(), Response> {
    if class_of(caller) == class {
        Ok(())
    } else {
        Err(Response::error(403, format!("{what} is for {class} cells only")))
    }
}

fn outbound_from_env(key: &str, url: &str, default: &str) -> Option<Outbound> {
    env(key).map(|key| Outbound { key, base: env(url).unwrap_or_else(|| default.to_string()).trim_end_matches('/').to_string() })
}

impl Keys {
    /// The node's keys: `FRAGMENT_KEYS_HOST_SECRET` (and `_PREVIOUS` during
    /// a rotation), `FRAGMENT_KEYS_CODESTORAGE_ORG` and `_PRIVATE_KEY`,
    /// `FRAGMENT_KEYS_WORKOS_API_KEY` (`_URL`), and
    /// `FRAGMENT_KEYS_OPENROUTER_MANAGEMENT_KEY` (`FRAGMENT_KEYS_OPENROUTER_URL`),
    /// and `FRAGMENT_KEYS_SPRITES_TOKEN` (`FRAGMENT_KEYS_SPRITES_URL`).
    pub fn from_env() -> Keys {
        let host_secrets = ["FRAGMENT_KEYS_HOST_SECRET", "FRAGMENT_KEYS_HOST_SECRET_PREVIOUS"].iter().filter_map(|n| env(n)).collect();
        let codestorage = match (env("FRAGMENT_KEYS_CODESTORAGE_ORG"), env("FRAGMENT_KEYS_CODESTORAGE_PRIVATE_KEY")) {
            (None, None) => None,
            (Some(org), Some(pem)) => Some(OrgKey::from_pem(&pem).map(|k| (org, k))),
            _ => Some(Err("set both FRAGMENT_KEYS_CODESTORAGE_ORG and FRAGMENT_KEYS_CODESTORAGE_PRIVATE_KEY".into())),
        };
        Keys::with(Config {
            host_secrets,
            codestorage,
            workos: outbound_from_env("FRAGMENT_KEYS_WORKOS_API_KEY", "FRAGMENT_KEYS_WORKOS_URL", "https://api.workos.com"),
            openrouter: outbound_from_env("FRAGMENT_KEYS_OPENROUTER_MANAGEMENT_KEY", "FRAGMENT_KEYS_OPENROUTER_URL", "https://openrouter.ai"),
            sprites: outbound_from_env("FRAGMENT_KEYS_SPRITES_TOKEN", "FRAGMENT_KEYS_SPRITES_URL", "https://api.sprites.dev"),
        })
    }

    pub fn with(cfg: Config) -> Keys {
        Keys { cfg: Arc::new(cfg) }
    }
}

impl Service for Keys {
    fn fetch(&self, req: Request) -> Reply {
        let cfg = self.cfg.clone();
        Box::pin(async move { cfg.handle(req).await })
    }
}

fn str_field<'a>(body: &'a Value, name: &str) -> Result<&'a str, Response> {
    body[name].as_str().filter(|s| !s.is_empty()).ok_or_else(|| Response::error(400, format!("{name} is required (a string)")))
}

impl Config {
    async fn handle(&self, req: Request) -> Response {
        let path = req.path().to_string();
        if (req.method.as_str(), path.as_str()) == ("GET", "whoami") {
            return Response::json(200, &json!({ "caller": req.caller }));
        }
        let Some(caller) = req.caller.as_deref() else {
            return Response::error(403, "only a cell may use KEYS (this call came from no cell)");
        };
        if req.method != "POST" {
            return Response::error(405, format!("{} /{path}: KEYS routes are POST", req.method));
        }
        let body: Value = match serde_json::from_slice(&req.body) {
            Ok(v) => v,
            Err(e) => return Response::error(400, format!("the body is not JSON: {e}")),
        };
        let answer = match path.as_str() {
            "seal" => self.seal(caller, &body),
            "open" => self.open(caller, &body),
            "nostr/keypair" => self.nostr_keypair(caller),
            "nostr/sign" => self.nostr_sign(caller, &body),
            "codestorage/token" => self.codestorage_token(caller, &body),
            "workos/authenticate" => self.workos(caller, &body).await,
            "openrouter/keys" => self.openrouter_keys(caller, &body).await,
            "sprites" => self.sprites(caller, &body).await,
            p => Err(Response::error(404, format!("no route POST /{p}"))),
        };
        answer.unwrap_or_else(|r| r)
    }

    fn hosts(&self) -> Result<Vec<&str>, Response> {
        match self.host_secrets.first() {
            Some(s) if s.len() >= seal::HOST_SECRET_MIN_BYTES => Ok(self.host_secrets.iter().map(String::as_str).collect()),
            Some(_) => Err(Response::error(503, SealError::WeakHostSecret.to_string())),
            None => Err(Response::error(503, "FRAGMENT_KEYS_HOST_SECRET is not set on this node")),
        }
    }

    fn seal_for(&self, caller: &str, plaintext: &[u8]) -> Result<String, Response> {
        let hosts = self.hosts()?;
        seal::seal(hosts[0], caller, plaintext, random()).map_err(|e| Response::error(503, e.to_string()))
    }

    /// Opens a value sealed for `caller`: (plaintext, the value resealed
    /// under the current host secret when it was stale).
    fn open_for(&self, caller: &str, body: &Value) -> Result<(Vec<u8>, Option<String>), Response> {
        let sealed = str_field(body, "sealed")?;
        let legacy = body["legacySalt"].as_str();
        let hosts = self.hosts()?;
        let opened = seal::open(&hosts, caller, sealed, legacy).map_err(|e| match e {
            SealError::Malformed => Response::error(400, e.to_string()),
            SealError::WeakHostSecret => Response::error(503, e.to_string()),
            SealError::NeedsLegacySalt | SealError::UnknownKey(_) | SealError::Corrupt => Response::error(403, e.to_string()),
        })?;
        let resealed = if opened.stale { Some(self.seal_for(caller, &opened.plaintext)?) } else { None };
        Ok((opened.plaintext, resealed))
    }

    fn seal(&self, caller: &str, body: &Value) -> Result<Response, Response> {
        let plaintext = body["plaintext"].as_str().and_then(|p| b64().decode(p).ok()).ok_or_else(|| Response::error(400, "plaintext must be base64"))?;
        if plaintext.len() > PLAINTEXT_MAX_BYTES {
            return Err(Response::error(400, format!("a sealed value is at most {PLAINTEXT_MAX_BYTES} bytes")));
        }
        Ok(Response::json(200, &json!({ "sealed": self.seal_for(caller, &plaintext)? })))
    }

    fn open(&self, caller: &str, body: &Value) -> Result<Response, Response> {
        let (plaintext, resealed) = self.open_for(caller, body)?;
        Ok(Response::json(200, &json!({ "plaintext": b64().encode(plaintext), "resealed": resealed })))
    }

    fn nostr_keypair(&self, caller: &str) -> Result<Response, Response> {
        let keys = loop {
            if let Some(k) = NostrKeys::from_secret_hex(&hex::encode(random::<32>())) {
                break k;
            }
        };
        let sealed = self.seal_for(caller, keys.secret_hex().as_bytes())?;
        Ok(Response::json(200, &json!({ "pubkey": keys.pubkey_hex(), "sealed": sealed })))
    }

    fn nostr_sign(&self, caller: &str, body: &Value) -> Result<Response, Response> {
        let (secret, resealed) = self.open_for(caller, body)?;
        let keys = std::str::from_utf8(&secret).ok().and_then(NostrKeys::from_secret_hex).ok_or_else(|| Response::error(400, "that sealed value is not a nostr key"))?;
        let method = str_field(body, "method")?;
        let url = str_field(body, "url")?;
        let created_at = body["createdAt"].as_i64().ok_or_else(|| Response::error(400, "createdAt is required (seconds)"))?;
        let is_sha = |s: &str| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit());
        let header = match body["kind"].as_str() {
            Some("header") => {
                let payload = body["payload"].as_str();
                if payload.is_some_and(|p| !is_sha(p)) {
                    return Err(Response::error(400, "payload is the body's SHA-256 as 64 hex"));
                }
                keys.header_for_payload(method, url, payload, created_at)
            }
            Some("proof") => {
                let signer = str_field(body, "signer")?;
                if !is_sha(signer) {
                    return Err(Response::error(400, "signer is a key as 64 hex"));
                }
                keys.proof(method, url, signer, created_at)
            }
            _ => return Err(Response::error(400, "kind is \"header\" or \"proof\"")),
        };
        Ok(Response::json(200, &json!({ "header": header, "pubkey": keys.pubkey_hex(), "resealed": resealed })))
    }

    fn codestorage_token(&self, caller: &str, body: &Value) -> Result<Response, Response> {
        only(caller, CODESTORAGE_CLASS, "a code.storage token")?;
        let (org, key) = match &self.codestorage {
            None => return Err(Response::error(503, "FRAGMENT_KEYS_CODESTORAGE_ORG and _PRIVATE_KEY are not set on this node")),
            Some(Err(why)) => return Err(Response::error(503, why.clone())),
            Some(Ok(pair)) => pair,
        };
        let repo = str_field(body, "repo")?;
        let sub = str_field(body, "sub")?;
        let scopes: Vec<&str> = body["scopes"].as_array().map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
        if scopes.is_empty() || scopes.iter().any(|s| !CODESTORAGE_SCOPES.contains(s)) {
            return Err(Response::error(400, format!("scopes are some of {CODESTORAGE_SCOPES:?}")));
        }
        let ttl = body["ttlS"].as_i64().unwrap_or(JWT_TTL_MAX_S);
        if !(1..=JWT_TTL_MAX_S).contains(&ttl) {
            return Err(Response::error(400, format!("ttlS must be 1..={JWT_TTL_MAX_S}")));
        }
        let iat = now_s();
        let token = key.token(&Claims { iss: org, sub, repo, scopes: &scopes, iat, exp: iat + ttl });
        Ok(Response::json(200, &json!({ "token": token, "expiresAt": (iat + ttl) * 1000 })))
    }

    async fn workos(&self, caller: &str, body: &Value) -> Result<Response, Response> {
        only(caller, WORKOS_CLASS, "WorkOS's code exchange")?;
        let workos = self.workos.as_ref().ok_or_else(|| Response::error(503, "FRAGMENT_KEYS_WORKOS_API_KEY is not set on this node"))?;
        let payload = json!({
            "client_id": str_field(body, "clientId")?,
            "client_secret": workos.key,
            "grant_type": "authorization_code",
            "code": str_field(body, "code")?,
        });
        let req = client().post(format!("{}/user_management/authenticate", workos.base)).json_body(&payload);
        let (status, mut answer) = send(req, "WorkOS").await?;
        if let Some(o) = answer.as_object_mut() {
            // the platform keeps its own session; WorkOS's long-lived one stays unused
            o.remove("refresh_token");
        }
        Ok(Response::json(200, &json!({ "status": status, "body": answer })))
    }

    async fn openrouter_keys(&self, caller: &str, body: &Value) -> Result<Response, Response> {
        only(caller, OPENROUTER_CLASS, "OpenRouter's key API")?;
        let or = self.openrouter.as_ref().ok_or_else(|| Response::error(503, "FRAGMENT_KEYS_OPENROUTER_MANAGEMENT_KEY is not set on this node"))?;
        let path = match body["hash"].as_str() {
            None => "keys".to_string(),
            Some(h) if !h.is_empty() && h.len() <= 128 && h.bytes().all(|b| b.is_ascii_alphanumeric()) => format!("keys/{h}"),
            Some(_) => return Err(Response::error(400, "hash is a key's hash (letters and digits)")),
        };
        let url = format!("{}/api/v1/{path}", or.base);
        let req = match body["method"].as_str() {
            Some("GET") => client().get(url),
            Some("POST") => client().post(url).json_body(&body["body"]),
            Some("PATCH") => client().patch(url).json_body(&body["body"]),
            _ => return Err(Response::error(400, "method is GET, POST, or PATCH")),
        };
        let (status, answer) = send(req.bearer_auth(&or.key), "OpenRouter").await?;
        Ok(Response::json(200, &json!({ "status": status, "body": answer })))
    }
}

/// The one Sprite a `Computer` cell reaches: named for its id, which the
/// host attests.
pub fn sprite_of(caller: &str) -> String {
    let hex = caller.split_once(':').map_or("", |(_, h)| h);
    assert!(hex.len() >= 24, "a cell's scope ends in its 64-hex id");
    format!("fragment-{}", &hex[..24])
}

impl Config {
    async fn sprites(&self, caller: &str, body: &Value) -> Result<Response, Response> {
        only(caller, SPRITES_CLASS, "the Sprites API")?;
        let sp = self.sprites.as_ref().ok_or_else(|| Response::error(503, "FRAGMENT_KEYS_SPRITES_TOKEN is not set on this node"))?;
        let sprite = sprite_of(caller);
        let base = format!("{}/v1/sprites", sp.base);
        let req = match str_field(body, "op")? {
            "create" => sprites_client().post(base).json_body(&json!({ "name": sprite })),
            "get" => sprites_client().get(format!("{base}/{sprite}")),
            "delete" => sprites_client().delete(format!("{base}/{sprite}")),
            "exec" => {
                let argv: Vec<&str> = body["cmd"].as_array().map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
                if argv.is_empty() || argv.len() > SPRITES_ARGV_MAX {
                    return Err(Response::error(400, format!("cmd is 1..={SPRITES_ARGV_MAX} strings")));
                }
                let mut query: Vec<(&str, &str)> = argv.into_iter().map(|a| ("cmd", a)).collect();
                let stdin = body["stdin"].as_str().unwrap_or("");
                if !stdin.is_empty() {
                    query.push(("stdin", "true"));
                }
                sprites_client().post(format!("{base}/{sprite}/exec")).query(&query).body(stdin.to_string())
            }
            op => return Err(Response::error(400, format!("op {op:?} is create, get, delete, or exec"))),
        };
        let resp = req.bearer_auth(&sp.key).send().await.map_err(|e| Response::error(502, format!("Sprites did not answer: {}", e.without_url())))?;
        let status = resp.status().as_u16();
        let bytes = resp.bytes().await.map_err(|e| Response::error(502, format!("Sprites' answer: {}", e.without_url())))?;
        let text = String::from_utf8_lossy(&bytes[..bytes.len().min(SPRITES_ANSWER_MAX_BYTES)]).into_owned();
        Ok(Response::json(200, &json!({ "status": status, "body": text, "sprite": sprite })))
    }
}

fn sprites_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| reqwest::Client::builder().timeout(SPRITES_TIMEOUT).build().expect("a reqwest client builds"))
}

fn client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| reqwest::Client::builder().timeout(OUTBOUND_TIMEOUT).build().expect("a reqwest client builds"))
}

trait JsonBody {
    fn json_body(self, v: &Value) -> Self;
}

impl JsonBody for reqwest::RequestBuilder {
    fn json_body(self, v: &Value) -> Self {
        self.header("content-type", "application/json").body(v.to_string())
    }
}

/// (status, JSON body or `null`) of an outbound call; not reaching the
/// host is a 502 to the caller.
async fn send(req: reqwest::RequestBuilder, host: &str) -> Result<(u16, Value), Response> {
    let resp = req.send().await.map_err(|e| Response::error(502, format!("{host} did not answer: {}", e.without_url())))?;
    let status = resp.status().as_u16();
    let bytes = resp.bytes().await.map_err(|e| Response::error(502, format!("{host}'s answer: {}", e.without_url())))?;
    Ok((status, serde_json::from_slice(&bytes).unwrap_or(Value::Null)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::task::{Context, Poll, Waker};

    const HOST: &str = "0123456789abcdef0123456789abcdef-current";
    const OLD: &str = "0123456789abcdef0123456789abcdef-previous";
    const ALICE: &str = "Fragment:aaaa";
    const BOB: &str = "Fragment:bbbb";
    const LEDGER: &str = "Ledger:cccc";

    fn keys(hosts: &[&str]) -> Keys {
        let pem = p256::ecdsa::SigningKey::from_slice(&[7u8; 32]).unwrap();
        let pem = p256::pkcs8::EncodePrivateKey::to_pkcs8_pem(&pem, Default::default()).unwrap().to_string();
        Keys::with(Config {
            host_secrets: hosts.iter().map(|s| s.to_string()).collect(),
            codestorage: Some(OrgKey::from_pem(&pem).map(|k| ("org".to_string(), k))),
            workos: None,
            openrouter: None,
            sprites: None,
        })
    }

    /// A route that makes no outbound call answers without waiting.
    fn call(k: &Keys, caller: Option<&str>, method: &str, path: &str, body: Value) -> (u16, Value) {
        let req = Request {
            caller: caller.map(str::to_string),
            method: method.into(),
            url: format!("https://keys/{path}"),
            headers: vec![],
            body: body.to_string().into_bytes(),
        };
        let mut fut = k.fetch(req);
        match fut.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
            Poll::Ready(r) => (r.status, serde_json::from_slice(&r.body).unwrap()),
            Poll::Pending => panic!("{path} waited on something"),
        }
    }

    #[test]
    fn only_cells_call() {
        let k = keys(&[HOST]);
        assert_eq!(call(&k, None, "GET", "whoami", json!({})), (200, json!({ "caller": null })));
        assert_eq!(call(&k, Some(ALICE), "GET", "whoami", json!({})).1["caller"], ALICE);
        for path in ["seal", "open", "nostr/keypair", "codestorage/token", "workos/authenticate"] {
            assert_eq!(call(&k, None, "POST", path, json!({})).0, 403, "{path} from no cell");
        }
        assert_eq!(call(&k, Some(ALICE), "POST", "nope", json!({})).0, 404);
    }

    #[test]
    fn a_value_opens_only_for_its_cell() {
        let k = keys(&[HOST]);
        let (s, v) = call(&k, Some(ALICE), "POST", "seal", json!({ "plaintext": b64().encode("sk-live") }));
        assert_eq!(s, 200);
        let sealed = v["sealed"].as_str().unwrap();
        let (s, v) = call(&k, Some(ALICE), "POST", "open", json!({ "sealed": sealed }));
        assert_eq!((s, v["plaintext"].as_str()), (200, Some(b64().encode("sk-live").as_str())));
        assert_eq!(v["resealed"], Value::Null);
        let (s, v) = call(&k, Some(BOB), "POST", "open", json!({ "sealed": sealed, "legacySalt": ALICE }));
        assert_eq!(s, 403, "{v}");
    }

    #[test]
    fn a_stale_value_comes_back_resealed() {
        let old = keys(&[OLD]);
        let sealed = call(&old, Some(ALICE), "POST", "seal", json!({ "plaintext": b64().encode("v") })).1["sealed"].as_str().unwrap().to_string();
        let k = keys(&[HOST, OLD]);
        let (s, v) = call(&k, Some(ALICE), "POST", "open", json!({ "sealed": sealed }));
        assert_eq!(s, 200);
        let fresh = v["resealed"].as_str().unwrap();
        assert!(fresh.starts_with(&format!("w2.{}.", seal::key_id(HOST))));
        let current = keys(&[HOST]);
        assert_eq!(call(&current, Some(ALICE), "POST", "open", json!({ "sealed": fresh })).1["plaintext"], b64().encode("v"));
        assert_eq!(call(&current, Some(ALICE), "POST", "open", json!({ "sealed": sealed })).0, 403, "the old secret is gone");
        assert_eq!(call(&keys(&[]), Some(ALICE), "POST", "seal", json!({ "plaintext": "" })).0, 503, "no host secret on the node");
    }

    #[test]
    fn nostr_keys_sign_and_never_leave() {
        let k = keys(&[HOST]);
        let (s, pair) = call(&k, Some(ALICE), "POST", "nostr/keypair", json!({}));
        assert_eq!(s, 200);
        let (pubkey, sealed) = (pair["pubkey"].as_str().unwrap(), pair["sealed"].as_str().unwrap());
        assert_eq!(pubkey.len(), 64);
        let url = "https://fragment.club/api/fragments";
        let body = br#"{"name":"x"}"#;
        let payload = hex::encode(<sha2::Sha256 as sha2::Digest>::digest(body));
        let now = now_s();
        let (s, v) = call(&k, Some(ALICE), "POST", "nostr/sign", json!({ "sealed": sealed, "kind": "header", "method": "post", "url": url, "payload": payload, "createdAt": now }));
        assert_eq!(s, 200, "{v}");
        assert_eq!(v["pubkey"], pubkey);
        let header = v["header"].as_str().unwrap();
        assert_eq!(fragment_nip98::verify(Some(header), "POST", url, body, now, 60).unwrap(), pubkey);
        assert!(fragment_nip98::verify(Some(header), "POST", url, b"other", now, 60).is_err());
        let signer = "ab".repeat(32);
        let (s, v) = call(&k, Some(ALICE), "POST", "nostr/sign", json!({ "sealed": sealed, "kind": "proof", "method": "POST", "url": url, "signer": signer, "createdAt": now }));
        assert_eq!(s, 200);
        assert_eq!(fragment_nip98::verify_proof(v["header"].as_str().unwrap(), "POST", url, &signer, now, 60).unwrap(), pubkey);
        assert_eq!(call(&k, Some(BOB), "POST", "nostr/sign", json!({ "sealed": sealed, "kind": "header", "method": "GET", "url": url, "createdAt": now })).0, 403);
        assert_eq!(call(&k, Some(ALICE), "POST", "nostr/sign", json!({ "sealed": sealed, "kind": "header", "method": "GET", "url": url, "payload": "zz", "createdAt": now })).0, 400);
    }

    #[test]
    fn codestorage_tokens_for_fragments_only() {
        let k = keys(&[HOST]);
        let ask = json!({ "repo": "r-1", "sub": "fragment-runtime", "scopes": ["git:read"], "ttlS": 300 });
        let (s, v) = call(&k, Some(ALICE), "POST", "codestorage/token", ask.clone());
        assert_eq!(s, 200, "{v}");
        let claims = v["token"].as_str().unwrap().split('.').nth(1).unwrap().to_string();
        let claims: Value = serde_json::from_slice(&base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(claims).unwrap()).unwrap();
        assert_eq!((claims["iss"].as_str(), claims["repo"].as_str()), (Some("org"), Some("r-1")));
        assert_eq!(claims["exp"].as_i64().unwrap() - claims["iat"].as_i64().unwrap(), 300);
        assert_eq!(call(&k, Some(LEDGER), "POST", "codestorage/token", ask.clone()).0, 403);
        let mut bad = ask.clone();
        bad["scopes"] = json!(["admin"]);
        assert_eq!(call(&k, Some(ALICE), "POST", "codestorage/token", bad).0, 400);
        let mut long = ask;
        long["ttlS"] = json!(901);
        assert_eq!(call(&k, Some(ALICE), "POST", "codestorage/token", long).0, 400);
    }

    #[test]
    fn fleet_credentials_serve_one_class_each() {
        let k = keys(&[HOST]);
        assert_eq!(call(&k, Some(ALICE), "POST", "workos/authenticate", json!({ "clientId": "c", "code": "x" })).0, 403);
        assert_eq!(call(&k, Some(ALICE), "POST", "openrouter/keys", json!({ "method": "GET" })).0, 403);
        // the right class, on a node without the credential
        assert_eq!(call(&k, Some("Registry:dddd"), "POST", "workos/authenticate", json!({ "clientId": "c", "code": "x" })).0, 503);
        assert_eq!(call(&k, Some(LEDGER), "POST", "openrouter/keys", json!({ "method": "GET" })).0, 503);
        assert_eq!(call(&k, Some(ALICE), "POST", "sprites", json!({ "op": "get" })).0, 403);
        assert_eq!(call(&k, Some("Computer:eeee"), "POST", "sprites", json!({ "op": "get" })).0, 503);
        // each computer cell reaches its own Sprite, named for its id
        let id = "0123456789abcdef".repeat(4);
        assert_eq!(sprite_of(&format!("Computer:{id}")), "fragment-0123456789abcdef01234567");
    }
}
