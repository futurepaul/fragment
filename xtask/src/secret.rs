//! `cargo xtask secret set|gen|list`: a deployment's secrets, in its
//! account's Cloudflare Secrets Store (docs/secrets.md), through wrangler
//! on the pinned Node (crates/devstack/src/store.rs).
//!
//!   secret set <name> --config <file> [--from-file <path>]
//!       sets the store secret <name>: from wrangler's own hidden prompt in
//!       a terminal, from standard input when it is not one, or, once, from
//!       a file (`--from-file`: moving a secret's old file in). A secret
//!       already there is updated, but for the config's host secret, which
//!       is rotated by name and never set again in place
//!   secret gen <name> --config <file>
//!       makes a new store secret <name> of 32 random bytes as hex, made
//!       here, set directly and never shown (a host secret)
//!   secret list --config <file>
//!       the store's secrets, names and times only, and any the config
//!       names that it lacks
//!
//! The account is the config's (`account_id`), and its store is the
//! account's one: `set` and `gen` make it (named `fragment`) when there is
//! none, and `list` says so and makes nothing. `--local <dir>` in place of
//! `--config` acts on wrangler's local store under a node's state
//! directory instead (`cell/.wrangler/state` is `cargo xtask dev`'s),
//! never the account's. A value never appears on a command line or in
//! output. There is no `rm`: deleting a secret is irreversible and Paul's,
//! with wrangler or on the dashboard.

use std::io::IsTerminal;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use fragment_devstack as devstack;
use devstack::store::{self, Listed, Put, Store, Value};

const USAGE: &str = "usage: cargo xtask secret set <name> (--config <file> | --local <state dir>) [--from-file <path>]\n\
                     \x20      cargo xtask secret gen <name> (--config <file> | --local <state dir>)\n\
                     \x20      cargo xtask secret list (--config <file> | --local <state dir>)";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verb {
    Set,
    Gen,
    List,
}

/// Which store a command acts on.
#[derive(Debug, PartialEq, Eq)]
enum At {
    /// The config's account's store.
    Account(PathBuf),
    /// wrangler's local store under this state directory.
    Local(PathBuf),
}

#[derive(Debug, PartialEq, Eq)]
struct Args {
    verb: Verb,
    name: Option<String>,
    at: At,
    from_file: Option<PathBuf>,
}

fn parse(rest: &[String]) -> Result<Args> {
    let usage = || anyhow::anyhow!("{USAGE}");
    let mut it = rest.iter();
    let verb = match it.next().map(String::as_str) {
        Some("set") => Verb::Set,
        Some("gen") => Verb::Gen,
        Some("list") => Verb::List,
        _ => return Err(usage()),
    };
    let (mut name, mut config, mut local, mut from_file) = (None, None, None, None);
    // bounded: each pass takes one argument at least
    while let Some(a) = it.next() {
        match a.as_str() {
            "--config" => config = Some(PathBuf::from(it.next().ok_or_else(usage)?)),
            "--local" => local = Some(PathBuf::from(it.next().ok_or_else(usage)?)),
            "--from-file" if verb == Verb::Set => from_file = Some(PathBuf::from(it.next().ok_or_else(usage)?)),
            // a value is never an argument: it would sit in shell history
            v if v.starts_with("--value") => bail!("a secret's value is never an argument (it would sit in your shell's history): set prompts for it, reads standard input, or takes --from-file <path>"),
            n if !n.starts_with('-') && name.is_none() && verb != Verb::List => name = Some(n.to_string()),
            _ => return Err(usage()),
        }
    }
    let at = match (config, local) {
        (Some(c), None) => At::Account(c),
        (None, Some(dir)) => At::Local(std::path::absolute(&dir).with_context(|| format!("resolve {}", dir.display()))?),
        _ => return Err(usage()),
    };
    if verb != Verb::List && name.is_none() {
        return Err(usage());
    }
    if let Some(n) = &name {
        anyhow::ensure!(store::valid_name(n), "{}", store::StoreError::InvalidName(n.clone()));
    }
    Ok(Args { verb, name, at, from_file })
}

