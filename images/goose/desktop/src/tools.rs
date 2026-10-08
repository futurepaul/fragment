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
