//! `cargo xtask stripe check|setup --config <file> [--branch <name>]`: a
//! deployment's Stripe as docs/billing.md wants it (finite-mono's readiness
//! audit, in Rust), on the account its config's `stripe.key_file` names:
//!
//! - its prices, by lookup key: the two seats monthly in USD at decision
//!   25's amounts, the $25 pack once (decision 55), tax-exclusive;
//! - its own portal configuration (`stripe.portal`, never the account's
//!   default, which finite-mono's audit pins): card, invoices, billing
//!   address, cancel at the period's end, no plan switching (seats are
//!   the registry's), returning to the platform's settings;
//! - its webhook endpoint at `<platform>/api/stripe/webhook`: enabled,
//!   exactly `fragment_core::stripe::EVENTS`, the pinned API version.
//!
//! `check` reads and says what is wrong, changing nothing. `setup` makes
//! what is missing, once: Products and Prices, a portal configuration (its
//! id printed, for the config), the endpoint (its signing secret written to
//! `--webhook-secret-file`, 0600, never printed: `cargo xtask secret set`
//! stores it). Live mode is Paul's to run. A branch deployment's endpoint
//! is `deploy`'s own (`branch_endpoint`, decision 58).

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use fragment_core::stripe::{self as rules, Form, API_VERSION, EVENTS, PACK_CENTS, PACK_LOOKUP_KEY};
use fragment_proto::org::SeatKind;
use serde_json::Value;

/// A Stripe account, as xtask calls it.
pub struct Stripe {
    api: String,
    key: String,
    http: reqwest::blocking::Client,
}

/// The Stripe Tax code of a seat and a pack: software as a service.
const TAX_CODE: &str = "txcd_10103001";

impl Stripe {
    pub fn new(api: &str, key: &str) -> Result<Stripe> {
        Ok(Stripe { api: api.trim_end_matches('/').to_string(), key: key.to_string(), http: reqwest::blocking::Client::builder().timeout(Duration::from_secs(30)).build()? })
    }

    fn answer(&self, req: reqwest::blocking::RequestBuilder, what: &str) -> Result<Value> {
        let r = req.bearer_auth(&self.key).header("stripe-version", API_VERSION).send().with_context(|| format!("Stripe: {what}"))?;
        let status = r.status();
        let v: Value = r.json().unwrap_or(Value::Null);
        if !status.is_success() {
            bail!("Stripe refused {what} ({status}): {}", v["error"]["message"].as_str().unwrap_or("no reason given"));
        }
        Ok(v)
    }

    fn get(&self, path: &str) -> Result<Value> {
        self.answer(self.http.get(format!("{}{path}", self.api)), path)
    }

    fn post(&self, path: &str, form: &Form, idempotency: &str) -> Result<Value> {
        let req = self.http.post(format!("{}{path}", self.api)).header("content-type", "application/x-www-form-urlencoded").header("idempotency-key", idempotency).body(form.encode());
        self.answer(req, path)
    }

    fn delete(&self, path: &str) -> Result<Value> {
        self.answer(self.http.delete(format!("{}{path}", self.api)), path)
    }

    /// The active price a lookup key names, if any.
    fn price(&self, key: &str) -> Result<Option<Value>> {
        let v = self.get(&format!("/v1/prices?lookup_keys[]={key}&active=true"))?;
        Ok(v["data"].as_array().and_then(|d| d.first().cloned()))
    }

    /// The endpoints at `url` (the account's first page: an account has a
    /// handful).
    fn endpoints_at(&self, url: &str) -> Result<Vec<Value>> {
        let v = self.get("/v1/webhook_endpoints?limit=100")?;
        Ok(v["data"].as_array().into_iter().flatten().filter(|e| e["url"] == url).cloned().collect())
    }
}

/// A price fragment sells: its lookup key, its amount, and whether it
/// recurs monthly.
struct Wanted {
    key: &'static str,
    cents: i64,
    monthly: bool,
    product: &'static str,
}

