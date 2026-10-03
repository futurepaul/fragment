//! The Node.js release the repo's JavaScript runs on, pinned: its version
//! and the SHA-256 of each platform's official tarball. Nothing else lives
//! in this file, so CI's cache of the unpacked Node (`target/tools`) keys
//! on it alone. Moving the pin: README.md, "The pinned Node".

/// Node 24 ("Krypton"), the Active LTS line since 2025-10-28 and supported
/// to 2028-04-30 (nodejs/Release's schedule): wrangler 4.145 asks for 22
/// or later (its `engines`); 22 has been in maintenance since 2025-10-21
/// and ends 2027-04-30; 26 is no LTS line before 2026-10-28.
pub const NODE_VERSION: &str = "24.21.0";

/// The majors an override (`FRAGMENT_NODE`) may be: the LTS lines from
/// wrangler's minimum to the pin's.
pub const OVERRIDE_MAJORS: [u32; 2] = [22, 24];

/// One platform's official tarball: `node-v<NODE_VERSION>-<platform>.tar.gz`
/// at https://nodejs.org/dist/v<NODE_VERSION>/.
#[derive(Debug, PartialEq, Eq)]
pub struct Tarball {
    /// Node's name for the platform (`darwin-arm64`), as in the file name.
    pub platform: &'static str,
    /// Its line in that release's SHASUMS256.txt.
    pub sha256: &'static str,
}

/// From https://nodejs.org/dist/v24.21.0/SHASUMS256.txt, whose signature
/// (SHASUMS256.txt.sig) checked good against the releaser's key
/// 5BE8A3F6C8A5C01D106C0AD820B1A390B168D356, one nodejs/node's README lists.
pub const TARBALLS: [Tarball; 4] = [
    Tarball { platform: "darwin-arm64", sha256: "bed7eea5325e1108f32ce5228ddd6a5f0f08a499ee42aa7442aea583702f6057" },
    Tarball { platform: "darwin-x64", sha256: "1462cb3b3046b815cf8ea436d3da450ec1a9f11dac7e5a46b0ada5305d7e8097" },
    Tarball { platform: "linux-arm64", sha256: "724282c3b43aec998aa9527380465b45d229e021b58035f5f4f63095eabfe5d5" },
    Tarball { platform: "linux-x64", sha256: "6e1db87ef58b8819e5d5402eff1536491b18edd8eb7bee5ef7897876e88dc5ff" },
];
