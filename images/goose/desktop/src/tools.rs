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
/// The headless browser's (a paired machine's: no desktop), the same
/// Playwright MCP the image pins, fetched by npx on first use
/// (`FRAGMENT_BROWSER_MCP` names another).
pub const HEADLESS_BROWSER_MCP: &str = "npx -y @playwright/mcp@0.0.83";
/// Playwright MCP's tools the agent is offered: all of its core set but
/// `browser_run_code_unsafe` (code in the MCP server's own process: the
/// shell is the agent's for code), the network log's two, `browser_drop`,
/// `browser_resize` and `browser_emulate_media`, which no task here needs.
pub const BROWSER_TOOLS: [&str; 18] = [
    "browser_navigate",
    "browser_navigate_back",
    "browser_snapshot",
    "browser_find",
    "browser_click",
    "browser_type",
    "browser_fill_form",
    "browser_select_option",
    "browser_press_key",
    "browser_hover",
    "browser_drag",
    "browser_wait_for",
    "browser_tabs",
    "browser_handle_dialog",
    "browser_file_upload",
    "browser_evaluate",
    "browser_console_messages",
    "browser_take_screenshot",
];
/// What a cut browser result says.
pub const BROWSER_CUT: &str = "read the part you need with browser_find (text or a regex), or browser_snapshot with `depth` or a `target`";

/// The computer's MCP server (`FRAGMENT_COMPUTER_MCP` names another).
pub const COMPUTER_MCP: &str = "/usr/local/bin/cua-driver mcp";
/// cua-driver's tools the agent is offered (it serves 66): the keyboard,
/// the mouse, the clipboard and the windows. Not its screenshots (the
/// model reads none: `screen_look` instead), its browser tools (the
/// browser's own are better and the same Chromium), its AT-SPI element
/// trees (the image runs no accessibility bus), its recordings, sessions,
/// cursor themes, updates and installs.
pub const COMPUTER_TOOLS: [&str; 17] = [
    "list_windows",
    "list_apps",
    "launch_app",
    "bring_to_front",
    "get_accessibility_tree",
    "get_screen_size",
    "get_cursor_position",
    "click",
    "double_click",
    "right_click",
    "drag",
    "scroll",
    "type_text",
    "press_key",
    "hotkey",
    "clipboard_read",
    "clipboard_write",
];
/// cua-driver's input tools: on Xvnc only its `foreground` delivery
/// reaches a window (the background one needs a virtual keyboard Xvnc
/// cannot add), so it is their default.
pub const COMPUTER_DEFAULTS: [(&str, &str, &str); 8] = [
    ("click", "delivery_mode", "foreground"),
    ("double_click", "delivery_mode", "foreground"),
    ("right_click", "delivery_mode", "foreground"),
    ("drag", "delivery_mode", "foreground"),
    ("scroll", "delivery_mode", "foreground"),
    ("type_text", "delivery_mode", "foreground"),
    ("press_key", "delivery_mode", "foreground"),
    ("hotkey", "delivery_mode", "foreground"),
];

/// What the browser's server tells the model.
pub const BROWSER_INSTRUCTIONS: &str = "Your own Chromium, on the desktop your owner watches. browser_navigate opens a page; browser_snapshot reads it as an accessibility tree, each element with a ref ([ref=f1e5]); act on an element by passing its ref as `target` to browser_click, browser_type, browser_fill_form or browser_select_option. An action's answer does not repeat the whole page: call browser_snapshot (or browser_find, to search a long page) to see what it did. One snapshot of a list gives you all of it: do not scroll to read. If the page needs your owner (a login, a captcha, a payment), say so and ask them to take over your screen.";
/// What the headless browser's server tells the model.
pub const HEADLESS_INSTRUCTIONS: &str = "A headless Chromium of your own, on your owner's machine: no one sees it. browser_navigate opens a page; browser_snapshot reads it as an accessibility tree, each element with a ref ([ref=f1e5]); act on an element by passing its ref as `target` to browser_click, browser_type, browser_fill_form or browser_select_option. An action's answer does not repeat the whole page: call browser_snapshot (or browser_find, to search a long page) to see what it did. One snapshot of a list gives you all of it: do not scroll to read. Each session starts it fresh, signed in nowhere: if a page needs your owner (a login, a captcha, a payment), say so in your report.";