fn wanted() -> [Wanted; 3] {
    [
        Wanted { key: rules::lookup_key(SeatKind::Seat), cents: rules::unit_amount(SeatKind::Seat), monthly: true, product: "Fragment seat" },
        Wanted { key: rules::lookup_key(SeatKind::SeatAlwaysOn), cents: rules::unit_amount(SeatKind::SeatAlwaysOn), monthly: true, product: "Fragment always-on seat" },
        Wanted { key: PACK_LOOKUP_KEY, cents: PACK_CENTS, monthly: false, product: "Fragment credit, $25" },
    ]
}

/// The webhook endpoint's URL for a platform's origin.
pub fn endpoint_url(platform: &str) -> String {
    format!("{}/api/stripe/webhook", platform.trim_end_matches('/'))
}

/// What is wrong with the deployment's Stripe (none: it is as it should be).
pub fn check(s: &Stripe, platform: &str, portal: Option<&str>) -> Result<Vec<String>> {
    let mut wrong = vec![];
    for w in wanted() {
        match s.price(w.key)? {
            None => wrong.push(format!("no active price {} (cargo xtask stripe setup makes it)", w.key)),
            Some(p) => {
                if p["unit_amount"].as_i64() != Some(w.cents) || p["currency"] != "usd" {
                    wrong.push(format!("{} is {} {}, not {} usd cents", w.key, p["unit_amount"], p["currency"], w.cents));
                }
                if w.monthly != (p["recurring"]["interval"] == "month" && p["recurring"]["interval_count"] == 1) {
                    wrong.push(format!("{} {} monthly", w.key, if w.monthly { "is not" } else { "should not be" }));
                }
                if p["tax_behavior"] != "exclusive" {
                    wrong.push(format!("{}'s tax behavior is {}, not exclusive (prices before tax: decision 54)", w.key, p["tax_behavior"]));
                }
            }
        }
    }
    match portal {
        None => wrong.push("the config names no stripe.portal (cargo xtask stripe setup makes one)".into()),
        Some(id) => {
            let c = s.get(&format!("/v1/billing_portal/configurations/{id}"))?;
            let f = &c["features"];
            let checks = [
                (c["active"] == true, "is not active"),
                (c["is_default"] != true, "is the account's default (finite-mono's): fragment's is its own"),
                (f["subscription_update"]["enabled"] == false, "lets a subscription change plans (seats are the registry's)"),
                (f["subscription_cancel"]["enabled"] == true && f["subscription_cancel"]["mode"] == "at_period_end", "does not cancel at the period's end"),
                (f["payment_method_update"]["enabled"] == true, "does not update the card"),
                (f["invoice_history"]["enabled"] == true, "shows no invoices"),
                (f["customer_update"]["enabled"] == true, "does not update the billing address"),
                (c["default_return_url"] == format!("{}/settings", platform.trim_end_matches('/')).as_str(), "does not return to the platform's settings"),
            ];
            wrong.extend(checks.iter().filter(|(ok, _)| !ok).map(|(_, why)| format!("the portal configuration {id} {why}")));
        }
    }
    let url = endpoint_url(platform);
    let at = s.endpoints_at(&url)?;
    match at.as_slice() {
        [] => wrong.push(format!("no webhook endpoint at {url} (cargo xtask stripe setup makes it)")),
        [e] => {
            let mut events: Vec<&str> = e["enabled_events"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
            events.sort_unstable();
            let mut want = EVENTS.to_vec();
            want.sort_unstable();
            if events != want {
                wrong.push(format!("the endpoint at {url} takes {events:?}, not exactly {want:?}"));
            }
            if e["status"] != "enabled" {
                wrong.push(format!("the endpoint at {url} is {}", e["status"]));
            }
            if e["api_version"] != API_VERSION {
                wrong.push(format!("the endpoint at {url} sends API version {}, not {API_VERSION}", e["api_version"]));
            }
        }
        more => wrong.push(format!("{} webhook endpoints at {url}: one only", more.len())),
    }
    Ok(wrong)
}

/// What `setup` made.
#[derive(Debug, Default)]
pub struct Made {
    pub prices: Vec<String>,
    pub portal: Option<String>,
    /// The endpoint made, and where its secret was written.
    pub endpoint: Option<(String, PathBuf)>,
}

fn endpoint_form(url: &str, platform: &str) -> Form {
    let mut f = Form::new().push("url", url).push("api_version", API_VERSION).push("description", "fragment's billing (docs/billing.md)").push(format!("metadata[{}]", rules::META_DEPLOYMENT), platform);
    for e in EVENTS {
        f = f.push("enabled_events[]", e);
    }
    f
}

/// Makes what is missing (module docs). `secret_file` takes a new
/// endpoint's signing secret; with none, an endpoint is not made.
pub fn setup(s: &Stripe, platform: &str, portal: Option<&str>, secret_file: Option<&Path>) -> Result<Made> {
    let mut made = Made::default();
    for w in wanted() {
        if s.price(w.key)?.is_some() {
            continue;
        }
        let product = s.post(
            "/v1/products",
            &Form::new().push("name", w.product).push("tax_code", TAX_CODE).push(format!("metadata[{}]", rules::META_DEPLOYMENT), "fragment"),
            &format!("fragment-product-{}", w.key),
        )?;
        let mut f = Form::new()
            .push("product", product["id"].as_str().context("a product has an id")?)
            .push("unit_amount", w.cents.to_string())
            .push("currency", "usd")
            .push("lookup_key", w.key)
            .push("tax_behavior", "exclusive");
        if w.monthly {
            f = f.push("recurring[interval]", "month");
        }
        s.post("/v1/prices", &f, &format!("fragment-price-{}", w.key))?;
        made.prices.push(w.key.to_string());
    }
    if portal.is_none() {
        let f = Form::new()
            .push("default_return_url", format!("{}/settings", platform.trim_end_matches('/')))
            .push("features[subscription_update][enabled]", "false")
            .push("features[subscription_cancel][enabled]", "true")
            .push("features[subscription_cancel][mode]", "at_period_end")
            .push("features[payment_method_update][enabled]", "true")
            .push("features[invoice_history][enabled]", "true")
            .push("features[customer_update][enabled]", "true")
            .push("features[customer_update][allowed_updates][]", "address")
            .push("features[customer_update][allowed_updates][]", "email")
            .push("features[customer_update][allowed_updates][]", "name")
            .push(format!("metadata[{}]", rules::META_DEPLOYMENT), platform);
        let c = s.post("/v1/billing_portal/configurations", &f, &format!("fragment-portal-{}", platform.trim_end_matches('/')))?;
        made.portal = c["id"].as_str().map(str::to_string);
    }
    let url = endpoint_url(platform);
    if s.endpoints_at(&url)?.is_empty() {
        if let Some(file) = secret_file {
            let e = s.post("/v1/webhook_endpoints", &endpoint_form(&url, platform), &format!("fragment-endpoint-{url}"))?;
            let secret = e["secret"].as_str().context("a new endpoint answers its secret")?;
            write_secret(file, secret)?;
            made.endpoint = Some((e["id"].as_str().unwrap_or("").to_string(), file.to_path_buf()));
        }
    }
    Ok(made)
}

/// A branch deployment's endpoint (decision 58): any at its URL removed and
/// one made, its signing secret answered for the deploy to upload as a
/// Worker secret. Stripe gives a secret only when it makes an endpoint, so
/// every deploy of a branch makes its own (an event in flight to the one
/// before is the reconcile's).
pub fn branch_endpoint(s: &Stripe, platform: &str) -> Result<String> {
    let url = endpoint_url(platform);
    for e in s.endpoints_at(&url)? {
        s.delete(&format!("/v1/webhook_endpoints/{}", e["id"].as_str().context("an endpoint has an id")?))?;
    }
    let attempt = fragment_devstack::random_hex(8);
    let e = s.post("/v1/webhook_endpoints", &endpoint_form(&url, platform), &format!("fragment-endpoint-{attempt}"))?;
    Ok(e["secret"].as_str().context("a new endpoint answers its secret")?.to_string())
}

fn write_secret(file: &Path, secret: &str) -> Result<()> {
    if file.exists() {
        bail!("{} exists: a secret file is never overwritten", file.display());
    }
    std::fs::write(file, format!("{secret}\n")).with_context(|| format!("write {}", file.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "sk_test_xtask";
    const PLATFORM: &str = "https://p5.finite.place";

    #[test]
    fn setup_makes_what_check_wants_and_check_then_finds_nothing_wrong() {
        let fake = fragment_fakes::stripe::Stripe::start_with(0, KEY, false).unwrap();
        let s = Stripe::new(&fake.url, KEY).unwrap();
        let before = check(&s, PLATFORM, None).unwrap();
        assert!(before.iter().any(|w| w.contains("no active price fragment_seat_month")), "{before:?}");
        assert!(before.iter().any(|w| w.contains("no webhook endpoint")), "{before:?}");
        let dir = std::env::temp_dir().join(format!("xtask-stripe-{}", fragment_devstack::random_hex(4)));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("webhook-secret");
        let made = setup(&s, PLATFORM, None, Some(&file)).unwrap();
        assert_eq!(made.prices.len(), 3);
        let portal = made.portal.clone().expect("a portal configuration made");
        let secret = std::fs::read_to_string(&file).unwrap();
        assert!(secret.starts_with("whsec_"), "the secret is in its file");
        let after = check(&s, PLATFORM, Some(&portal)).unwrap();
        assert!(after.is_empty(), "{after:?}");
        // again: nothing made twice, and a secret file is never overwritten
        let again = setup(&s, PLATFORM, Some(&portal), Some(&file)).unwrap();
        assert!(again.prices.is_empty() && again.portal.is_none() && again.endpoint.is_none(), "{again:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn check_names_a_portal_that_is_not_fragments_and_two_endpoints() {
        let fake = fragment_fakes::stripe::Stripe::start(0, KEY).unwrap();
        let s = Stripe::new(&fake.url, KEY).unwrap();
        let url = endpoint_url(PLATFORM);
        for _ in 0..2 {
            s.post("/v1/webhook_endpoints", &Form::new().push("url", url.as_str()).push("enabled_events[]", "*"), &fragment_devstack::random_hex(4)).unwrap();
        }
        let loose = s.post("/v1/billing_portal/configurations", &Form::new().push("features[subscription_update][enabled]", "true"), "p").unwrap();
        let wrong = check(&s, PLATFORM, loose["id"].as_str()).unwrap();
        assert!(wrong.iter().any(|w| w.contains("2 webhook endpoints")), "{wrong:?}");
        assert!(wrong.iter().any(|w| w.contains("lets a subscription change plans")), "{wrong:?}");
        assert!(wrong.iter().any(|w| w.contains("does not return to the platform's settings")), "{wrong:?}");
    }

    #[test]
    fn a_branch_endpoint_is_made_anew_each_deploy() {
        let fake = fragment_fakes::stripe::Stripe::start(0, KEY).unwrap();
        let s = Stripe::new(&fake.url, KEY).unwrap();
        let first = branch_endpoint(&s, PLATFORM).unwrap();
        let second = branch_endpoint(&s, PLATFORM).unwrap();
        assert_ne!(first, second, "a new endpoint, a new secret");
        let at: Vec<_> = fake.endpoints().into_iter().filter(|e| e["url"] == endpoint_url(PLATFORM).as_str()).collect();
        assert_eq!(at.len(), 1, "the one before is removed");
        let mut events: Vec<&str> = at[0]["enabled_events"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
        events.sort_unstable();
        let mut want = EVENTS.to_vec();
        want.sort_unstable();
        assert_eq!(events, want);
    }
}
