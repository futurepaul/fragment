//! The deployment's own secrets as its Workers read them: Cloudflare
//! Secrets Store secrets, each bound to the Worker under a name fixed here
//! (docs/secrets.md). A deployment's config names the store secret behind
//! each binding; `cargo xtask deploy` writes the bindings, and `cargo xtask
//! secret` sets the values. Nothing here holds a value: the cell
//! (cell/src/keys.rs) and the agents' Worker (agent/src/keys.rs) read them,
//! through `Cache`.
//!
//! The names are new at the move to the store (2026-10-05), so none is the
//! name of a Worker secret an earlier deploy uploaded: such a secret, still
//! on the Worker, is never read, and never collides with a binding.

use crate::catalog;

/// The host secret: seals every value at rest (seal.rs), and the key
/// computers' placeholders are tagged with is derived from it.
pub const HOST_SECRET: &str = "HOST_SECRET";
/// The host secret before a rotation: values it sealed still open, and come
/// back resealed under `HOST_SECRET`. Bound only while a rotation runs.
pub const HOST_SECRET_PREVIOUS: &str = "HOST_SECRET_PREVIOUS";
/// The code.storage org's signing key (PKCS#8 P-256, in PEM).
pub const CODESTORAGE_KEY: &str = "CODESTORAGE_KEY";
/// The WorkOS environment sign-in and Pipes use: its client id and API key.
pub const WORKOS_CLIENT: &str = "WORKOS_CLIENT";
pub const WORKOS_KEY: &str = "WORKOS_KEY";
/// What an operator key's binding starts with (`operator_key`).
pub const OPERATOR_KEY_PREFIX: &str = "OPERATOR_KEY_";

/// How long a value read from the store is used before it is read again:
/// a value changed in the store (`cargo xtask secret set`) reaches every
/// warm isolate within this, with no deploy.
pub const CACHE_MS_MAX: i64 = 60_000;
/// The bindings one Worker has: the five above and an operator key per
/// provider of the catalog, at most.
pub const CACHE_ENTRIES_MAX: usize = 5 + catalog::PROVIDERS_MAX;

