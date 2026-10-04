//! The box's private CA, made once (rcgen), and the zone's certificate,
//! issued from it at each start.
//!
//! - **The root** is constrained to the zone (RFC 5280 name constraints,
//!   critical): a device that trusts it trusts it for `<zone>` and the names
//!   under it, never for anyone else's. Its key (`ca.key`, mode 0600) never
//!   leaves the state directory; a key readable by others is refused.
//! - **The zone's certificate** names the zone and `*.<zone>` (every
//!   fragment, `dex`, a computer's own origin), for serverAuth, valid
//!   `LEAF_DAYS`: inside iOS's 825 days for a certificate from a root its
//!   owner installed. Nothing pins it, so each start issues a fresh one.
//! - **What devices install**: `fragment-ca.mobileconfig` (iOS and macOS: a
//!   `com.apple.security.root` payload), `fragment-ca.crt` (DER) and
//!   `ca.pem`, served over plain http by the front door (door.rs).

use std::fmt;
use std::fs;
use std::io::Write as _;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use base64::Engine as _;
use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, GeneralSubtree, IsCa, Issuer, KeyPair, KeyUsagePurpose, NameConstraints, SerialNumber,
};
use sha2::{Digest, Sha256};
use time::{Duration, OffsetDateTime};

/// The root's key: PKCS#8 PEM, mode 0600.
pub const CA_KEY: &str = "ca.key";
/// The root, PEM: what `CELLD_EXTRA_CA_FILE` and `curl --cacert` take.
pub const CA_PEM: &str = "ca.pem";
/// The root, DER: what macOS's Keychain Access and `security` take.
pub const CA_CRT: &str = "fragment-ca.crt";
/// The root as an Apple configuration profile (iOS, macOS).
pub const PROFILE: &str = "fragment-ca.mobileconfig";
/// The zone the root is constrained to, beside it.
pub const CA_ZONE: &str = "ca.zone";
/// The zone's certificate (PEM) and its key (PKCS#8 PEM, mode 0600).
pub const LEAF_PEM: &str = "zone.pem";
pub const LEAF_KEY: &str = "zone.key";

/// How long the root is valid.
pub const CA_DAYS: i64 = 3650;
/// How long the zone's certificate is valid: Chrome's 398-day rule for
/// public roots, and well inside iOS's limit.
pub const LEAF_DAYS: i64 = 397;
/// iOS refuses a server certificate valid longer than this (iOS 13 and
/// later; Apple's "Requirements for trusted certificates").
pub const IOS_MAX_DAYS: i64 = 825;
/// Certificates are dated this far back, for a device whose clock lags.
const BACKDATE: Duration = Duration::hours(1);

/// Why the CA or the zone's certificate could not be made or read.
#[derive(Debug)]
pub enum CaError {
    Io { path: PathBuf, error: std::io::Error },
    /// A key file others may read: it is refused rather than used.
    KeyMode { path: PathBuf, mode: u32 },
    /// The root on disk is constrained to another zone.
    ZoneMismatch { dir: PathBuf, have: String, want: String },
    /// Some of the root's files are there and some are not.
    Partial { dir: PathBuf, missing: &'static str },
    Cert(rcgen::Error),
}

impl fmt::Display for CaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CaError::Io { path, error } => write!(f, "{}: {error}", path.display()),
            CaError::KeyMode { path, mode } => write!(f, "{} is mode {mode:o}: a key must be readable by its owner alone (chmod 600 it)", path.display()),
            CaError::ZoneMismatch { dir, have, want } => write!(
                f,
                "the CA in {} is for {have}, not {want}: name another state directory (FRAGMENT_LAN_STATE), or remove that one and install the new root on every device",
                dir.display()
            ),
            CaError::Partial { dir, missing } => write!(f, "{} holds part of a CA ({missing} is missing): restore it, or remove the directory to make a new one", dir.display()),
            CaError::Cert(e) => write!(f, "certificate: {e}"),
        }
    }
}