/// What the computer's server tells the model.
pub const COMPUTER_INSTRUCTIONS: &str = "Your desktop's keyboard, mouse, clipboard and windows, for apps and pages the browser tools cannot reach. You read no images: screen_look asks a vision model what the screen shows, and screen_click finds what you describe (Clef) and clicks it. For a window's own input, list_windows gives each window's window_id and pid: pass both to type_text, press_key, hotkey, click or scroll (x and y in the window's own pixels). In the browser, prefer the browser tools.";

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
    let cdp = format!("http://127.0.0.1:{}", desk::cdp_port(display));
    // no image inline (the model reads none), no Playwright code echoed;
    // its files (a snapshot of the page after each action, screenshots) in
    // the desktop's directory, never /data: one a click, they would pile
    // up in every save
    let files = desk.dir.join("browser").display().to_string();
    for (k, v) in [("--cdp-endpoint", cdp.as_str()), ("--image-responses", "omit"), ("--codegen", "none"), ("--output-dir", files.as_str()), ("--file-paths", "absolute")] {
        cmd.extend([k.to_string(), v.to_string()]);
    }
    Proxy { command: cmd, env: vec![("DISPLAY".into(), format!(":{display}"))], desk: Some(desk), extra: None, only: Some(&BROWSER_TOOLS), cut_note: BROWSER_CUT, defaults: &[], instructions: Some(BROWSER_INSTRUCTIONS) }
}

/// The browser tools on a headless Chromium (a paired machine's hands,
/// with no desktop): a fresh profile in memory each session (Playwright
/// MCP's `--isolated`), the Chromium `FRAGMENT_BROWSER_CHROME` names when
/// it names one, else Playwright's own; images never returned; its files
/// under `FRAGMENT_BROWSER_FILES` (else the system's temporary directory).
pub fn headless_browser() -> Proxy {
    let files = std::env::var("FRAGMENT_BROWSER_FILES").ok().filter(|d| std::path::Path::new(d).is_absolute()).map_or_else(|| std::env::temp_dir().join("fragment-browser"), std::path::PathBuf::from);
    let chrome = std::env::var("FRAGMENT_BROWSER_CHROME").ok().filter(|c| !c.trim().is_empty());
    headless_with(command("FRAGMENT_BROWSER_MCP", HEADLESS_BROWSER_MCP), &files, chrome)
}

/// `headless_browser`'s server: `mcp`, then its arguments.
fn headless_with(mut cmd: Vec<String>, files: &std::path::Path, chrome: Option<String>) -> Proxy {
    let files = files.display().to_string();
    for (k, v) in [("--image-responses", "omit"), ("--codegen", "none"), ("--output-dir", files.as_str()), ("--file-paths", "absolute")] {
        cmd.extend([k.to_string(), v.to_string()]);
    }
    cmd.extend(["--headless".to_string(), "--isolated".to_string()]);
    if let Some(chrome) = chrome {
        cmd.extend(["--executable-path".to_string(), chrome]);
    }
    Proxy { command: cmd, env: vec![], desk: None, extra: None, only: Some(&BROWSER_TOOLS), cut_note: BROWSER_CUT, defaults: &[], instructions: Some(HEADLESS_INSTRUCTIONS) }
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
        only: Some(&COMPUTER_TOOLS),
        cut_note: "ask for less: a window, not the whole desktop",
        defaults: &COMPUTER_DEFAULTS,
        instructions: Some(COMPUTER_INSTRUCTIONS),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Goal: a paired machine's browser is headless, its own each session,
    /// on the Chromium named, behind the same gate (no images, the same
    /// tools) and with no desktop to hold it back. Method: its server's
    /// command line and gate, with a Chromium named and without.
    #[test]
    fn a_machines_browser_is_headless_and_gated() {
        let mcp: Vec<String> = HEADLESS_BROWSER_MCP.split_whitespace().map(str::to_string).collect();
        let p = headless_with(mcp.clone(), std::path::Path::new("/home/p/fragment-hands/browser"), Some("/usr/bin/chromium".into()));
        let line = p.command.join(" ");
        assert!(line.starts_with("npx -y @playwright/mcp@0.0.83 "), "{line}");
        for part in ["--headless", "--isolated", "--image-responses omit", "--output-dir /home/p/fragment-hands/browser", "--executable-path /usr/bin/chromium"] {
            assert!(line.contains(part), "{part} in {line}");
        }
        assert!(!line.contains("--cdp-endpoint"), "no desktop's browser to attach to");
        assert!(p.desk.is_none() && p.env.is_empty() && p.only == Some(&BROWSER_TOOLS[..]) && p.instructions == Some(HEADLESS_INSTRUCTIONS));
        let own = headless_with(mcp, std::path::Path::new("/tmp/b"), None);
        assert!(!own.command.iter().any(|a| a == "--executable-path"), "Playwright's own Chromium when none is named");
    }
}
