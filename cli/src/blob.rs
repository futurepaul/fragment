// The blob tier (docs/blob-tier.md): bytes live outside the cell, addressed
// by content hash, written CLI-direct to bloasd with a kind-24242 Blossom
// auth event; rows commit by reference afterwards (api::Client::put_ref).
//
//   changed file > 64 KiB: HEAD /<sha> -> hit? skip : PUT /upload with
//     Authorization: Nostr <24242 event> (x tag = tier public URL, payload =
//     the uploaded bytes' hash), then validate the echoed BlobDescriptor
//     sha256 against OUR digest before any row commit happens.
//   pull: .fragment/cache/<sha> short-circuits the network fetch entirely;
//     misses stream through tmp, land via atomic rename, and write through
//     into the cache (LRU soft-cap 256 MB, oldest-atime eviction).
use crate::auth::Identity;
use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Inline carve-out from docs/blob-tier.md: bodies up to this size may be
/// committed raw (the runtime uploads them itself inside the write turn);
/// anything larger MUST go blob-first.
pub const INLINE_MAX_BYTES: usize = 65536;

/// Mirror of bloasd's BLOBSD_MAX_BLOB_BYTES default (docs/blob-tier.md):
/// larger objects cannot land in a default-configured tier, so they are
/// warn-skipped up front instead of failing mid-sync.
pub const MAX_BLOB_BYTES: u64 = 67_108_864;

/// Bound on concurrent tier PUTs (the sync/commit loop itself stays serial;
/// only the pre-warm upload pass fans out, capped here).
pub const UPLOAD_CONCURRENCY: usize = 4;

/// Soft cap for `.fragment/cache/`: eviction runs only after adding an entry
/// and tolerates losing races with other sync processes.
pub const CACHE_CAP_BYTES: u64 = 256 * 1024 * 1024;

/// A resolved blob tier: base URL plus the identity signing upload events.
pub struct BlobTier {
    pub base: String,
    id: Identity,
    http: reqwest::blocking::Client,
}

impl BlobTier {
    /// Resolution order per the lane spec: FRAGMENT_BLOB_URL env overrides,
    /// then the optional `blob_url` key in the host config json (the same
    /// file main.rs loads; dirs::config_dir()/fragment/config.json).
    /// Absent/malformed => None, and oversized pushes then hard-fail with
    /// [`no_tier_error`] instead of silently falling back to raw bodies.
    pub fn resolve(id: &Identity) -> Option<BlobTier> {
        let url = std::env::var("FRAGMENT_BLOB_URL")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or_else(config_blob_url);
        let base = url?.trim_end_matches('/').to_string();
        if base.is_empty() || (!base.starts_with("http://") && !base.starts_with("https://")) {
            return None;
        }
        Some(BlobTier {
            base,
            id: Identity::from_secret(id.secret),
            http: reqwest::blocking::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(15))
                .build()
                .ok()?,
        })
    }

    /// Test seam: hand-built tier pointing anywhere (in-crate stub servers).
    #[cfg(test)]
    pub fn for_base(base: &str, id: Identity) -> BlobTier {
        BlobTier { base: base.trim_end_matches('/').to_string(), id, http: reqwest::blocking::Client::new() }
    }

    /// True when the tier does NOT have this object yet.
    ///
    /// Probed with a 1-byte ranged GET, not HEAD: the HEAD reads bloasd's
    /// descriptor row, which can outlive the R2 object it names (a bucket
    /// emptied out-of-band) — trusting it skips uploads for blobs that
    /// aren't actually there, and every read 404s forever after. A ranged
    /// GET touches the object itself, so the answer is the truth.
    pub fn needs_upload(&self, sha256: &str) -> bool {
        let url = format!("{}/{}", self.base, sha256);
        let hit = matches!(
            self.http
                .get(&url)
                .header("range", "bytes=0-0")
                .send(),
            Ok(resp) if resp.status().is_success()
        );
        !hit
    }

    /// Ensure the bytes are in the tier (HEAD-gated PUT /upload), verifying
    /// the echoed descriptor names OUR hash before declaring victory.
    pub fn ensure_uploaded(&self, sha256: &str, bytes: &[u8]) -> Result<()> {
        self.ensure_uploaded_with_mime(sha256, bytes, "application/octet-stream")
    }

    /// Mime rides the upload: bloasd stores it in the descriptor and serves
    /// it on every public read — omitting it here sentenced >64KB assets to
    /// application/octet-stream forever (dedupe replays never rewrite).
    pub fn ensure_uploaded_with_mime(&self, sha256: &str, bytes: &[u8], mime: &str) -> Result<()> {
        if !self.needs_upload(sha256) {
            return Ok(());
        }
        // bloasd checks kind/t=upload/expiration/x-tag and the payload tag
        // pins these exact bytes; x must equal its configured public URL.
        let auth = self.id.blossom_header("upload", &self.base, sha256, 300);
        let resp = self
            .http
            .put(format!("{}/upload", self.base))
            .header("authorization", &auth)
            .header("content-type", mime)
            .body(bytes.to_vec())
            .send()
            .context("blob tier unreachable")?;
        let status = resp.status().as_u16();
        let body = resp.bytes().context("reading blob descriptor")?;
        if !(200..300).contains(&status) {
            bail!(
                "blob upload rejected: http {} {}",
                status,
                String::from_utf8_lossy(&body).chars().take(200).collect::<String>()
            );
        }
        let desc: Value =
            serde_json::from_slice(&body).context("blob tier returned a non-JSON BlobDescriptor")?;
        let echoed = desc["sha256"].as_str().unwrap_or("");
        if !echoed.eq_ignore_ascii_case(sha256) {
            bail!(
                "blob tier echoed sha256 \"{echoed}\" but local hash is \"{sha256}\" — refusing to build on unverified storage"
            );
        }
        Ok(())
    }
}

