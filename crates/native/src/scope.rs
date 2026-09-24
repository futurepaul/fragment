//! Durable Object addresses as celld derives them (crates/celld/js.rs,
//! `namespace_key` and `durable_object_id_for_name`), so a service can check
//! "the caller is fragment `todo.paul`" against the scope the host attests.
//! celld's comments call these strings addresses that must never change.
//! Nothing asks yet: phase 6 made one per fragment host's certificate
//! request, which one wildcard certificate replaced; a custom domain's will
//! need it again.

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

fn hmac(key: &[u8; 32], input: &[u8]) -> [u8; 32] {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("HMAC takes any key length");
    mac.update(input);
    mac.finalize().into_bytes().into()
}

/// The scope of the Durable Object `class` named `name` in `script`.
pub fn of_name(script: &str, class: &str, name: &str) -> String {
    let namespace = format!("cells:v1:{}:{script}:{class}", script.len());
    let key: [u8; 32] = Sha256::digest(namespace.as_bytes()).into();
    let mut id = [0u8; 32];
    id[..16].copy_from_slice(&hmac(&key, name.as_bytes())[..16]);
    let tail = hmac(&key, &id[..16]);
    id[16..].copy_from_slice(&tail[..16]);
    format!("{class}:{}", hex::encode(id))
}

#[cfg(test)]
mod tests {
    #[test]
    fn shape() {
        let s = super::of_name("fragment", "Fragment", "todo.paul");
        assert!(s.starts_with("Fragment:") && s.len() == "Fragment:".len() + 64);
        assert_ne!(s, super::of_name("fragment", "Fragment", "todo.paula"));
        assert_ne!(s, super::of_name("other", "Fragment", "todo.paul"));
    }
}
