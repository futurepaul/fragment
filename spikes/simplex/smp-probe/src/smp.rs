//! SMP's transport from a client, as far as one PING: TLS 1.3 to the router
//! with its identity pinned by fingerprint (no WebPKI, no SNI, ALPN `smp/1`),
//! the router's hello, the client's hello, then PING and its PONG
//! (simplexmq `protocol/simplex-messaging.md`, "TLS transport encryption",
//! "Router certificate", "ALPN to agree handshake version", "Transport
//! handshake"; `src/Simplex/Messaging/Transport.hs` for the exact bytes).
//!
//! Generic over any tokio stream, so the same code runs over a host
//! `TcpStream` and over a Worker's `connect()` socket.
//!
//! What it leaves out, as a spike: the leaf certificate's signature by the
//! pinned one is not checked (the fingerprint and the TLS handshake's own
//! signature are); the session identifier is taken from the router's hello,
//! since rustls exposes no `tls-unique`; no client key is sent, so the
//! router adds no block encryption inside TLS.

use std::sync::Arc;

use base64::Engine;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{verify_tls12_signature, verify_tls13_signature, CryptoProvider, WebPkiSupportedAlgorithms};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Every SMP transport block is this long.
pub const BLOCK: usize = 16384;
/// The highest SMP version this probe says it speaks (simplexmq 7.0.1's).
pub const SMP_VERSION: u16 = 20;

/// What a probe found.
#[derive(Debug, Default)]
pub struct Report {
    pub alpn: Option<String>,
    pub cipher: Option<String>,
    /// The router's SMP version range.
    pub versions: (u16, u16),
    pub session_id_len: usize,
    /// Certificates in the chain the router's hello carries.
    pub hello_chain: usize,
    /// The router answered PING with PONG.
    pub pong: bool,
    /// (step, ms since the probe began)
    pub times: Vec<(&'static str, f64)>,
}

impl std::fmt::Display for Report {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "alpn: {:?}", self.alpn)?;
        writeln!(f, "cipher: {:?}", self.cipher)?;
        writeln!(f, "router SMP versions: {}..={}", self.versions.0, self.versions.1)?;
        writeln!(f, "session identifier: {} bytes", self.session_id_len)?;
        writeln!(f, "certificates in the router's hello: {}", self.hello_chain)?;
        writeln!(f, "PING answered with PONG: {}", self.pong)?;
        for (step, ms) in &self.times {
            writeln!(f, "  {step}: {ms:.1} ms")?;
        }
        Ok(())
    }
}

/// A router identity from its address (`smp://<fingerprint>@host`): the
/// SHA-256 of its identity certificate, base64url.
pub fn fingerprint(s: &str) -> Result<[u8; 32], String> {
    let b = base64::engine::general_purpose::URL_SAFE
        .decode(s.trim())
        .map_err(|e| format!("the fingerprint is not base64url: {e}"))?;
    b.try_into().map_err(|_| "a fingerprint is 32 bytes".to_string())
}

/// Accepts the router whose chain carries the pinned identity certificate:
/// SMP's chain is `[leaf, identity]` (or 3 or 4 with an operator's), and
/// the identity is the second (`chainIdCaCerts` in simplexmq's
/// `Transport/Shared.hs`).
#[derive(Debug)]
struct Pinned {
    identity: [u8; 32],
    algs: WebPkiSupportedAlgorithms,
}

impl ServerCertVerifier for Pinned {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if !(1..=3).contains(&intermediates.len()) {
            return Err(rustls::Error::General(format!("an SMP router sends 2 to 4 certificates, not {}", intermediates.len() + 1)));
        }
        let id = Sha256::digest(intermediates[0].as_ref());
        if id.as_slice() != self.identity {
            return Err(rustls::Error::General("the router's identity certificate is not the pinned one".into()));
        }
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(message, cert, dss, &self.algs)
    }

    fn verify_tls13_signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, dss, &self.algs)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algs.supported_schemes()
    }
}

/// The TLS client an SMP router accepts: TLS 1.3, ChaCha20-Poly1305,
/// X25519, Ed25519 (ring's), ALPN `smp/1`, no SNI (a router that also serves
/// its web page answers SNI with the page).
pub fn tls_config(identity: [u8; 32]) -> Result<rustls::ClientConfig, String> {
    let ring = rustls::crypto::ring::default_provider();
    let provider = CryptoProvider {
        cipher_suites: vec![rustls::crypto::ring::cipher_suite::TLS13_CHACHA20_POLY1305_SHA256],
        kx_groups: vec![rustls::crypto::ring::kx_group::X25519],
        ..ring
    };
    let algs = provider.signature_verification_algorithms;
    let mut config = rustls::ClientConfig::builder_with_provider(Arc::new(provider))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|e| e.to_string())?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(Pinned { identity, algs }))
        .with_no_client_auth();
    config.alpn_protocols = vec![b"smp/1".to_vec()];
    config.enable_sni = false;
    config.resumption = rustls::client::Resumption::disabled();
    Ok(config)
}

