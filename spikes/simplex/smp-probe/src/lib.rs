//! The SMP transport probe (src/smp.rs), and as a Worker:
//!
//!   GET /?host=&port=&fp=&mode=on    `connect()` with the platform's TLS (`secureTransport: "on"`)
//!   GET /?host=&port=&fp=&mode=off   `connect()` plain, with TLS 1.3 in userland (rustls), then SMP's hellos and a PING

pub mod smp;

#[cfg(target_arch = "wasm32")]
mod worker_entry {
    use std::collections::HashMap;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use worker::{event, Context, Date, Env, Request, Response, Result, SecureTransport, Socket};

    use crate::smp;

    #[event(fetch)]
    async fn fetch(req: Request, _env: Env, _ctx: Context) -> Result<Response> {
        let url = req.url()?;
        let q: HashMap<String, String> = url.query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
        let host = q.get("host").cloned().unwrap_or_else(|| "127.0.0.1".into());
        let name = q.get("name").cloned().unwrap_or_else(|| host.clone());
        let port: u16 = q.get("port").and_then(|p| p.parse().ok()).unwrap_or(5223);
        let fp = q.get("fp").cloned().unwrap_or_default();
        let t0 = Date::now().as_millis() as f64;
        let now = move || Date::now().as_millis() as f64 - t0;
        let out = match q.get("mode").map(String::as_str).unwrap_or("off") {
            // the platform's TLS: it validates against public roots, and SMP's
            // self-signed chain is not one; a write and a read make it try
            "on" => {
                let mut sock = Socket::builder().secure_transport(SecureTransport::On).connect(host.clone(), port)?;
                let opened = sock.opened().await.map(|i| format!("{:?}", i.remote_address)).map_err(|e| e.to_string());
                let mut buf = vec![0u8; 64];
                let wrote = sock.write_all(b"\0").await.map_err(|e| e.to_string());
                let read = sock.read(&mut buf).await.map_err(|e| e.to_string());
                format!("secureTransport on, {host}:{port}\nopened: {opened:?}\nwrite: {wrote:?}\nread: {read:?}\n")
            }
            _ => {
                let identity = match smp::fingerprint(&fp) {
                    Ok(i) => i,
                    Err(e) => return Response::error(e, 400),
                };
                let sock = Socket::builder().secure_transport(SecureTransport::Off).connect(host.clone(), port)?;
                let opened = sock.opened().await;
                let tcp = now();
                let mut corr = [0u8; 24];
                getrandom_fill(&mut corr);
                match opened {
                    Err(e) => format!("secureTransport off, {host}:{port}\nopened: {e}\n"),
                    Ok(_) => match smp::probe(sock, &name, identity, &now, corr).await {
                        Ok(r) => format!("secureTransport off + rustls (wasm32), {host}:{port}\nTCP opened: {tcp:.1} ms\n{r}"),
                        Err(e) => format!("secureTransport off + rustls (wasm32), {host}:{port}\nTCP opened: {tcp:.1} ms\nfailed: {e}\n"),
                    },
                }
            }
        };
        Response::ok(out)
    }

    /// 24 random bytes from ring's source (the JS crypto API on wasm32).
    fn getrandom_fill(b: &mut [u8]) {
        use ring::rand::SecureRandom;
        let _ = ring::rand::SystemRandom::new().fill(b);
    }
}