/// Read the optional `blob_url` key from the host config json. Forgiving by
/// design (mirrors main.rs's loader): unreadable file just means unset.
fn config_blob_url() -> Option<String> {
    let path = dirs::config_dir()?.join("fragment").join("config.json");
    let text = std::fs::read_to_string(path).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    v["blob_url"].as_str().map(|s| s.to_string()).filter(|s| !s.is_empty())
}

/// Shared tier handle for one sync pass: the resolved tier (if any) plus a
/// memo so the concurrent pre-warm pass and the serial row-commit loop agree
/// on what already landed — or already failed, replaying the SAME error.
pub struct TierShared {
    pub blob: Option<BlobTier>,
    pub memo: Mutex<HashMap<String, Option<String>>>,
}

impl TierShared {
    pub fn new(blob: Option<BlobTier>) -> Arc<TierShared> {
        Arc::new(TierShared { blob, memo: Mutex::new(HashMap::new()) })
    }
}

/// Error for an oversized push with no tier available: loud, actionable,
/// exit non-zero (never a silent fallback to a raw body the runtime would
/// refuse past the inline limit).
pub fn no_tier_error(path: &str, size: usize) -> anyhow::Error {
    anyhow!(
        "{path} is {size} bytes — over the 64 KiB inline limit and no blob store is configured.\n  set FRAGMENT_BLOB_URL (env) or \"blob_url\" (fragment config) to the bloasd base URL, then re-run"
    )
}

/// Upload one candidate via the shared memo (memo hit short-circuits both
/// directions: successes are free, failures replay their original error).
pub fn blob_ensure(shared: &Arc<TierShared>, sha256: &str, bytes: &[u8], mime: &str) -> Result<()> {
    {
        let memo = shared.memo.lock().expect("memo lock");
        if let Some(entry) = memo.get(sha256) {
            return match entry {
                None => Ok(()),
                Some(err) => bail!("{err}"),
            };
        }
    }
    let outcome = match &shared.blob {
        Some(tier) => tier.ensure_uploaded(sha256, bytes),
        None => Err(no_tier_error("(file — path unavailable in prewarm)", bytes.len())),
    };
    let msg = outcome.as_ref().err().map(|e| format!("{e:#}"));
    shared.memo.lock().expect("memo lock").insert(sha256.to_string(), msg);
    outcome
}