pub fn secret(rest: &[String]) -> Result<()> {
    let args = parse(rest)?;
    let tools = devstack::Tools::locate()?;
    match args.verb {
        Verb::Set | Verb::Gen => put(&tools, &args),
        Verb::List => list(&tools, &args),
    }
}

/// The value `set` or `gen` puts, read (or made) before the store is asked
/// anything, so a value that would be refused changes nothing.
fn value_of(args: &Args) -> Result<Value> {
    let value = match (args.verb, &args.from_file) {
        (Verb::Gen, _) => Value::Bytes(devstack::random_hex(32).into_bytes()),
        (Verb::Set, Some(file)) => Value::from_file(&crate::deploy::expand(file))?,
        (Verb::Set, None) if std::io::stdin().is_terminal() => {
            // wrangler prompts only when its input and output are both a terminal, outside CI
            if !std::io::stdout().is_terminal() || std::env::var_os("CI").is_some() {
                bail!("wrangler's hidden prompt needs a terminal for its input and output, outside CI: pipe the value in, or name --from-file <path>");
            }
            Value::Prompt
        }
        (Verb::Set, None) => Value::from_stdin()?,
        (Verb::List, _) => unreachable!("list puts nothing"),
    };
    Ok(value)
}

fn put(tools: &devstack::Tools, args: &Args) -> Result<()> {
    let name = args.name.as_deref().expect("set and gen name a secret");
    let value = value_of(args)?;
    let prompted = matches!(value, Value::Prompt);
    let (account, host_secrets) = match &args.at {
        At::Account(config) => {
            let of = crate::deploy::secrets_of(config)?;
            (Some(of.account_id), of.host_secrets)
        }
        At::Local(_) => (None, vec![]),
    };
    let found = match &account {
        Some(account_id) => Some(account_store(tools, account_id, true)?.expect("made when missing")),
        None => None,
    };
    let (target, place) = target(&args.at, account.as_deref(), found.as_ref());
    let listed = store::list(tools, &target)?;
    let existing = listed.iter().find(|l| l.name == name);
    if let Some(why) = refused_update(args.verb, name, existing, &host_secrets) {
        bail!("{why}");
    }
    if prompted && existing.is_some() {
        println!("{name} is in {place} already: wrangler asks whether to update its value (answer y), then for the value");
    }
    let put = store::put(tools, &target, name, value, existing.map(|l| l.id.as_str()))?;
    let done = match put {
        Put::Created => "created",
        Put::Updated => "updated",
    };
    let how = match args.verb {
        Verb::Gen => " (32 random bytes as hex, made here and never shown)",
        _ => "",
    };
    println!("{name}: {done} in {place}{how}");
    Ok(())
}

/// Why `verb` may not put `name` over `existing`, if it may not: `gen`
/// makes new secrets only, and the config's host secret is never set again
/// in place (every value at rest is sealed under it).
fn refused_update(verb: Verb, name: &str, existing: Option<&Listed>, host_secrets: &[String]) -> Option<String> {
    let existing = existing?;
    if verb == Verb::Gen {
        return Some(format!(
            "{name} is in the store already (modified {}): gen makes a new secret only. A host secret is rotated by name: gen a new name, then name it in the config (docs/secrets.md, Rotating the host secret)",
            existing.modified
        ));
    }
    if host_secrets.iter().any(|h| h == name) {
        return Some(format!(
            "{name} is this config's host secret: every value at rest is sealed under it, so it is never set again in place (that would leave them unopenable). Rotate it by name: docs/secrets.md, Rotating the host secret"
        ));
    }
    None
}

/// The account's store; made first when `make` and there is none.
fn account_store(tools: &devstack::Tools, account_id: &str, make: bool) -> Result<Option<store::AccountStore>> {
    match store::account_store(tools, account_id)? {
        Some(s) => Ok(Some(s)),
        None if make => {
            let made = store::create_store(tools, account_id)?;
            println!("the account had no Secrets Store: made {} ({})", made.name, made.id);
            Ok(Some(made))
        }
        None => Ok(None),
    }
}

