//! `fragment-lan`: the DNS server and the TLS front door of the dev stack on
//! a home network (docs/self-host-lan.md). `cargo xtask dev --lan` starts it.
//!
//!   fragment-lan serve <config.json> [--until-stdin-closes]
//!   fragment-lan version     prints the config version it reads

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["serve", config, rest @ ..] if rest.iter().all(|a| *a == "--until-stdin-closes") => {
            let text = std::fs::read_to_string(config).map_err(|e| anyhow::anyhow!("{config}: {e}"))?;
            let cfg: fragment_lan::serve::ServeConfig = serde_json::from_str(&text).map_err(|e| anyhow::anyhow!("{config}: {e}"))?;
            fragment_lan::serve::run(cfg, !rest.is_empty())
        }
        ["version"] => {
            println!("fragment-lan {}", fragment_lan::serve::CONFIG_VERSION);
            Ok(())
        }
        _ => anyhow::bail!("usage: fragment-lan serve <config.json> [--until-stdin-closes] | version"),
    }
}