// ---------- extension -> mime ----------
// Intentional duplication: mirrors the web-hosting MIME list in
// docs/blob-tier.md ("woff/woff2/ttf/otf/json/webmanifest/svg-refs/etc").
// The runtime owns the canonical serving table; the CLI only fills
// ref-form descriptors. Keep both sides in sync manually.
pub fn mime_for(path: &str) -> &'static str {
    let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    let found = match ext.as_str() {
        "html" | "htm" => "text/html",
        "css" => "text/css",
        "js" | "mjs" => "text/javascript",
        "json" | "map" => "application/json",
        "webmanifest" => "application/manifest+json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "bmp" => "image/bmp",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "txt" | "log" => "text/plain",
        "md" | "markdown" => "text/markdown",
        "csv" => "text/csv",
        "xml" => "application/xml",
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        "wasm" => "application/wasm",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "mov" => "video/quicktime",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "ogg" => "audio/ogg",
        "ics" => "text/calendar",
        _ => "",
    };
    // "" would be a lie about content; octet-stream is the honest default
    if found.is_empty() { "application/octet-stream" } else { found }
}

pub fn is_valid_sha256(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

// ---------- pull cache (.fragment/cache/<sha>) ----------

pub fn cache_dir(dir: &Path) -> PathBuf {
    dir.join(".fragment").join("cache")
}

/// Short-circuit probe BEFORE any network traffic. Content-addressed keys are
/// strictly validated first: a hostile listing can never alias or traverse.
pub fn cache_lookup(dir: &Path, sha256: &str) -> Option<PathBuf> {
    if !is_valid_sha256(sha256) {
        return None;
    }
    let p = cache_dir(dir).join(sha256.to_ascii_lowercase());
    p.is_file().then_some(p)
}

/// Write-through: copy a freshly materialized tmp file into the cache under
/// its lowercase-hex name; rename keeps the cache internally atomic.
pub fn cache_store(dir: &Path, sha256: &str, tmp_file: &Path) -> Result<()> {
    if !is_valid_sha256(sha256) {
        return Ok(()); // unaddressable body: nothing sane to cache
    }
    let cdir = cache_dir(dir);
    std::fs::create_dir_all(&cdir)?;
    let dest = cdir.join(sha256.to_ascii_lowercase());
    if dest.exists() {
        return Ok(()); // content-addressed: identical bytes already there
    }
    let staging = cdir.join(format!(".staging-{}", std::process::id()));
    let _ = std::fs::remove_file(&staging);
    std::fs::copy(tmp_file, &staging).with_context(|| format!("caching {}", sha256))?;
    std::fs::rename(&staging, &dest)?; // atomic within the cache dir
    evict_if_over_cap(&cdir, CACHE_CAP_BYTES);
    Ok(())
}

/// LRU-by-atime trim under `cap`. Every failure mode degrades to warnings:
/// losing cache entries costs a re-download, never correctness.
pub fn evict_if_over_cap(cdir: &Path, cap: u64) {
    let rd = match std::fs::read_dir(cdir) {
        Ok(rd) => rd,
        Err(_) => return,
    };
    let mut entries: Vec<(PathBuf, u64, std::time::SystemTime)> = Vec::new();
    let mut total: u64 = 0;
    for entry in rd.flatten() {
        let md = match entry.metadata() {
            Ok(md) if md.is_file() => md,
            _ => continue,
        };
        total += md.len();
        entries.push((entry.path(), md.len(), md.accessed().unwrap_or(std::time::UNIX_EPOCH)));
    }
    if total <= cap {
        return;
    }
    entries.sort_by_key(|(_, _, t)| *t); // oldest accessed first out
    for (p, len, _) in &entries {
        if total <= cap {
            break;
        }
        match std::fs::remove_file(p) {
            Ok(()) => total = total.saturating_sub(*len),
            Err(e) => eprintln!("warning: could not evict cache entry {}: {e}", p.display()),
        }
    }
    if total > cap {
        eprintln!("warning: blob cache still over its soft cap after eviction");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Identity;
    use sha2::{Digest, Sha256};
    use std::io::{Read, Write};
    use std::sync::atomic::{AtomicBool, Ordering};

    type Counts = Arc<Mutex<HashMap<String, usize>>>;
    type Blobs = Arc<Mutex<HashMap<String, Vec<u8>>>>;

    /// Hand-rolled minimal HTTP stub over std::net (no new crates). Auth is
    /// PRESENCE-only by mandate: real crypto validation lives in blobsd's own
    /// suite. Implements just enough of the two-tier surface for the CLI paths.
    struct Stub {
        addr: String,
        counts: Counts,
        blobs: Blobs,
        last_ref_put: Arc<Mutex<Option<(String, Value)>>>,
        /// when set, PUT /upload descriptors echo a WRONG hash (dishonest server)
        lie: Arc<AtomicBool>,
    }

    impl Stub {
        fn spawn() -> Stub {
            use std::net::TcpListener;
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
            let addr = listener.local_addr().unwrap().to_string();
            let counts: Counts = Default::default();
            let blobs: Blobs = Default::default();
            let last_ref_put: Arc<Mutex<Option<(String, Value)>>> = Default::default();
            let lie = Arc::new(AtomicBool::new(false));
            let (c2, b2, l2, li2) = (counts.clone(), blobs.clone(), last_ref_put.clone(), lie.clone());
            std::thread::spawn(move || {
                for stream in listener.incoming().flatten() {
                    let (c, b, l, li) = (c2.clone(), b2.clone(), l2.clone(), li2.clone());
                    std::thread::spawn(move || {
                        let mut s = stream;
                        let _ = handle_conn(&mut s, c, b, l, li);
                    });
                }
            });
            Stub { addr, counts, blobs, last_ref_put, lie }
        }

        fn base(&self) -> String {
            format!("http://{}", self.addr)
        }

        /// sum of requests seen whose "METHOD path" key starts with prefix
        fn count_of(&self, method: &str, path: &str) -> usize {
            self.counts
                .lock()
                .unwrap()
                .iter()
                .filter(|(k, _)| k.starts_with(&format!("{method} {path}")))
                .map(|(_, v)| *v)
                .sum()
        }
    }

    fn handle_conn(
        stream: &mut std::net::TcpStream,
        counts: Counts,
        blobs: Blobs,
        last_ref: Arc<Mutex<Option<(String, Value)>>>,
        lie: Arc<AtomicBool>,
    ) -> std::io::Result<()> {
        let mut buf = [0u8; 16384];
        let mut data: Vec<u8> = Vec::new();
        loop {
            let n = stream.read(&mut buf)?;
            if n == 0 {
                break;
            }
            data.extend_from_slice(&buf[..n]);
            if data.windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
        }
        let head_end = data.windows(4).position(|w| w == b"\r\n\r\n").map_or(data.len(), |p| p + 4);
        let head = String::from_utf8_lossy(&data[..head_end]).to_string();
        let mut lines = head.split("\r\n");
        let req_line = lines.next().unwrap_or_default().to_string();
        let mut parts = req_line.split(' ');
        let method = parts.next().unwrap_or_default().to_string();
        let target = parts.next().unwrap_or("/").to_string();
        let lower_head = head.to_ascii_lowercase();
        let has_nostr_auth = lower_head.contains("\r\nauthorization: nostr ");
        let content_length = head
            .split("\r\n")
            .find_map(|l| l.strip_prefix("content-length:").and_then(|v| v.trim().parse::<usize>().ok()))
            .unwrap_or(0);
        while data.len() < head_end + content_length {
            let n = stream.read(&mut buf)?;
            if n == 0 {
                break;
            }
            data.extend_from_slice(&buf[..n]);
        }
        let body = data[head_end.min(data.len())..].to_vec();

        *counts.lock().unwrap().entry(format!("{method} {target}")).or_default() += 1;
        let req_ct_json = lower_head.contains("\r\ncontent-type: application/json");

        enum R {
            #[allow(dead_code)] // POST-style answers kept off lint radars
            Json(u16, Value),
            Bytes(u16, &'static str, Vec<u8>),
            Redirect(String),
        }

        let r = match (method.as_str(), target.as_str()) {
            ("HEAD", t) if t.len() == 65 && t.starts_with('/') => {
                let hit = blobs.lock().unwrap().contains_key(&t[1..]);
                R::Bytes(if hit { 200 } else { 404 }, "", vec![])
            }
            ("PUT", "/upload") => {
                if !has_nostr_auth {
                    R::Json(401, serde_json::json!({"error": "missing Authorization event"}))
                } else {
                    let sha = hex::encode(Sha256::digest(&body));
                    blobs.lock().unwrap().entry(sha.clone()).or_insert_with(|| body.clone());
                    let echoed = if lie.load(Ordering::SeqCst) { "f".repeat(64) } else { sha.clone() };
                    R::Json(
                        200,
                        serde_json::json!({"sha256": echoed, "size": body.len(), "type": "application/octet-stream", "uploaded": 1787800000000i64}),
                    )
                }
            }
            ("GET", t) if t.len() == 65 && t.starts_with('/') => match blobs.lock().unwrap().get(&t[1..]) {
                Some(b) => R::Bytes(200, "content-type: application/octet-stream\r\ncache-control: public, max-age=31536000, immutable\r\n", b.clone()),
                None => R::Bytes(404, "", vec![]),
            },
            // NEW-runtime public read shape: 302 straight to the hashed asset
            ("GET", t) if t.starts_with("/api/f/") && t.contains("/file?path=") => {
                let m = blobs.lock().unwrap();
                match m.iter().next() {
                    Some((sha, _)) => R::Redirect(format!("/{sha}")),
                    None => R::Bytes(404, "", vec![]),
                }
            }
            ("PUT", t) if t.starts_with("/api/f/") && t.contains("/file?") => {
                if req_ct_json {
                    let v: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                    let sha_ok = v["ref"]["sha256"].as_str().is_some_and(is_valid_sha256);
                    if sha_ok {
                        *last_ref.lock().unwrap() = Some((t.to_string(), v));
                        R::Json(200, serde_json::json!({"rev": 7}))
                    } else {
                        R::Json(400, serde_json::json!({"error": "bad ref"}))
                    }
                } else {
                    R::Json(200, serde_json::json!({"rev": 7}))
                }
            }
            ("GET", _) if target.contains("/files?since_rev=0") => {
                R::Json(200, serde_json::json!({"files": []}))
            }
            ("GET", _) if target.contains("/manifest") => {
                R::Json(200, serde_json::json!({"appendOnly": []}))
            }
            _ => R::Bytes(404, "", vec![]),
        };

        let out = match r {
            R::Json(status, v) => {
                let b = serde_json::to_vec(&v).unwrap_or_default();
                format!(
                    "HTTP/1.1 {status} STUB\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    b.len()
                )
                .into_bytes()
                .tap_extend(b)
            }
            R::Bytes(status, hdrs, b) => format!(
                "HTTP/1.1 {status} STUB\r\netag: \"stub\"\r\n{hdrs}content-length: {}\r\nconnection: close\r\n\r\n",
                b.len()
            )
            .into_bytes()
            .tap_extend(b),
            R::Redirect(loc) => format!(
                "HTTP/1.1 302 Found\r\nlocation: {loc}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
            )
            .into_bytes(),
        };
        stream.write_all(&out)?;
        stream.flush()
    }

    trait TapExtend: Sized {
        fn tap_extend(self, more: Vec<u8>) -> Vec<u8>;
    }
    impl TapExtend for Vec<u8> {
        fn tap_extend(mut self, more: Vec<u8>) -> Vec<u8> {
            self.extend(more);
            self
        }
    }

    fn sha_of(b: &[u8]) -> String {
        crate::sync::sha256_hex(b)
    }

    #[test]
    fn mime_map_fonts_and_conventions() {
        assert_eq!(mime_for("fonts/Inter.woff2"), "font/woff2");
        assert_eq!(mime_for("a/b/site.FONT.TTF"), "font/ttf"); // case-insensitive ext
        assert_eq!(mime_for("app.woff"), "font/woff");
        assert_eq!(mime_for("styles/app.css"), "text/css");
        assert_eq!(mime_for("site.webmanifest"), "application/manifest+json");
        assert_eq!(mime_for("logo.svg"), "image/svg+xml");
        assert_eq!(mime_for("bundle.js.map"), "application/json");
        assert_eq!(mime_for("data.bin"), "application/octet-stream");
        assert_eq!(mime_for("notes.big.MD"), "text/markdown");
    }

    #[test]
    fn presence_probe_skips_put_and_descriptor_echo_is_checked() {
        let stub = Stub::spawn();
        let tier = BlobTier::for_base(&stub.base(), Identity::from_secret([7u8; 32]));
        let bytes = b"fragment blob tier";
        let sha = sha_of(bytes);

        // first call: HEAD miss -> PUT /upload carrying the auth event
        tier.ensure_uploaded(&sha, bytes).expect("fresh upload succeeds");
        assert_eq!(stub.count_of("GET", "/"), 1, "one presence probe");
        assert_eq!(stub.count_of("PUT", "/upload"), 1, "one upload PUT");

        // an explicit presence check now answers negatively WITHOUT another PUT
        assert!(!tier.needs_upload(&sha), "object now present");
        assert_eq!(stub.count_of("GET", "/"), 2);
        assert_eq!(stub.count_of("PUT", "/upload"), 1);

        // idempotent re-push: HEAD hit means zero further PUTs
        tier.ensure_uploaded(&sha, bytes).expect("HEAD-hit short circuit");
        assert_eq!(stub.count_of("GET", "/"), 3);
        assert_eq!(stub.count_of("PUT", "/upload"), 1, "HEAD hit skips the PUT");

        // bytes addressable by their exact hash afterwards
        assert!(stub.blobs.lock().unwrap().contains_key(&sha));
    }

    #[test]
    fn dishonest_descriptor_sha_aborts() {
        let stub = Stub::spawn();
        stub.lie.store(true, Ordering::SeqCst);
        let tier = BlobTier::for_base(&stub.base(), Identity::from_secret([7u8; 32]));
        let bytes = b"payload whose echoed hash will be lied about";
        let err = tier.ensure_uploaded(&sha_of(bytes), bytes).expect_err("lied-about hash refused");
        assert!(
            format!("{err:#}").contains("refusing"),
            "error must name the refusal: {err:#}"
        );
    }

    #[test]
    fn upload_requires_auth_event_presence_even_on_stub() {
        let stub = Stub::spawn();
        let bare = reqwest::blocking::Client::new();
        let st = bare.put(format!("{}/upload", stub.base())).body(vec![1u8; 16]).send().unwrap();
        assert_eq!(st.status().as_u16(), 401, "missing Authorization event rejected");
    }

    #[test]
    fn no_tier_guidance_names_the_env_var() {
        let shared = TierShared::new(None);
        let bytes = vec![0u8; INLINE_MAX_BYTES + 1];
        let err = blob_ensure(&shared, &sha_of(&bytes), &bytes, "application/octet-stream").expect_err("absent tier must hard-fail");
        let msg = format!("{err:#}");
        assert!(msg.contains("FRAGMENT_BLOB_URL"), "guidance names the env var: {msg}");
        assert!(msg.contains("blob_url"), "guidance names the config key: {msg}");
        // memo replays the SAME failure without retrying any network call
        let again = blob_ensure(&shared, &sha_of(&bytes), &bytes, "application/octet-stream").expect_err("replayed");
        assert_eq!(format!("{again:#}"), msg);
    }

    #[test]
    fn put_ref_wire_form_captured_on_stub() {
        let stub = Stub::spawn();
        let ctx = crate::api::Client::new(&stub.base(), Identity::from_secret([7u8; 32]));
        let sha = "cd".repeat(32);
        let resp = ctx
            .put_ref(
                &format!("/api/f/e2e/file?path={}&base_rev=0", crate::api::encode_q("big.bin")),
                &sha,
                1234,
                "font/woff2",
            )
            .expect("put_ref completes");
        assert!(resp.ok());
        assert_eq!(resp.json().unwrap()["rev"], 7);

        let (path, body) = stub.last_ref_put.lock().unwrap().clone().expect("captured");
        assert!(path.starts_with("/api/f/e2e/file?path="), "{path}");
        assert!(path.contains("base_rev=0"));
        assert_eq!(body["ref"]["sha256"], sha);
        assert_eq!(body["ref"]["size"], 1234);
        assert_eq!(body["ref"]["mime"], "font/woff2");

        // malformed refs bounce rather than silently committing junk rows
        let bad = ctx.put_ref("/api/f/e2e/file?path=x&base_rev=0", "nothex", 5, "").unwrap();
        assert_eq!(bad.status, 400);
    }

    #[test]
    fn get_stream_follows_tier_redirect_and_streams_body() {
        let stub = Stub::spawn();
        let tier = BlobTier::for_base(&stub.base(), Identity::from_secret([7u8; 32]));
        let payload = b"bytes behind a 302";
        tier.ensure_uploaded(&sha_of(payload), payload).expect("seed object");

        let ctx = crate::api::Client::new(&stub.base(), Identity::from_secret([7u8; 32]));
        let sr = ctx
            .get_stream("/api/f/somefrag/file?path=big.bin")
            .expect("streaming GET completes (redirect followed)");
        assert_eq!(sr.status, 200, "302 was followed transparently");
        let etag_present = sr.etag().is_some();
        assert!(etag_present, "immutable asset carries an etag");
        let mut buf = Vec::new();
        sr.into_read().read_to_end(&mut buf).expect("stream read");
        assert_eq!(buf, payload.to_vec());
        assert_eq!(stub.count_of("GET", "/api/f/"), 1, "exactly one network fetch");
    }

    #[test]
    fn cache_roundtrip_validation_and_eviction_order() {
        let dir = std::env::temp_dir().join(format!("fragment-blobcache-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let sha = "ab".repeat(32);

        assert!(cache_lookup(&dir, &sha).is_none());
        let tmp = dir.join("incoming.tmp");
        std::fs::write(&tmp, b"cached bytes").unwrap();
        cache_store(&dir, &sha, &tmp).unwrap();
        let cached = cache_lookup(&dir, &sha).expect("cached after write-through");
        assert_eq!(std::fs::read(&cached).unwrap(), b"cached bytes");

        // malformed names never become entries (no traversal, no aliasing)
        assert!(cache_lookup(&dir, "../../secret").is_none());
        assert!(cache_lookup(&dir, "zz").is_none());
        assert!(cache_store(&dir, "../../evil", &tmp).is_ok());
        let entries: Vec<String> = std::fs::read_dir(cache_dir(&dir))
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(entries.len(), 1, "exactly the one hashed entry: {entries:?}");
        assert_eq!(entries[0], sha);

        // LRU: three entries with forced access times; a tiny cap evicts the
        // oldest-ACCESSED entry (smaller absolute timestamp = more stale)
        let base = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        for (name, age_secs) in [("old0", 3_000u64), ("mid1", 2_000), ("new2", 10)] {
            let p = cache_dir(&dir).join(name);
            std::fs::write(&p, vec![b'x'; 100]).unwrap();
            let f = std::fs::File::options().write(true).open(&p).unwrap();
            let t = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(base - age_secs);
            f.set_times(std::fs::FileTimes::new().set_accessed(t).set_modified(t)).unwrap();
        }
        evict_if_over_cap(&cache_dir(&dir), 250);
        let survivors: Vec<String> = std::fs::read_dir(cache_dir(&dir))
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(survivors.iter().all(|n| n != "old0"), "oldest-atime entry evicted: {survivors:?}");
        assert!(survivors.contains(&"new2".to_string()), "newest survives: {survivors:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
