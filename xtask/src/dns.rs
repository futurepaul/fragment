//! The DNS a deployment's routes need: each route's host proxied by
//! Cloudflare, or its Worker routes never see a request. With the config's
//! `dns_token_file` (a Cloudflare API token with DNS Edit on the zones),
//! `deploy` makes a missing record (AAAA `100::`, proxied: the placeholder
//! address Workers routes answer on) and refuses one that is not proxied.
//! It never changes or deletes a record that is there.

use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};

const API: &str = "https://api.cloudflare.com/client/v4";
/// The address a proxied record that only Workers routes answer points at.
const PLACEHOLDER: &str = "100::";
/// Records with one name a zone may hold (Cloudflare's own page size).
const PAGE: usize = 100;

/// One host a deployment's routes answer on, and the zone it is in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wanted {
    pub zone: String,
    /// The record's full name: `*.finite.place`, or an apex.
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub struct Existing {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub proxied: bool,
}

/// What `ensure` does about one wanted host.
#[derive(Debug, PartialEq, Eq)]
pub enum Plan {
    /// A proxied record is there.
    Ready,
    /// None is there: make one.
    Create,
    /// One is there that is not proxied: fixing it is a person's call.
    Refuse(String),
}

/// What to do for `name`, given the records with that name the zone has.
pub fn plan(name: &str, existing: &[Existing]) -> Plan {
    let routable = existing.iter().filter(|r| matches!(r.kind.as_str(), "A" | "AAAA" | "CNAME")).collect::<Vec<_>>();
    if routable.is_empty() {
        return Plan::Create;
    }
    if routable.iter().all(|r| r.proxied) {
        return Plan::Ready;
    }
    let kinds = routable.iter().filter(|r| !r.proxied).map(|r| r.kind.as_str()).collect::<Vec<_>>().join(", ");
    Plan::Refuse(format!("{name} has a record that is not proxied ({kinds}): Workers routes answer only proxied hosts. Proxy it (or remove it) in the dashboard, then deploy again"))
}

struct Cloudflare {
    http: reqwest::blocking::Client,
    token: String,
}

impl Cloudflare {
    /// One call's `result`, or its errors.
    fn call(&self, req: reqwest::blocking::RequestBuilder, what: &str) -> Result<Value> {
        let resp = req.bearer_auth(&self.token).send().with_context(|| format!("{what}: Cloudflare's API did not answer"))?;
        let status = resp.status();
        let body: Value = resp.json().with_context(|| format!("{what}: Cloudflare answered {status} with no JSON"))?;
        if body["success"] != true {
            let errors: Vec<String> = body["errors"].as_array().into_iter().flatten().map(|e| format!("{} ({})", e["message"].as_str().unwrap_or("?"), e["code"])).collect();
            bail!("{what}: Cloudflare answered {status}: {}", errors.join("; "));
        }
        Ok(body["result"].clone())
    }

    fn zone_id(&self, account: &str, zone: &str) -> Result<String> {
        let what = format!("finding the zone {zone}");
        let zones = self.call(self.http.get(format!("{API}/zones")).query(&[("name", zone), ("account.id", account)]), &what)?;
        let id = zones.as_array().and_then(|z| z.first()).and_then(|z| z["id"].as_str());
        id.map(str::to_string).with_context(|| format!("{what}: the token sees no zone {zone} on account {account} (it needs Zone · DNS · Edit on it)"))
    }

    fn records(&self, zone_id: &str, name: &str) -> Result<Vec<Existing>> {
        let what = format!("reading {name}'s records");
        let found = self.call(self.http.get(format!("{API}/zones/{zone_id}/dns_records")).query(&[("name", name), ("per_page", &PAGE.to_string())]), &what)?;
        serde_json::from_value(found).with_context(|| format!("{what}: an answer that is not a list of records"))
    }

    fn create(&self, zone_id: &str, name: &str) -> Result<()> {
        let record = json!({ "type": "AAAA", "name": name, "content": PLACEHOLDER, "proxied": true, "ttl": 1, "comment": "fragment (cargo xtask deploy): Workers routes only" });
        self.call(self.http.post(format!("{API}/zones/{zone_id}/dns_records")).json(&record), &format!("making {name}'s record"))?;
        Ok(())
    }
}

/// Makes sure every host in `wanted` is proxied, making the records that
/// are missing. Stops at the first it may not fix, before deploying.
pub fn ensure(token: &str, account: &str, wanted: &[Wanted]) -> Result<()> {
    let cf = Cloudflare { http: reqwest::blocking::Client::builder().timeout(Duration::from_secs(30)).build()?, token: token.to_string() };
    for w in wanted {
        let zone = cf.zone_id(account, &w.zone)?;
        match plan(&w.name, &cf.records(&zone, &w.name)?) {
            Plan::Ready => println!("dns: {} is proxied", w.name),
            Plan::Create => {
                cf.create(&zone, &w.name)?;
                println!("dns: made {} (AAAA {PLACEHOLDER}, proxied)", w.name);
            }
            Plan::Refuse(why) => bail!("dns: {why}"),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(kind: &str, proxied: bool) -> Existing {
        Existing { kind: kind.into(), proxied }
    }

    /// A missing host is made, a proxied one left, an unproxied one refused
    /// (never changed); records that route nothing do not count.
    #[test]
    fn plans() {
        assert_eq!(plan("*.finite.place", &[]), Plan::Create);
        assert_eq!(plan("*.finite.place", &[rec("TXT", false)]), Plan::Create);
        assert_eq!(plan("*.finite.place", &[rec("AAAA", true)]), Plan::Ready);
        assert_eq!(plan("x.dev", &[rec("A", true), rec("AAAA", true)]), Plan::Ready);
        let Plan::Refuse(why) = plan("x.dev", &[rec("A", true), rec("CNAME", false)]) else { panic!("an unproxied record is refused") };
        assert!(why.contains("x.dev") && why.contains("CNAME"), "{why}");
    }
}
