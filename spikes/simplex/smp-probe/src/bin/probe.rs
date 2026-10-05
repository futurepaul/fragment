//! The SMP transport probe from the host: `probe <host> <port> <fingerprint> [<tls name>]`.

use std::time::Instant;

use smp_probe::smp;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [host, port, fp, ..] = args.as_slice() else {
        return Err("usage: probe <host> <port> <fingerprint> [<tls name>]".into());
    };
    let name = args.get(3).cloned().unwrap_or_else(|| host.clone());
    let identity = smp::fingerprint(fp)?;
    let t0 = Instant::now();
    let now = || t0.elapsed().as_secs_f64() * 1000.0;
    let sock = tokio::net::TcpStream::connect((host.as_str(), port.parse::<u16>().map_err(|e| e.to_string())?)).await.map_err(|e| e.to_string())?;
    let _ = sock.set_nodelay(true);
    println!("TCP connected: {:.1} ms", now());
    let mut corr = [0u8; 24];
    for (i, b) in corr.iter_mut().enumerate() {
        *b = (t0.elapsed().subsec_nanos() as u8).wrapping_add(i as u8);
    }
    let report = smp::probe(sock, &name, identity, &now, corr).await?;
    print!("{report}");
    Ok(())
}
