//! Large files (docs/MODEL.md): a file of 1 MiB or more is kept in git as
//! a pointer, and its bytes in the fleet's blob store under their SHA-256.
//! The pointer is a git-lfs v1 pointer file, so git tools recognize it for
//! what it is; the platform, not an LFS server, holds the bytes.

/// Files this large or larger are pointers.
pub const BLOB_MIN_BYTES: usize = 1024 * 1024;
/// A pointer is never larger than this, so only small files need reading to find one.
pub const POINTER_MAX_BYTES: usize = 200;
const VERSION: &str = "version https://git-lfs.github.com/spec/v1";

#[derive(Debug, Clone, PartialEq)]
pub struct Pointer {
    pub sha256: String,
    pub size: u64,
}

/// A blob's name: the SHA-256 of its bytes, lowercase hex.
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(bytes))
}

pub fn valid_sha(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The pointer file for bytes with this hash and size.
pub fn pointer(sha256: &str, size: u64) -> String {
    assert!(valid_sha(sha256), "a pointer names a sha256 in lowercase hex");
    format!("{VERSION}\noid sha256:{sha256}\nsize {size}\n")
}

/// The pointer a file's bytes are, if they are one.
pub fn parse(bytes: &[u8]) -> Option<Pointer> {
    if bytes.len() > POINTER_MAX_BYTES {
        return None;
    }
    let text = std::str::from_utf8(bytes).ok()?;
    let mut lines = text.strip_suffix('\n')?.split('\n');
    if lines.next()? != VERSION {
        return None;
    }
    let sha256 = lines.next()?.strip_prefix("oid sha256:")?.to_string();
    let size = lines.next()?.strip_prefix("size ")?.parse().ok()?;
    (valid_sha(&sha256) && lines.next().is_none()).then_some(Pointer { sha256, size })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA: &str = "4d7a214614ab2935c943f9e0ff69d22eadbb8f32b1258daaa5e2ca24d17e2393";

    #[test]
    fn round_trip() {
        let p = pointer(SHA, 12_345_678);
        assert!(p.len() <= POINTER_MAX_BYTES);
        assert_eq!(parse(p.as_bytes()), Some(Pointer { sha256: SHA.into(), size: 12_345_678 }));
    }

    #[test]
    fn not_pointers() {
        for bad in [
            "".to_string(),
            "hello".to_string(),
            pointer(SHA, 1).trim_end().to_string(),
            pointer(SHA, 1) + "extra\n",
            pointer(SHA, 1).replace("sha256:", "sha1:"),
            pointer(SHA, 1).replace(SHA, &SHA.to_uppercase()),
            pointer(SHA, 1).replace("size 1", "size x"),
        ] {
            assert_eq!(parse(bad.as_bytes()), None, "{bad:?}");
        }
    }
}