/// The binding that holds operator key `provider` (`perplexity` →
/// `OPERATOR_KEY_PERPLEXITY`, `google-places` → `OPERATOR_KEY_GOOGLE_PLACES`).
pub fn operator_key(provider: &str) -> String {
    assert!(catalog::valid_name(provider), "a provider's name is checked before its key is bound");
    let binding = format!("{OPERATOR_KEY_PREFIX}{}", provider.to_ascii_uppercase().replace('-', "_"));
    assert!(binding.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_'), "a binding is an identifier");
    binding
}

struct Entry {
    binding: String,
    value: String,
    read_ms: i64,
}

/// What each binding read last, and when: one per isolate (its clock is
/// the caller's).
///
/// A cache, so its contract. Source: the deployment's Secrets Store.
/// Invalidation: time alone; an entry read `CACHE_MS_MAX` ago or more is
/// read again, so a value rotated in the store is in use everywhere within
/// a minute. Stale reads: for up to `CACHE_MS_MAX` after a rotation an
/// isolate still uses the value before it, which every secret here allows
/// (an operator key's or code.storage's old value works until its vendor
/// revokes it; the host secret rotates by name, through a deploy). A clock
/// that runs backwards makes an entry stale, never fresher.
pub struct Cache {
    entries: Vec<Entry>,
}

impl Cache {
    pub const fn new() -> Cache {
        Cache { entries: Vec::new() }
    }

    /// `binding`'s value, when it was read less than `CACHE_MS_MAX` before `now_ms`.
    pub fn fresh(&self, binding: &str, now_ms: i64) -> Option<&str> {
        let entry = self.entries.iter().find(|e| e.binding == binding)?;
        let age_ms = now_ms - entry.read_ms;
        let fresh = (0..CACHE_MS_MAX).contains(&age_ms);
        if fresh {
            Some(entry.value.as_str())
        } else {
            None
        }
    }

    /// `value`, read for `binding` at `now_ms`, in place of what it held.
    pub fn put(&mut self, binding: &str, value: String, now_ms: i64) {
        assert!(!binding.is_empty() && !value.is_empty(), "a cached secret is named and holds a value");
        match self.entries.iter_mut().find(|e| e.binding == binding) {
            Some(entry) => {
                entry.value = value;
                entry.read_ms = now_ms;
            }
            None => {
                // bounded: a Worker has at most CACHE_ENTRIES_MAX bindings,
                // and each is one entry; more is a binding name made up at
                // run time, which none is
                assert!(self.entries.len() < CACHE_ENTRIES_MAX, "at most {CACHE_ENTRIES_MAX} secrets are bound to a Worker");
                self.entries.push(Entry { binding: binding.to_string(), value, read_ms: now_ms });
            }
        }
        assert!(self.fresh(binding, now_ms).is_some(), "what was put reads back, fresh");
    }
}

impl Default for Cache {
    fn default() -> Cache {
        Cache::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The window: a value read at t is used until t + 60 s, read again
    /// from then on, and a value read again replaces the one before. Method:
    /// a cache driven by a clock the test holds.
    #[test]
    fn a_value_is_used_for_at_most_a_minute() {
        let mut cache = Cache::new();
        assert_eq!(cache.fresh(HOST_SECRET, 1_000), None, "nothing read, nothing cached");
        cache.put(HOST_SECRET, "first".into(), 1_000);
        assert_eq!(cache.fresh(HOST_SECRET, 1_000), Some("first"));
        assert_eq!(cache.fresh(HOST_SECRET, 1_000 + CACHE_MS_MAX - 1), Some("first"), "fresh until the minute is out");
        assert_eq!(cache.fresh(HOST_SECRET, 1_000 + CACHE_MS_MAX), None, "read again once it is");
        assert_eq!(cache.fresh(HOST_SECRET, 1_000 + 10 * CACHE_MS_MAX), None);
        assert_eq!(cache.fresh(WORKOS_KEY, 1_000), None, "each binding is its own");

        // a rotated value, read again, is the one used from then on
        cache.put(HOST_SECRET, "second".into(), 1_000 + CACHE_MS_MAX);
        assert_eq!(cache.fresh(HOST_SECRET, 1_000 + CACHE_MS_MAX + 1), Some("second"));
    }

    /// A clock that steps back (an isolate's clock is the request's) makes
    /// an entry stale: it is read again, never trusted for longer.
    #[test]
    fn a_clock_that_runs_backwards_reads_again() {
        let mut cache = Cache::new();
        cache.put(CODESTORAGE_KEY, "pem".into(), 50_000);
        assert_eq!(cache.fresh(CODESTORAGE_KEY, 49_999), None);
        assert_eq!(cache.fresh(CODESTORAGE_KEY, 50_000), Some("pem"));
    }

    /// Every binding a Worker can have fits; one more is a bug.
    #[test]
    fn the_cache_holds_every_binding_and_no_more() {
        let mut cache = Cache::new();
        for i in 0..CACHE_ENTRIES_MAX {
            cache.put(&format!("B{i}"), "v".into(), 0);
        }
        // putting one already there again stays within the bound
        cache.put("B0", "w".into(), 1);
        assert_eq!(cache.fresh("B0", 1), Some("w"));
        let over = std::panic::catch_unwind(move || cache.put("ONE_MORE", "v".into(), 0));
        assert!(over.is_err(), "a binding past the bound is refused");
    }

    #[test]
    fn an_operator_keys_binding_is_its_name_in_capitals() {
        assert_eq!(operator_key("perplexity"), "OPERATOR_KEY_PERPLEXITY");
        assert_eq!(operator_key("google-places"), "OPERATOR_KEY_GOOGLE_PLACES");
        assert!(std::panic::catch_unwind(|| operator_key("Not A Name")).is_err());
        // none is a name an earlier deploy's Worker secret had
        for b in [HOST_SECRET, HOST_SECRET_PREVIOUS, CODESTORAGE_KEY, WORKOS_CLIENT, WORKOS_KEY, &operator_key("xai")] {
            assert!(!["FRAGMENT_HOST_SECRET", "FRAGMENT_HOST_SECRET_PREVIOUS", "CODESTORAGE_PRIVATE_KEY", "WORKOS_API_KEY", "WORKOS_CLIENT_ID", "FRAGMENT_KEY_XAI"].contains(&b), "{b}");
        }
    }
}