/// The store a command acts on, and how its output names it.
fn target<'a>(at: &'a At, account_id: Option<&'a str>, found: Option<&'a store::AccountStore>) -> (Store<'a>, String) {
    match (at, account_id, found) {
        (At::Account(_), Some(account_id), Some(s)) => (Store::Account { account_id, store_id: &s.id }, format!("the account's Secrets Store {} ({})", s.name, s.id)),
        (At::Local(dir), _, _) => (Store::Local { persist: dir }, format!("the local store under {}", dir.display())),
        _ => unreachable!("an account's store is resolved first"),
    }
}

fn list(tools: &devstack::Tools, args: &Args) -> Result<()> {
    match &args.at {
        At::Account(config) => {
            let of = crate::deploy::secrets_of(config)?;
            let Some(found) = account_store(tools, &of.account_id, false)? else {
                println!("the account {} has no Secrets Store yet: `cargo xtask secret set <name> --config {}` makes it", of.account_id, config.display());
                return Ok(());
            };
            let listed = store::list(tools, &Store::Account { account_id: &of.account_id, store_id: &found.id })?;
            print!("{}", listing(&format!("the account's Secrets Store {} ({})", found.name, found.id), &listed));
            match &of.named {
                Ok(named) => {
                    let named: Vec<(String, &str)> = named.iter().map(|(f, n)| (f.clone(), n.as_str())).collect();
                    let missing = store::missing(&named, &listed);
                    match missing.is_empty() {
                        true => println!("every secret {} names is there", config.display()),
                        false => println!("{} names {} it lacks:\n{}", config.display(), missing.len(), crate::deploy::set_commands(config, &missing).join("\n")),
                    }
                }
                Err(why) => println!("(not checked against {}, which a deploy would refuse: {why})", config.display()),
            }
        }
        At::Local(dir) => {
            let listed = store::list(tools, &Store::Local { persist: dir })?;
            print!("{}", listing(&format!("the local store under {} (it keeps no times: wrangler shows now)", dir.display()), &listed));
        }
    }
    Ok(())
}