impl std::error::Error for CaError {}

impl From<rcgen::Error> for CaError {
    fn from(e: rcgen::Error) -> Self {
        CaError::Cert(e)
    }
}

fn io(path: &Path) -> impl FnOnce(std::io::Error) -> CaError + '_ {
    move |error| CaError::Io { path: path.to_path_buf(), error }
}

/// The root, as the state directory holds it.
pub struct Ca {
    pub dir: PathBuf,
    pub zone: String,
    pub pem: String,
    pub der: Vec<u8>,
    key: KeyPair,
}

impl Ca {
    /// The root's SHA-256 over its DER, as `openssl x509 -fingerprint
    /// -sha256` prints it (colon-separated, upper case): what a person
    /// compares on a device before trusting it.
    pub fn fingerprint(&self) -> String {
        Sha256::digest(&self.der).iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(":")
    }

    /// The root's PEM file.
    pub fn pem_file(&self) -> PathBuf {
        self.dir.join(CA_PEM)
    }
}

/// The zone's certificate and key, as files.
pub struct Leaf {
    pub cert_file: PathBuf,
    pub key_file: PathBuf,
    pub not_after: OffsetDateTime,
}

/// The CA in `dir` for `zone`: made there on first use (`dir` 0700, the
/// key 0600), else read and checked. `name` is what devices show it as.
pub fn ensure_ca(dir: &Path, zone: &str, name: &str) -> Result<Ca, CaError> {
    assert!(crate::valid_zone(zone), "a valid zone");
    fs::create_dir_all(dir).map_err(io(dir))?;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).map_err(io(dir))?;
    let (key_path, pem_path, zone_path) = (dir.join(CA_KEY), dir.join(CA_PEM), dir.join(CA_ZONE));
    let ca = match (key_path.exists(), pem_path.exists(), zone_path.exists()) {
        (false, false, false) => make_ca(dir, zone, name)?,
        (true, true, true) => read_ca(dir, zone)?,
        (key, pem, _) => return Err(CaError::Partial { dir: dir.to_path_buf(), missing: if !key { CA_KEY } else if !pem { CA_PEM } else { CA_ZONE } }),
    };
    // what devices download is written from the root every time
    write_public(&dir.join(CA_CRT), &ca.der)?;
    write_public(&dir.join(PROFILE), mobileconfig(&ca.der, zone, name).as_bytes())?;
    Ok(ca)
}

fn make_ca(dir: &Path, zone: &str, name: &str) -> Result<Ca, CaError> {
    let key = KeyPair::generate()?;
    let mut params = CertificateParams::new(Vec::<String>::new())?;
    params.distinguished_name = rcgen::DistinguishedName::new();
    params.distinguished_name.push(DnType::CommonName, name);
    params.distinguished_name.push(DnType::OrganizationName, "fragment");
    // it signs end-entity certificates only
    params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign, KeyUsagePurpose::DigitalSignature];
    params.name_constraints = Some(NameConstraints { permitted_subtrees: vec![GeneralSubtree::DnsName(zone.to_string())], excluded_subtrees: vec![] });
    let now = OffsetDateTime::now_utc();
    params.not_before = now - BACKDATE;
    params.not_after = now + Duration::days(CA_DAYS);
    params.serial_number = Some(serial());
    let cert = params.self_signed(&key)?;
    write_key(&dir.join(CA_KEY), &key.serialize_pem())?;
    write_public(&dir.join(CA_PEM), cert.pem().as_bytes())?;
    write_public(&dir.join(CA_ZONE), format!("{zone}\n").as_bytes())?;
    Ok(Ca { dir: dir.to_path_buf(), zone: zone.to_string(), pem: cert.pem(), der: cert.der().to_vec(), key })
}

