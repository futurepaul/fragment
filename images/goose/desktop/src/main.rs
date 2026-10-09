//! `fragment-desktop`: each agent's desktop in our goose image, and the
//! tools its goose uses on it and on the web (docs/computers.md, "Our
//! images"; desk.rs).
//!
//! ```text
//! fragment-desktop start [<agent>]      start its desktop (the bridge's BRIDGE_SCREEN_START)
//! fragment-desktop stop [<agent>]       stop it
//! fragment-desktop status [<agent>]     whether it runs, its display and its browser's CDP
//! fragment-desktop display [<agent>]    its display's number (given it at the first ask)
//! fragment-desktop env [<agent>]        start it; print DISPLAY and the CDP address, for a shell
//! fragment-desktop run <agent>          the desktop's supervisor (start runs it)
//! fragment-desktop mcp browser [<agent>]   the browser tools (MCP on stdio)
//! fragment-desktop mcp browser --headless  the browser tools on a headless Chromium (a paired machine's)
//! fragment-desktop mcp computer [<agent>]  the screen tools (MCP on stdio)
//! fragment-desktop mcp web              reading and searching the web (MCP on stdio)
//! ```
//!
//! `<agent>` is the agent's fragment, `FRAGMENT_AS_AGENT` (its shell's) by
//! default. The desktops are under `FRAGMENT_DESKTOPS` (`/run/desktop`).

mod desk;
mod look;
mod mcp;
mod proxy;
mod tools;
mod web;

use std::sync::Arc;

use desk::Desk;

fn fail(why: &str) -> ! {
    eprintln!("fragment-desktop: {why}");
    std::process::exit(2);
}

/// The agent named, or the shell's own.
fn agent(arg: Option<String>) -> Desk {
    let name = arg.or_else(|| std::env::var("FRAGMENT_AS_AGENT").ok()).filter(|a| !a.trim().is_empty()).unwrap_or_else(|| fail("name the agent: fragment-desktop <command> <agent fragment> (or FRAGMENT_AS_AGENT)"));
    Desk::new(&desk::root(), &name).unwrap_or_else(|e| fail(&e))
}

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let cmd = args.next().unwrap_or_default();
    match cmd.as_str() {
        "start" => {
            let d = agent(args.next());
            match desk::start(&d) {
                Ok(n) => println!("{} is up on :{n} (CDP http://127.0.0.1:{})", d.agent, desk::cdp_port(n)),
                Err(e) => fail(&e),
            }
        }
        "stop" => {
            let d = agent(args.next());
            println!("{}", if desk::stop(&d) { "stopped" } else { "not running" });
        }
        "display" => {
            let d = agent(args.next());
            println!("{}", desk::allocate(&d).unwrap_or_else(|e| fail(&e)));
        }
        "status" => {
            let d = agent(args.next());
            let n = desk::allocate(&d).unwrap_or_else(|e| fail(&e));
            let state = if desk::up(&d, n) { "up" } else if desk::running(&d).is_some() { "starting" } else { "stopped" };
            println!("{}: {state}, display :{n}, CDP http://127.0.0.1:{}, held: {}, dir {}", d.agent, desk::cdp_port(n), d.held(), d.dir.display());
        }
        "env" => {
            let d = agent(args.next());
            let n = desk::start(&d).unwrap_or_else(|e| fail(&e));
            println!("export DISPLAY=:{n}\nexport FRAGMENT_BROWSER_CDP=http://127.0.0.1:{}", desk::cdp_port(n));
        }
        "run" => {
            let d = agent(args.next());
            if let Err(e) = desk::run(d).await {
                fail(&e);
            }
        }
        "mcp" => {
            let kind = args.next().unwrap_or_default();
            let ran = match kind.as_str() {
                "browser" => match args.next() {
                    Some(flag) if flag == "--headless" => tools::headless_browser().run().await,
                    named => tools::browser(agent(named)).run().await,
                },
                "computer" => tools::computer(agent(args.next())).run().await,
                "web" => mcp::serve("web", tools::WEB_INSTRUCTIONS, Arc::new(tools::Web::new())).await,
                other => fail(&format!("{other:?}: mcp browser, computer or web")),
            };
            if let Err(e) = ran {
                fail(&e.to_string());
            }
        }
        "version" => println!("fragment-desktop {}", env!("CARGO_PKG_VERSION")),
        other => fail(&format!("{other:?}: start, stop, status, display, env, run, mcp or version")),
    }
}