/// A listing as `list` prints it: names and times, never a value.
fn listing(head: &str, listed: &[Listed]) -> String {
    let width = listed.iter().map(|l| l.name.len()).max().unwrap_or(0).max("NAME".len());
    let mut out = format!("{head}: {} secret{}\n", listed.len(), if listed.len() == 1 { "" } else { "s" });
    if !listed.is_empty() {
        let time = listed.iter().map(|l| l.created.len()).max().unwrap_or(0).max("CREATED".len());
        out.push_str(&format!("  {:width$}  {:time$}  MODIFIED\n", "NAME", "CREATED"));
        for l in listed {
            out.push_str(&format!("  {:width$}  {:time$}  {}\n", l.name, l.created, l.modified));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// A directory as `--local` takes it.
    fn local(dir: &str) -> At {
        At::Local(std::path::absolute(Path::new(dir)).unwrap())
    }

    fn args(line: &str) -> Result<Args> {
        parse(&line.split_whitespace().map(str::to_string).collect::<Vec<_>>())
    }

    /// The three verbs and their one store each; a value is never an
    /// argument, a name is a store name, and `rm` is no verb.
    #[test]
    fn the_commands_parse_as_documented() {
        let config = || At::Account("/c/e2e.jsonc".into());
        assert_eq!(args("set fragment-host-secret --config /c/e2e.jsonc").unwrap(), Args { verb: Verb::Set, name: Some("fragment-host-secret".into()), at: config(), from_file: None });
        assert_eq!(
            args("set fragment-codestorage-private-key --config /c/e2e.jsonc --from-file ~/.config/fragment/secrets/codestorage-private-key.pem").unwrap().from_file,
            Some(PathBuf::from("~/.config/fragment/secrets/codestorage-private-key.pem"))
        );
        assert_eq!(args("gen fragment-host-secret --local target/s").unwrap(), Args { verb: Verb::Gen, name: Some("fragment-host-secret".into()), at: local("target/s"), from_file: None });
        assert_eq!(args("list --config /c/e2e.jsonc").unwrap(), Args { verb: Verb::List, name: None, at: config(), from_file: None });
        for bad in [
            "set",
            "set fragment-host-secret",
            "set fragment-host-secret --config a --local b",
            "list fragment-host-secret --config a",
            "gen fragment-host-secret --config a --from-file f",
            "set two names --config a",
            "rm fragment-host-secret --config a",
            "set 'has space' --config a",
            "set ~/.config/fragment/secrets/host-secret --config a",
        ] {
            assert!(args(bad).is_err(), "{bad}");
        }
        let refused = args("set fragment-host-secret --config a --value=hunter2").err().map(|e| e.to_string()).unwrap_or_default();
        assert!(refused.contains("never an argument") && !refused.contains("hunter2"), "{refused}");
        assert!(args("set fragment-host-secret --config a --value hunter2").is_err());
    }

    /// `gen` never replaces a secret, and `set` never replaces the
    /// config's host secret (or the one before it); `set` updates any
    /// other secret already there.
    #[test]
    fn a_host_secret_is_never_set_again_in_place() {
        let there = Listed { name: "fragment-host-secret".into(), id: "17cf".into(), created: "c".into(), modified: "10/5/2026, 8:04:03 AM".into() };
        let hosts = vec!["fragment-host-secret".to_string(), "fragment-host-secret-old".to_string()];
        assert_eq!(refused_update(Verb::Set, "fragment-host-secret", None, &hosts), None, "a new host secret is set");
        assert_eq!(refused_update(Verb::Gen, "fragment-host-secret", None, &hosts), None, "or made");
        let why = refused_update(Verb::Set, "fragment-host-secret", Some(&there), &hosts).unwrap();
        assert!(why.contains("never set again in place") && why.contains("Rotate it by name"), "{why}");
        let other = Listed { name: "fragment-host-secret-old".into(), ..there.clone() };
        assert!(refused_update(Verb::Set, "fragment-host-secret-old", Some(&other), &hosts).is_some(), "nor the one before it");
        let why = refused_update(Verb::Gen, "fragment-xai-api-key", Some(&Listed { name: "fragment-xai-api-key".into(), ..there.clone() }), &hosts).unwrap();
        assert!(why.contains("gen makes a new secret only"), "{why}");
        let key = Listed { name: "fragment-xai-api-key".into(), ..there };
        assert_eq!(refused_update(Verb::Set, "fragment-xai-api-key", Some(&key), &hosts), None, "any other secret is updated");
    }

    /// `list` prints names and times in columns, and nothing else.
    #[test]
    fn a_listing_shows_names_and_times() {
        let listed = vec![
            Listed { name: "fragment-host-secret".into(), id: "17cf".into(), created: "10/5/2026, 8:03:58 AM".into(), modified: "10/5/2026, 8:03:58 AM".into() },
            Listed { name: "fragment-xai-api-key".into(), id: "2e18".into(), created: "10/5/2026, 9:00:00 AM".into(), modified: "10/6/2026, 9:00:00 AM".into() },
        ];
        let out = listing("the account's Secrets Store fragment (0f0e)", &listed);
        assert_eq!(
            out,
            "the account's Secrets Store fragment (0f0e): 2 secrets\n  NAME                  CREATED                MODIFIED\n  fragment-host-secret  10/5/2026, 8:03:58 AM  10/5/2026, 8:03:58 AM\n  fragment-xai-api-key  10/5/2026, 9:00:00 AM  10/6/2026, 9:00:00 AM\n"
        );
        assert!(!out.contains("17cf"), "ids are wrangler's, not the listing's");
        assert_eq!(listing("the local store", &[]), "the local store: 0 secrets\n");
    }
}