/// `content` as one block: its length (word16, big-endian), then it, then
/// `#` to the block's end.
pub fn pad(content: &[u8]) -> Vec<u8> {
    assert!(content.len() + 2 <= BLOCK, "a block holds {} bytes at most", BLOCK - 2);
    let mut b = Vec::with_capacity(BLOCK);
    b.extend_from_slice(&(content.len() as u16).to_be_bytes());
    b.extend_from_slice(content);
    b.resize(BLOCK, b'#');
    b
}

pub fn unpad(block: &[u8]) -> Result<&[u8], String> {
    let n = u16::from_be_bytes([block[0], block[1]]) as usize;
    block.get(2..2 + n).ok_or_else(|| "a block shorter than its length".into())
}

fn short(b: &[u8], at: &mut usize) -> Result<Vec<u8>, String> {
    let n = *b.get(*at).ok_or("truncated")? as usize;
    let v = b.get(*at + 1..*at + 1 + n).ok_or("truncated")?.to_vec();
    *at += 1 + n;
    Ok(v)
}

fn large(b: &[u8], at: &mut usize) -> Result<Vec<u8>, String> {
    let n = u16::from_be_bytes([*b.get(*at).ok_or("truncated")?, *b.get(*at + 1).ok_or("truncated")?]) as usize;
    let v = b.get(*at + 2..*at + 2 + n).ok_or("truncated")?.to_vec();
    *at += 2 + n;
    Ok(v)
}

/// The probe: TLS, both hellos, PING, PONG. `now` is milliseconds on any
/// clock; `corr_id` is the PING's correlation id (random, 24 bytes).
pub async fn probe<S>(sock: S, host: &str, identity: [u8; 32], now: &dyn Fn() -> f64, corr_id: [u8; 24]) -> Result<Report, String>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let t0 = now();
    let mut r = Report::default();
    let connector = tokio_rustls::TlsConnector::from(Arc::new(tls_config(identity)?));
    let name = ServerName::try_from(host.to_string()).map_err(|e| e.to_string())?;
    let mut tls = connector.connect(name, sock).await.map_err(|e| format!("TLS: {e}"))?;
    r.times.push(("TLS 1.3 handshake", now() - t0));
    {
        let (_, conn) = tls.get_ref();
        r.alpn = conn.alpn_protocol().map(|p| String::from_utf8_lossy(p).to_string());
        r.cipher = conn.negotiated_cipher_suite().map(|c| format!("{:?}", c.suite()));
    }
    if r.alpn.as_deref() != Some("smp/1") {
        return Err(format!("the router did not agree ALPN smp/1: {:?}", r.alpn));
    }
    // the router's hello: smpVersionRange sessionIdentifier [certChain signedRouterKey]
    let mut block = vec![0u8; BLOCK];
    tls.read_exact(&mut block).await.map_err(|e| format!("reading the router's hello: {e}"))?;
    r.times.push(("router hello", now() - t0));
    let hello = unpad(&block)?;
    r.versions = (u16::from_be_bytes([hello[0], hello[1]]), u16::from_be_bytes([hello[2], hello[3]]));
    let mut at = 4;
    let session_id = short(hello, &mut at)?;
    r.session_id_len = session_id.len();
    if at < hello.len() {
        r.hello_chain = hello[at] as usize;
        at += 1;
        for _ in 0..r.hello_chain {
            large(hello, &mut at)?;
        }
    }
    let v = SMP_VERSION.min(r.versions.1);
    if v < r.versions.0 || v < 16 {
        return Err(format!("no version in common: the router speaks {:?}", r.versions));
    }
    // the client's hello: smpVersion keyHash [clientKey] proxyRouter optClientService
    let mut ch = Vec::new();
    ch.extend_from_slice(&v.to_be_bytes());
    ch.push(32);
    ch.extend_from_slice(&identity);
    ch.push(b'F'); // not a proxy router
    ch.push(b'0'); // no service certificate
    tls.write_all(&pad(&ch)).await.map_err(|e| format!("sending the client's hello: {e}"))?;
    // PING: one transmission, unauthorized, no entity
    let mut t = vec![0u8]; // empty authorization
    t.push(24);
    t.extend_from_slice(&corr_id);
    t.push(0); // no entity id
    t.extend_from_slice(b"PING");
    let mut batch = vec![1u8];
    batch.extend_from_slice(&(t.len() as u16).to_be_bytes());
    batch.extend_from_slice(&t);
    tls.write_all(&pad(&batch)).await.map_err(|e| format!("sending PING: {e}"))?;
    tls.flush().await.map_err(|e| e.to_string())?;
    let sent = now();
    tls.read_exact(&mut block).await.map_err(|e| format!("reading the answer to PING: {e}"))?;
    r.times.push(("PING → PONG round trip", now() - sent));
    r.times.push(("total", now() - t0));
    let answer = unpad(&block)?;
    r.pong = answer.windows(4).any(|w| w == b"PONG") && answer.windows(24).any(|w| w == corr_id);
    if !r.pong {
        return Err(format!("no PONG: {:?}", String::from_utf8_lossy(answer)));
    }
    let _ = tls.shutdown().await;
    Ok(r)
}