fn read_ca(dir: &Path, zone: &str) -> Result<Ca, CaError> {
    let zone_path = dir.join(CA_ZONE);
    let have = fs::read_to_string(&zone_path).map_err(io(&zone_path))?.trim().to_string();
    if have != zone {
        return Err(CaError::ZoneMismatch { dir: dir.to_path_buf(), have, want: zone.to_string() });
    }
    let key = KeyPair::from_pem(&read_key(&dir.join(CA_KEY))?)?;
    let pem_path = dir.join(CA_PEM);
    let pem = fs::read_to_string(&pem_path).map_err(io(&pem_path))?;
    let der = pem_der(&pem).ok_or_else(|| CaError::Io { path: pem_path.clone(), error: std::io::Error::other("no CERTIFICATE in it") })?;
    Ok(Ca { dir: dir.to_path_buf(), zone: zone.to_string(), pem, der, key })
}

/// A fresh certificate for the zone and `*.<zone>`, from `ca`, written to
/// `zone.pem` and `zone.key` (0600) in its directory.
pub fn issue_leaf(ca: &Ca) -> Result<Leaf, CaError> {
    let issuer = Issuer::from_ca_cert_pem(&ca.pem, &ca.key)?;
    let key = KeyPair::generate()?;
    let mut params = CertificateParams::new(vec![ca.zone.clone(), format!("*.{}", ca.zone)])?;
    params.distinguished_name = rcgen::DistinguishedName::new();
    params.distinguished_name.push(DnType::CommonName, ca.zone.as_str());
    params.is_ca = IsCa::ExplicitNoCa;
    params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    params.use_authority_key_identifier_extension = true;
    let now = OffsetDateTime::now_utc();
    params.not_before = now - BACKDATE;
    params.not_after = now + Duration::days(LEAF_DAYS);
    assert!(params.not_after - params.not_before <= Duration::days(IOS_MAX_DAYS), "inside iOS's limit");
    params.serial_number = Some(serial());
    let cert = params.signed_by(&key, &issuer)?;
    let (cert_file, key_file) = (ca.dir.join(LEAF_PEM), ca.dir.join(LEAF_KEY));
    // a fresh key replaces the last: written whole, then renamed over it
    let tmp = ca.dir.join(format!("{LEAF_KEY}.tmp"));
    let _ = fs::remove_file(&tmp);
    write_key(&tmp, &key.serialize_pem())?;
    fs::rename(&tmp, &key_file).map_err(io(&key_file))?;
    write_public(&cert_file, cert.pem().as_bytes())?;
    Ok(Leaf { cert_file, key_file, not_after: params.not_after })
}

/// A positive 128-bit serial from the OS.
fn serial() -> SerialNumber {
    let mut bytes = [0u8; 16];
    getrandom(&mut bytes);
    bytes[0] &= 0x7f;
    SerialNumber::from_slice(&bytes)
}

/// Random bytes from the OS.
pub(crate) fn getrandom(buf: &mut [u8]) {
    use std::io::Read as _;
    fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(buf)).expect("read /dev/urandom");
}

/// The first CERTIFICATE in a PEM text, as DER.
pub fn pem_der(pem: &str) -> Option<Vec<u8>> {
    let body = pem.split("-----BEGIN CERTIFICATE-----").nth(1)?.split("-----END CERTIFICATE-----").next()?;
    let b64: String = body.chars().filter(|c| !c.is_whitespace()).collect();
    base64::engine::general_purpose::STANDARD.decode(b64).ok()
}

/// A key file, readable by its owner alone; refused if it is not.
pub fn read_key(path: &Path) -> Result<String, CaError> {
    let meta = fs::metadata(path).map_err(io(path))?;
    let mode = meta.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(CaError::KeyMode { path: path.to_path_buf(), mode });
    }
    fs::read_to_string(path).map_err(io(path))
}

/// A new key file, mode 0600 from its creation (never another mode first).
fn write_key(path: &Path, pem: &str) -> Result<(), CaError> {
    let mut f = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path).map_err(io(path))?;
    f.write_all(pem.as_bytes()).map_err(io(path))?;
    f.sync_all().map_err(io(path))
}

