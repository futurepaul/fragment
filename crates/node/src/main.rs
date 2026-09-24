//! Starts celld on a fleet Machine (Fly). The public listener takes :8080
//! (the Fly service); the peer and operator listener takes the Machine's
//! private address (Fly's 6PN, never a service) and is advertised to the
//! other nodes as is; the work directory is on the Machine's volume, paired
//! for life with the bucket it first served (celld's watch dir must never
//! be pointed at another bucket). celld reads everything else from the
//! environment: the bucket from fly.toml, its credentials from Fly secrets.

use std::net::Ipv6Addr;
use std::os::unix::fs::MetadataExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;

const CELLD: &str = "/usr/local/bin/celld";
const PUBLIC: &str = "0.0.0.0:8080";
const INTERNAL_PORT: u16 = 8081;
/// The volume's mount point (fly.toml `[mounts]`).
const VOLUME: &str = "/data";

/// The internal listener's address: the Machine's private IP, which must be
/// in Fly's private network (fdaa::/16).
fn internal_addr(private_ip: &str) -> Result<String, String> {
    let ip: Ipv6Addr = private_ip.parse().map_err(|_| format!("FLY_PRIVATE_IP {private_ip:?} is not an IPv6 address"))?;
    if ip.segments()[0] != 0xfdaa {
        return Err(format!("FLY_PRIVATE_IP {ip} is not in Fly's private network (fdaa::/16)"));
    }
    Ok(format!("[{ip}]:{INTERNAL_PORT}"))
}

/// Records the bucket a work directory serves on first start, and refuses
/// any other bucket after.
fn pair(marker: &Path, bucket: &str) -> Result<(), String> {
    match std::fs::read_to_string(marker) {
        Ok(paired) if paired.trim() == bucket => Ok(()),
        Ok(paired) => Err(format!("this volume served {} and must not serve {bucket}: give the Machine a new volume", paired.trim())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            std::fs::write(marker, format!("{bucket}\n")).map_err(|e| format!("writing {}: {e}", marker.display()))
        }
        Err(e) => Err(format!("reading {}: {e}", marker.display())),
    }
}

fn fail(msg: String) -> ! {
    eprintln!("fragment-node: {msg}");
    std::process::exit(2)
}

fn main() {
    let private_ip = std::env::var("FLY_PRIVATE_IP").unwrap_or_else(|_| fail("FLY_PRIVATE_IP is unset (not on a Fly Machine?)".into()));
    let internal = internal_addr(&private_ip).unwrap_or_else(|e| fail(e));
    let bucket = std::env::var("CELLD_BUCKET").unwrap_or_else(|_| fail("CELLD_BUCKET is unset".into()));
    let (root, volume) = (std::fs::metadata("/"), std::fs::metadata(VOLUME));
    match (root, volume) {
        (Ok(r), Ok(v)) if v.is_dir() && v.dev() != r.dev() => {}
        _ => fail(format!("{VOLUME} is not a mounted volume: celld's work directory must outlive restarts")),
    }
    pair(&Path::new(VOLUME).join("bucket"), bucket.trim()).unwrap_or_else(|e| fail(e));
    let err = Command::new(CELLD)
        .env("CELLD_ADDR", PUBLIC)
        .env("CELLD_INTERNAL_ADDR", &internal)
        .env("CELLD_ADVERTISE", &internal)
        .env("CELLD_WATCH", format!("{VOLUME}/watch"))
        .exec();
    fail(format!("exec {CELLD}: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn internal_address() {
        assert_eq!(internal_addr("fdaa:0:1:a7b:1f2:3c4d:5e6f:2").as_deref(), Ok("[fdaa:0:1:a7b:1f2:3c4d:5e6f:2]:8081"));
        assert!(internal_addr("2606:4700::1").unwrap_err().contains("private network"));
        assert!(internal_addr("10.0.0.1").unwrap_err().contains("not an IPv6"));
    }

    #[test]
    fn a_volume_serves_one_bucket() {
        let dir = std::env::temp_dir().join(format!("fragment-node-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let marker = dir.join("bucket");
        let _ = std::fs::remove_file(&marker);
        assert_eq!(pair(&marker, "s3://one"), Ok(()));
        assert_eq!(pair(&marker, "s3://one"), Ok(()));
        assert!(pair(&marker, "s3://two").unwrap_err().contains("must not serve s3://two"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
