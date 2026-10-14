//! An identity's face: what a page shows it by. An agent's is its agent
//! fragment's, at live: its title (`fragment.json`'s `meta.title`) and
//! its picture (`avatar.png`), which the cell reads at each install of
//! live (docs/api.md, "An agent's name and picture"). A person's picture
//! is the registry's. Both pictures are served at
//! `/api/identities/<npub>/picture` and take the same images.

use fragment_proto::limits;

/// Where an agent fragment keeps its picture: one path, whatever the
/// image's type, which its bytes tell.
pub const AGENT_PICTURE: &str = "avatar.png";

/// A picture's type from its first bytes (a request's content-type and a
/// path's extension are not trusted).
pub fn picture_type(bytes: &[u8]) -> Option<&'static str> {
    match bytes {
        [0x89, b'P', b'N', b'G', ..] => Some("image/png"),
        [0xFF, 0xD8, 0xFF, ..] => Some("image/jpeg"),
        [b'G', b'I', b'F', b'8', ..] => Some("image/gif"),
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => Some("image/webp"),
        _ => None,
    }
}

/// Why a picture is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    /// Its bytes, past `limits::PICTURE_MAX_BYTES`.
    TooLarge(u64),
    NotAnImage,
}

impl Refused {
    /// What its maker is told, `what` naming it (`avatar.png`).
    pub fn message(self, what: &str) -> String {
        match self {
            Refused::TooLarge(size) => format!(
                "{what} is {size} bytes; a picture is at most {} (256 KiB): make it smaller (512×512 or less)",
                limits::PICTURE_MAX_BYTES
            ),
            Refused::NotAnImage => format!("{what} is not a picture: a picture is a PNG, JPEG, WebP or GIF"),
        }
    }
}

/// A picture's type, or why it is refused: at most
/// `limits::PICTURE_MAX_BYTES`, and a PNG, JPEG, WebP or GIF by its bytes.
pub fn check_picture(bytes: &[u8]) -> Result<&'static str, Refused> {
    if bytes.len() > limits::PICTURE_MAX_BYTES {
        return Err(Refused::TooLarge(bytes.len() as u64));
    }
    picture_type(bytes).ok_or(Refused::NotAnImage)
}

/// Where an identity's picture is served, on the platform's origin: its
/// address names the picture's SHA-256 (its first 12 digits), so a new
/// picture is a new address.
pub fn picture_path(identity: &str, sha256: &str) -> String {
    assert!(sha256.len() >= 12 && sha256.is_char_boundary(12), "a picture is named by its SHA-256");
    format!("/api/identities/{identity}/picture?v={}", &sha256[..12])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Valid: each of the four types, by its bytes alone, up to the limit
    /// itself. Invalid: one byte past it, text, an empty file, and a
    /// RIFF that is no WebP, each saying why in words its maker acts on.
    #[test]
    fn a_picture_is_one_of_four_images_of_at_most_256_kib() {
        let png = [&[0x89, b'P', b'N', b'G', 0x0D, 0x0A][..], &[0; 10]].concat();
        assert_eq!(check_picture(&png), Ok("image/png"));
        assert_eq!(check_picture(&[0xFF, 0xD8, 0xFF, 0xE0, 0, 0]), Ok("image/jpeg"));
        assert_eq!(check_picture(b"GIF89a......"), Ok("image/gif"));
        assert_eq!(check_picture(b"RIFF\0\0\0\0WEBPVP8 "), Ok("image/webp"));
        let mut full = vec![0u8; limits::PICTURE_MAX_BYTES];
        full[..4].copy_from_slice(&[0x89, b'P', b'N', b'G']);
        assert_eq!(check_picture(&full), Ok("image/png"), "the limit itself is a picture");
        full.push(0);
        let over = check_picture(&full);
        assert_eq!(over, Err(Refused::TooLarge(limits::PICTURE_MAX_BYTES as u64 + 1)));
        let said = over.unwrap_err().message(AGENT_PICTURE);
        assert!(said.starts_with("avatar.png is 262145 bytes") && said.contains("262144"), "{said}");
        for bytes in [&b"hello, I am text"[..], b"", b"RIFF\0\0\0\0WAVEfmt "] {
            assert_eq!(check_picture(bytes), Err(Refused::NotAnImage), "{bytes:?}");
        }
        assert!(Refused::NotAnImage.message(AGENT_PICTURE).contains("PNG, JPEG, WebP or GIF"));
    }

    /// The address names the picture's hash, so a new one is a new address.
    #[test]
    fn a_pictures_address_names_its_hash() {
        let sha = "0123456789abcdef".repeat(4);
        assert_eq!(picture_path("npub1x", &sha), "/api/identities/npub1x/picture?v=0123456789ab");
        assert_ne!(picture_path("npub1x", &sha), picture_path("npub1x", &"f".repeat(64)));
    }
}
