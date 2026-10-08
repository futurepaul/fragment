//! The MCP servers goose's sessions get on our goose image (the bridge's
//! goose runtime names them: docs/bridge.md):
//!
//! - `browser`: Microsoft's Playwright MCP, attached over CDP to the
//!   Chromium on the agent's own desktop, so what it does is on the screen
//!   its owner watches. Its tools read the page's accessibility snapshot
//!   (elements by `ref`), so the model needs no eyes. Behind our gate
//!   (proxy.rs).
//! - `computer`: the desktop itself, for apps and pages the browser tools
//!   cannot reach: cua-driver's screen tools (screenshots, which the model
//!   cannot read, are left out; the keyboard and mouse), with ours beside
//!   them (look.rs: `screen_look` through the vision model, `screen_click`
//!   through Clef). Behind the same gate.
//! - `web`: reading and searching without a browser (web.rs).

use std::sync::Arc;

use serde_json::Value;

use crate::desk::{self, Desk};
use crate::look::{self, Screen};
use crate::mcp::{self, Tools};
use crate::proxy::Proxy;
use crate::web;

/// The browser's MCP server, less its CDP endpoint
/// (`FRAGMENT_BROWSER_MCP` names another).
pub const BROWSER_MCP: &str = "node /opt/fragment/browser/node_modules/@playwright/mcp/cli.js";
/// The computer's MCP server (`FRAGMENT_COMPUTER_MCP` names another).
pub const COMPUTER_MCP: &str = "/usr/local/bin/cua-driver mcp";

pub const WEB_INSTRUCTIONS: &str = "Reading and searching the web without a browser: web_search for what to read, web_read for a page as Markdown. Fast and cheap: use these first for anything that needs no clicking, typing or login.";

fn command(var: &str, default: &str) -> Vec<String> {
    std::env::var(var).ok().filter(|v| !v.trim().is_empty()).unwrap_or_else(|| default.to_string()).split_whitespace().map(str::to_string).collect()
}

/// The browser tools on `desk`'s Chromium: attached over CDP at its first
/// call (the gate starts the desktop then), images never returned, what it
/// saves in the work.
pub fn browser(desk: Desk) -> Proxy {
    let display = desk::allocate(&desk).unwrap_or_else(|e| {
        eprintln!("fragment-desktop: {e}");
        std::process::exit(2)
    });
    let mut cmd = command("FRAGMENT_BROWSER_MCP", BROWSER_MCP);
    cmd.extend(["--cdp-endpoint".into(), format!("http://127.0.0.1:{}", desk::cdp_port(display)), "--image-responses".into(), "omit".into(), "--output-dir".into(), "/data/work/browser".into()]);
    Proxy { command: cmd, env: vec![("DISPLAY".into(), format!(":{display}"))], desk: Some(desk), extra: None }
}

/// The screen tools on `desk`.
pub fn computer(desk: Desk) -> Proxy {
    let display = desk::allocate(&desk).unwrap_or_else(|e| {
        eprintln!("fragment-desktop: {e}");
        std::process::exit(2)
    });
    let extra: Arc<dyn Tools> = Arc::new(Screen { desk: desk.clone() });
    Proxy {
        command: command("FRAGMENT_COMPUTER_MCP", COMPUTER_MCP),
        env: vec![("DISPLAY".into(), format!(":{display}")), ("CUA_DRIVER_RS_TELEMETRY_ENABLED".into(), "false".into()), ("CUA_DRIVER_RS_UPDATE_CHECK".into(), "false".into())],
        desk: Some(desk),
        extra: Some(extra),
    }
}

impl Tools for Screen {
    fn list(&self) -> Vec<Value> {
        look::tools()
    }

    fn call(&self, name: &str, args: Value) -> std::pin::Pin<Box<dyn std::future::Future<Output = Value> + Send + '_>> {
        let name = name.to_string();
        Box::pin(async move {
            match name.as_str() {
                "screen_look" => self.look(&args).await,
                "screen_click" => self.click(&args).await,
                other => mcp::text_result(&format!("no tool {other}"), true),
            }
        })
    }
}

/// The web tools.
pub struct Web;

impl Web {
    pub fn new() -> Web {
        Web
    }
}

impl Tools for Web {
    fn list(&self) -> Vec<Value> {
        web::tools()
    }

    fn call(&self, name: &str, args: Value) -> std::pin::Pin<Box<dyn std::future::Future<Output = Value> + Send + '_>> {
        let name = name.to_string();
        Box::pin(async move {
            match name.as_str() {
                "web_read" => web::read(&args).await,
                "web_search" => web::search(&args).await,
                other => mcp::text_result(&format!("no tool {other}"), true),
            }
        })
    }
}
