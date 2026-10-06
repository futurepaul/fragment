//! The secrets service a self-hosted deployment reads its own secrets from
//! (docs/self-host.md, seam 12), pinned: OpenBao, its version and the
//! SHA-256 of each platform's release tarball. Nothing else lives in this
//! file, so a cache of the unpacked binary (`target/tools`) can key on it
//! alone.
//!
//! Moving the pin: the release's `checksums.txt` and its `.gpgsig`, the
//! signature checked against OpenBao's key (openbao.org/docs/install,
//! `openbao-gpg-pub-20240618.asc`, primary 66D1 5FDD 8728 7219 C8E1 5478
//! D200 CD70 2853 E6D0), then each tarball's line below.

/// OpenBao 2.7.1 (2026-10-01): the static seal (2.4) and declarative
/// self-initialization (2.4) are in it; 2.7 removed the `file` storage
/// backend, whose successor for one server is `pebbledb`.
pub const OPENBAO_VERSION: &str = "2.7.1";

/// One platform's tarball: `openbao_<OPENBAO_VERSION>_<platform>.tar.gz` at
/// https://github.com/openbao/openbao/releases/download/v<OPENBAO_VERSION>/.
#[derive(Debug, PartialEq, Eq)]
pub struct Tarball {
    /// OpenBao's name for the platform (`linux_amd64`), as in the file name.
    pub platform: &'static str,
    /// Its line in that release's checksums.txt.
    pub sha256: &'static str,
}

/// From https://github.com/openbao/openbao/releases/download/v2.7.1/checksums.txt,
/// whose signature (checksums.txt.gpgsig, by subkey E617 DCD4 065C 2AFC
/// 0B2C F7A7 BA8B C08C 0F69 1F94) checked good on 2026-10-05.
pub const TARBALLS: [Tarball; 4] = [
    Tarball { platform: "darwin_amd64", sha256: "c1d6d3e2ff72a6fc47b9efc2ed82482c30c70fc216f67f1c3b56edafd2f396e2" },
    Tarball { platform: "darwin_arm64", sha256: "15625b5f69aee5bb4578b4e76e856a2141647342b0f8e5969a8875b44e0fbf91" },
    Tarball { platform: "linux_amd64", sha256: "0e2f1ce10d124e03112b50dd2fbec6b78003783253bc3a91587938f39d1e2243" },
    Tarball { platform: "linux_arm64", sha256: "2b3807d90f224df05d4fe1fdede7f052d2227596c648a56bd291381f8fd01840" },
];
