//! The browser preview cards are shot with off Cloudflare (docs/self-host.md,
//! seam 7), pinned: chrome-headless-shell from Chrome for Testing, its
//! version and the SHA-256 of each platform's zip. Nothing else lives in
//! this file, so a cache of the unpacked browser (`target/tools`) can key
//! on it alone.
//!
//! Moving the pin: the version from Chrome for Testing's
//! last-known-good-versions-with-downloads.json; each platform's zip
//! fetched, its MD5 checked against the `x-goog-hash` the bucket answers
//! for it, and its SHA-256 recorded below.

/// Chrome for Testing's Stable on 2026-10-04 (its
/// last-known-good-versions-with-downloads.json, revision 1689415).
pub const CHROME_VERSION: &str = "154.0.8037.92";

/// One platform's zip: `chrome-headless-shell-<platform>.zip` at
/// https://storage.googleapis.com/chrome-for-testing-public/<CHROME_VERSION>/<platform>/.
#[derive(Debug, PartialEq, Eq)]
pub struct Zip {
    /// Chrome for Testing's name for the platform (`mac-arm64`).
    pub platform: &'static str,
    pub sha256: &'static str,
}

/// Chrome for Testing publishes no checksums. Each hash is of the zip
/// fetched on 2026-10-04, whose MD5 matched the `x-goog-hash` Google's
/// bucket answers for it.
pub const ZIPS: [Zip; 4] = [
    Zip { platform: "linux64", sha256: "636aa5c79f2693632e9921b8bbb050038ba11672e02346c06c20f991aed096f9" },
    Zip { platform: "linux-arm64", sha256: "0ed0e47d9e9f639197f508d62ada09e5c6b4c4c60edab3160a9312a733091df6" },
    Zip { platform: "mac-arm64", sha256: "77da14e75d7f2568e6f7898d3df7cdc6faac74b15e903b2c9d486ebb6ca9b929" },
    Zip { platform: "mac-x64", sha256: "a54292aaacbb77f76f6ef47558e7c51ab884044e0adacca315567f83c060bcc4" },
];