/// A public file (a certificate, the profile), written whole then renamed.
fn write_public(path: &Path, bytes: &[u8]) -> Result<(), CaError> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, bytes).map_err(io(&tmp))?;
    fs::set_permissions(&tmp, fs::Permissions::from_mode(0o644)).map_err(io(&tmp))?;
    fs::rename(&tmp, path).map_err(io(path))
}

/// A UUID-shaped name derived from `seed` (version 5's layout, SHA-256
/// in place of SHA-1): the same root gives the same profile identifiers,
/// so installing it again replaces it.
fn uuid(seed: &[u8]) -> String {
    let h = Sha256::digest(seed);
    let mut b = [0u8; 16];
    b.copy_from_slice(&h[..16]);
    b[6] = (b[6] & 0x0f) | 0x50;
    b[8] = (b[8] & 0x3f) | 0x80;
    let hex: String = b.iter().map(|x| format!("{x:02X}")).collect();
    format!("{}-{}-{}-{}-{}", &hex[..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..])
}

/// XML's five escapes, for a profile's strings.
fn xml(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&apos;")
}

/// The root as an Apple configuration profile: one `com.apple.security.root`
/// payload. iOS installs it from Settings (General, VPN & Device
/// Management), then trusts it for TLS once its owner turns it on in
/// Certificate Trust Settings.
pub fn mobileconfig(der: &[u8], zone: &str, name: &str) -> String {
    let data = base64::engine::general_purpose::STANDARD.encode(der);
    let lines: Vec<&str> = data.as_bytes().chunks(64).map(|c| std::str::from_utf8(c).expect("base64 is ASCII")).collect();
    let id = zone.split('.').rev().collect::<Vec<_>>().join(".");
    let (payload_uuid, profile_uuid) = (uuid(&[b"payload:".as_slice(), der].concat()), uuid(&[b"profile:".as_slice(), der].concat()));
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>PayloadContent</key>
	<array>
		<dict>
			<key>PayloadCertificateFileName</key>
			<string>{CA_CRT}</string>
			<key>PayloadContent</key>
			<data>
			{data}
			</data>
			<key>PayloadDescription</key>
			<string>The root certificate of {zone_x}, valid for {zone_x} and the names under it only.</string>
			<key>PayloadDisplayName</key>
			<string>{name_x}</string>
			<key>PayloadIdentifier</key>
			<string>{id}.ca.{payload_uuid}</string>
			<key>PayloadType</key>
			<string>com.apple.security.root</string>
			<key>PayloadUUID</key>
			<string>{payload_uuid}</string>
			<key>PayloadVersion</key>
			<integer>1</integer>
		</dict>
	</array>
	<key>PayloadDescription</key>
	<string>Trust fragment on {zone_x}, this network's intranet.</string>
	<key>PayloadDisplayName</key>
	<string>fragment on {zone_x}</string>
	<key>PayloadIdentifier</key>
	<string>{id}.profile</string>
	<key>PayloadRemovalDisallowed</key>
	<false/>
	<key>PayloadType</key>
	<string>Configuration</string>
	<key>PayloadUUID</key>
	<string>{profile_uuid}</string>
	<key>PayloadVersion</key>
	<integer>1</integer>
</dict>
</plist>
"#,
        data = lines.join("\n\t\t\t"),
        zone_x = xml(zone),
        name_x = xml(name),
        id = xml(&id),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
    use std::sync::Arc;
    use x509_parser::prelude::*;

    const ZONE: &str = "fragment.home.arpa";

    fn tmp(name: &str) -> PathBuf {
        let mut b = [0u8; 6];
        getrandom(&mut b);
        std::env::temp_dir().join(format!("fragment-lan-{name}-{}", hex::encode(b)))
    }

    /// A verifier trusting `ca` alone, as a device that installed it does.
    fn verifier(ca: &Ca) -> Arc<dyn rustls::client::danger::ServerCertVerifier> {
        let mut roots = rustls::RootCertStore::empty();
        roots.add(CertificateDer::from(ca.der.clone())).expect("the root is a trust anchor");
        rustls::client::WebPkiServerVerifier::builder_with_provider(Arc::new(roots), Arc::new(rustls::crypto::ring::default_provider())).build().expect("a verifier")
    }

    fn leaf_der(leaf: &Leaf) -> Vec<u8> {
        pem_der(&fs::read_to_string(&leaf.cert_file).unwrap()).unwrap()
    }

    // Goal: a device that trusts the root accepts the zone's certificate for
    // the zone, any one name under it (a fragment, dex, a computer), and no
    // other name.
    #[test]
    fn the_zone_certificate_verifies_for_the_zone_alone() {
        let dir = tmp("verify");
        let ca = ensure_ca(&dir, ZONE, "fragment LAN CA (test)").unwrap();
        let leaf = issue_leaf(&ca).unwrap();
        let v = verifier(&ca);
        let der = CertificateDer::from(leaf_der(&leaf));
        let now = UnixTime::now();
        for ok in [ZONE, "dex.fragment.home.arpa", "todo--paul.fragment.home.arpa", "0123456789abcdef01234567--computer.fragment.home.arpa"] {
            let name = ServerName::try_from(ok).unwrap();
            v.verify_server_cert(&der, &[], &name, &[], now).unwrap_or_else(|e| panic!("{ok}: {e}"));
        }
        for bad in ["example.com", "a.b.fragment.home.arpa", "fragment.home.arpa.evil.example", "home.arpa"] {
            let name = ServerName::try_from(bad).unwrap();
            assert!(v.verify_server_cert(&der, &[], &name, &[], now).is_err(), "{bad}");
        }
        fs::remove_dir_all(&dir).unwrap();
    }

    // Goal: the root can vouch for nothing outside the zone, even if its key
    // leaks: a certificate it signs for another name fails to verify.
    #[test]
    fn the_root_is_constrained_to_the_zone() {
        let dir = tmp("constrained");
        let ca = ensure_ca(&dir, ZONE, "fragment LAN CA (test)").unwrap();
        let issuer = Issuer::from_ca_cert_pem(&ca.pem, &ca.key).unwrap();
        let key = KeyPair::generate().unwrap();
        let mut params = CertificateParams::new(vec!["bank.example".to_string()]).unwrap();
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        let forged = params.signed_by(&key, &issuer).unwrap();
        let name = ServerName::try_from("bank.example").unwrap();
        let refused = verifier(&ca).verify_server_cert(forged.der(), &[], &name, &[], UnixTime::now());
        assert!(refused.is_err(), "a name outside the zone is refused");
        let (_, cert) = X509Certificate::from_der(&ca.der).unwrap();
        let nc = cert.name_constraints().unwrap().expect("name constraints");
        assert!(nc.critical, "the constraint is critical");
        fs::remove_dir_all(&dir).unwrap();
    }

    // Goal: what iOS checks of a server certificate from a root its owner
    // installed: DNS names in the SAN, serverAuth, at most 825 days, and a
    // P-256 key with a SHA-256 signature.
    #[test]
    fn the_zone_certificate_meets_ios_requirements() {
        let dir = tmp("ios");
        let ca = ensure_ca(&dir, ZONE, "fragment LAN CA (test)").unwrap();
        let leaf = issue_leaf(&ca).unwrap();
        let der = leaf_der(&leaf);
        let (_, cert) = X509Certificate::from_der(&der).unwrap();
        let days = (cert.validity().not_after.timestamp() - cert.validity().not_before.timestamp()) / 86_400;
        assert!(days <= IOS_MAX_DAYS, "{days} days");
        let san = cert.subject_alternative_name().unwrap().expect("a SAN");
        let names: Vec<String> = san.value.general_names.iter().map(|n| format!("{n}")).collect();
        assert_eq!(names, ["DNSName(fragment.home.arpa)", "DNSName(*.fragment.home.arpa)"]);
        let eku = cert.extended_key_usage().unwrap().expect("an EKU");
        assert!(eku.value.server_auth);
        assert_eq!(cert.signature_algorithm.algorithm, x509_parser::oid_registry::OID_SIG_ECDSA_WITH_SHA256);
        let bc = cert.basic_constraints().unwrap().expect("basic constraints");
        assert!(!bc.value.ca);
        fs::remove_dir_all(&dir).unwrap();
    }

    // Goal: the root is made once and kept (devices trust it), its key 0600
    // in a 0700 directory; each start issues a new zone certificate from it.
    #[test]
    fn the_root_is_kept_and_its_key_is_private() {
        let dir = tmp("kept");
        let first = ensure_ca(&dir, ZONE, "fragment LAN CA (test)").unwrap();
        let leaf1 = leaf_der(&issue_leaf(&first).unwrap());
        let again = ensure_ca(&dir, ZONE, "fragment LAN CA (test)").unwrap();
        assert_eq!(first.der, again.der, "the same root");
        assert_eq!(first.fingerprint(), again.fingerprint());
        let leaf2 = issue_leaf(&again).unwrap();
        assert_ne!(leaf1, leaf_der(&leaf2), "a fresh certificate");
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&dir.join(CA_KEY)), 0o600);
        assert_eq!(mode(&dir.join(LEAF_KEY)), 0o600);
        assert_eq!(mode(&dir.join(CA_PEM)), 0o644);
        assert_eq!(fs::read(dir.join(CA_CRT)).unwrap(), first.der);
        fs::remove_dir_all(&dir).unwrap();
    }

    // Goal: what is refused rather than used: a key others can read, a root
    // for another zone, and half a CA.
    #[test]
    fn a_readable_key_another_zone_and_half_a_ca_are_refused() {
        let dir = tmp("refused");
        ensure_ca(&dir, ZONE, "test").unwrap();
        fs::set_permissions(dir.join(CA_KEY), fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(ensure_ca(&dir, ZONE, "test"), Err(CaError::KeyMode { mode: 0o644, .. })));
        fs::set_permissions(dir.join(CA_KEY), fs::Permissions::from_mode(0o600)).unwrap();
        let other = ensure_ca(&dir, "corp.example", "test").err().expect("another zone is refused");
        assert!(other.to_string().contains("is for fragment.home.arpa, not corp.example"), "{other}");
        fs::remove_file(dir.join(CA_PEM)).unwrap();
        assert!(matches!(ensure_ca(&dir, ZONE, "test"), Err(CaError::Partial { missing: CA_PEM, .. })));
        fs::remove_dir_all(&dir).unwrap();
    }

    // Goal: the profile carries the root itself, as Apple's payload, and the
    // same root gives the same identifiers (installing again replaces it).
    #[test]
    fn the_profile_carries_the_root() {
        let dir = tmp("profile");
        let ca = ensure_ca(&dir, ZONE, "fragment LAN CA (omarchy) & co").unwrap();
        let profile = fs::read_to_string(dir.join(PROFILE)).unwrap();
        assert!(profile.contains("<string>com.apple.security.root</string>"));
        assert!(profile.contains("<string>arpa.home.fragment.profile</string>"));
        assert!(profile.contains("fragment LAN CA (omarchy) &amp; co"));
        let data: String = profile.split("<data>").nth(1).unwrap().split("</data>").next().unwrap().chars().filter(|c| !c.is_whitespace()).collect();
        assert_eq!(base64::engine::general_purpose::STANDARD.decode(data).unwrap(), ca.der);
        assert_eq!(profile, mobileconfig(&ca.der, ZONE, "fragment LAN CA (omarchy) & co"));
        fs::remove_dir_all(&dir).unwrap();
    }
}
