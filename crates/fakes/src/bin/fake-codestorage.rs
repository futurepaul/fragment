//! The code.storage fake as a process of its own: a code store outside the
//! node and outside the e2e, reached only through its HTTP API, so the
//! suite's external-store mode (`FRAGMENT_E2E_CODESTORE=external`) can be
//! proven on it before a real self-hosted store (macrofiche) is ready.
//! None of its test levers is reachable: they are methods, never routes.
//!
//!   fake-codestorage --org <org> --key-file <pkcs8 pem> [--port <p>] [--state-file <file>]
//!
//! It prints `ready <url>` once it listens, and serves until it is killed.

use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use fragment_fakes::codestorage::{CodeStorage, Options};

const USAGE: &str = "usage: fake-codestorage --org <org> --key-file <pkcs8 pem> [--port <p>] [--state-file <file>]";

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (mut org, mut key_file, mut port, mut state_file) = (None, None, 0u16, None);
    let mut it = args.iter();
    // bounded: each pass takes one argument at least
    while let Some(arg) = it.next() {
        let mut value = || it.next().cloned().context(USAGE);
        match arg.as_str() {
            "--org" => org = Some(value()?),
            "--key-file" => key_file = Some(PathBuf::from(value()?)),
            "--port" => port = value()?.parse().context("--port is a port")?,
            "--state-file" => state_file = Some(PathBuf::from(value()?)),
            _ => bail!("{USAGE}"),
        }
    }
    let (Some(org), Some(key_file)) = (org, key_file) else { bail!("{USAGE}") };
    // an org key always: a store outside the node verifies every token
    let key = std::fs::read_to_string(&key_file).with_context(|| format!("read {}", key_file.display()))?;
    let fake = CodeStorage::start(Options { org, org_key_pem: Some(key), state_file, port, ..Default::default() })?;
    println!("ready {}", fake.url);
    // serves on its own threads until the process is killed
    loop {
        std::thread::park();
    }
}
