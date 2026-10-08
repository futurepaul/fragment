//! The bridge's screens, in process, against fake displays: each agent's
//! screen is its own display, routed by the `agent` its sockets name; Take
//! over is that agent's lease file, written as Hermes writes its own, and
//! the screen's input gate follows the file whoever changes it. Method: a
//! fake API with agents on one computer, a screens file (as our Hermes
//! image writes it) naming, for each agent, a fake RFB server
//! (support/display.rs, named `hermes:<profile>` as Hermes names its
//! desktops), its lease and its activity file; the bridge with its screen
//! on a loopback port; and viewers and control sockets as the screen's
//! page opens them (support/rfb.rs).

mod support;

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};

use fragment_bridge::lease::{self, Holder, LeaseFile};
use fragment_bridge::net::Base;
use fragment_bridge::screen::ScreenConfig;
use support::display::Display;
use support::fake::Fake;
use support::rfb::{Control, Viewer};

const WAIT: Duration = Duration::from_secs(10);

/// One agent's desktop's files, as a profile's `bot-desktop/` has them.
fn desktop(dir: &Path, profile: &str) -> PathBuf {
    let d = dir.join(profile).join("bot-desktop");
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn lease_of(dir: &Path, profile: &str) -> LeaseFile {
    LeaseFile::new(desktop(dir, profile).join("lease.json"))
}

/// The screens file: each of `screens` (agent, profile, display target).
fn write_screens(dir: &Path, screens: &[(&str, &str, String)]) {
    let list: Vec<Value> = screens
        .iter()
        .map(|(agent, profile, target)| {
            let d = desktop(dir, profile);
            json!({ "agent": agent, "rfb": target, "lease": d.join("lease.json"), "activity": d.join("activity") })
        })
        .collect();
    std::fs::write(dir.join("screens.tmp"), json!({ "screens": list }).to_string()).unwrap();
    std::fs::rename(dir.join("screens.tmp"), dir.join("screens.json")).unwrap();
}

fn write_ready(dir: &Path, agents: &[&str]) {
    std::fs::write(dir.join("ready.tmp"), json!({ "agents": agents }).to_string()).unwrap();
    std::fs::rename(dir.join("ready.tmp"), dir.join("agents.json")).unwrap();
}

/// A bridge whose screen listens on a loopback port, reading `dir`'s
/// screens and ready files; its start command notes each agent it starts.
async fn bridge(fake: &Fake, dir: &Path) -> (support::Running, Base) {
    let port = Display::free_port().await;
    let page = dir.join("page");
    std::fs::create_dir_all(&page).unwrap();
    std::fs::write(page.join("index.html"), "<p>the screen</p>").unwrap();
    let mut cfg = support::config(&fake.url(), dir, support::settings());
    cfg.agents_file = Some(dir.join("agents.json"));
    let started = dir.join("started");
    cfg.screen = Some(ScreenConfig {
        listen: format!("127.0.0.1:{port}").parse().unwrap(),
        dir: page,
        screens_file: Some(dir.join("screens.json")),
        // the agent's fragment comes last: `$0` to sh
        start: Some(vec!["/bin/sh".into(), "-c".into(), format!("echo \"$0\" >> {}", started.display())]),
    });
    let running = support::start(cfg, support::script());
    (running, Base::parse(&format!("http://127.0.0.1:{port}")).unwrap())
}

/// Opens a control socket once the screen answers (the bridge starting).
async fn control(base: &Base, viewer: &str, agent: &str) -> Control {
    let t = std::time::Instant::now();
    // bounded by WAIT
    loop {
        match Control::open(base, viewer, agent).await {
            Ok(c) => return c,
            Err(e) if t.elapsed() < WAIT => {
                let _ = e;
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Err(e) => panic!("{agent}'s control socket: {e}"),
        }
    }
}

/// What a control socket says next, within WAIT, or why not.
async fn next(c: &mut Control) -> Value {
    c.next().await.unwrap_or_else(|e| panic!("a word from the control socket: {e}"))
}

/// Waits until `done`, looked at every 50 ms.
async fn until(what: &str, done: impl Fn() -> bool) {
    support::until(WAIT.as_millis() as u64, what, done).await;
}

/// Goal: each agent's screen is its own desktop. From a socket naming
/// juniper the screen is juniper's display (its desktop's name); fred's,
/// down, is started for its first viewer, the start naming fred alone, and
/// is fred's. An agent the computer does not run, one the image names no
/// screen for, and a name that is no name are refused, never another's
/// screen. A viewer's stream touches its desktop's activity, so a watched
/// desktop is in use.
#[tokio::test]
async fn each_agent_has_its_own_screen() {
    let fake = Fake::start("127.0.0.1:0", &["juniper", "fred", "oak"]).await;
    let dir = support::dir("screens-own");
    let juniper = Display::start(0, "hermes:juniper--k3x9").await;
    let fred_port = Display::free_port().await;
    write_screens(&dir, &[("juniper--k3x9", "juniper--k3x9", juniper.target()), ("fred--k3x9", "fred--k3x9", format!("tcp:127.0.0.1:{fred_port}"))]);
    write_ready(&dir, &["juniper--k3x9", "fred--k3x9", "oak--k3x9"]);
    // juniper's desktop last used an hour ago
    let activity = desktop(&dir, "juniper--k3x9").join("activity");
    std::fs::write(&activity, b"").unwrap();
    let hour_ago = SystemTime::now() - Duration::from_secs(3600);
    std::fs::File::options().write(true).open(&activity).unwrap().set_modified(hour_ago).unwrap();
    let (running, base) = bridge(&fake, &dir).await;

    let mut c = control(&base, "w", "juniper--k3x9").await;
    assert_eq!(next(&mut c).await, json!({ "type": "control", "agent": "juniper--k3x9", "name": "juniper", "holder": null }), "a control socket names its agent");

    // refused: never another agent's screen
    let refused = |path: String| {
        let base = base.clone();
        async move { fragment_bridge::net::connect_ws(&base, &path, &[]).await.err().unwrap_or_default() }
    };
    let nobody = refused("/control?viewer=w&agent=nobody--k3x9".into()).await;
    assert!(nobody.contains("404"), "an agent the computer does not run: {nobody}");
    let unnamed = refused("/websockify?viewer=w&agent=oak--k3x9".into()).await;
    assert!(unnamed.contains("404"), "an agent the image names no screen for: {unnamed}");
    let unnamed = refused("/control?viewer=w&agent=oak--k3x9".into()).await;
    assert!(unnamed.contains("404"), "nor its control: {unnamed}");
    for bad in ["/control?viewer=w&agent=Juniper--k3x9", "/control?viewer=w&agent=juniper", "/websockify?viewer=w", "/control?agent=juniper--k3x9", "/control?viewer=w&agent=juniper--k3x9&agent=fred--k3x9"] {
        let why = refused(bad.into()).await;
        assert!(why.contains("400"), "{bad}: {why}");
    }

    let j = Viewer::open(&base, "w", "juniper--k3x9", WAIT).await.unwrap();
    assert_eq!(j.name, "hermes:juniper--k3x9", "juniper's socket shows juniper's desktop");
    let touched = std::fs::metadata(&activity).unwrap().modified().unwrap();
    assert!(touched > SystemTime::now() - Duration::from_secs(60), "a viewer's stream is a use of the desktop: {touched:?}");

    // fred's desktop is down: its first viewer starts it, naming fred
    let opening = {
        let base = base.clone();
        tokio::spawn(async move { Viewer::open(&base, "w", "fred--k3x9", Duration::from_secs(20)).await })
    };
    let started = dir.join("started");
    until("fred's desktop started", || std::fs::read_to_string(&started).is_ok_and(|s| s.contains("fred--k3x9"))).await;
    let fred = Display::start(fred_port, "hermes:fred--k3x9").await;
    let f = opening.await.unwrap().unwrap_or_else(|e| panic!("fred's screen: {e}"));
    assert_eq!(f.name, "hermes:fred--k3x9", "fred's socket shows fred's desktop");
    assert_eq!(std::fs::read_to_string(&started).unwrap().trim(), "fred--k3x9", "the start named fred alone: juniper's was up");
    drop((j, f, juniper, fred));
    running.stop().await;
}

/// Goal: Take over is the agent's own lease (Hermes' Bot Desktop lease, so
/// its computer_use refuses while a person holds it), and the screen's
/// input gate follows the lease, whoever changes it. Valid: a take writes
/// `human` and the viewer, one epoch on, and every viewer hears it; the
/// holder's pointer reaches the desktop and a watcher's does not; Give back
/// and the holder leaving each write `agent`. Replay: taking again changes
/// nothing. Invalid: a watcher's give changes nothing. A change by the
/// runtime (its `screen stop --force`) reaches every viewer, and the
/// former holder's input stops. Another agent's lease is untouched.
#[tokio::test]
async fn take_over_is_the_agents_lease() {
    let fake = Fake::start("127.0.0.1:0", &["juniper", "fred"]).await;
    let dir = support::dir("screens-lease");
    let juniper = Display::start(0, "hermes:juniper--k3x9").await;
    let fred = Display::start(0, "hermes:fred--k3x9").await;
    write_screens(&dir, &[("juniper--k3x9", "juniper--k3x9", juniper.target()), ("fred--k3x9", "fred--k3x9", fred.target())]);
    write_ready(&dir, &["juniper--k3x9", "fred--k3x9"]);
    let (running, base) = bridge(&fake, &dir).await;
    let (jl, fl) = (lease_of(&dir, "juniper--k3x9"), lease_of(&dir, "fred--k3x9"));

    let mut dc = control(&base, "d", "juniper--k3x9").await;
    let mut wc = control(&base, "w", "juniper--k3x9").await;
    assert_eq!(next(&mut dc).await["holder"], Value::Null);
    assert_eq!(next(&mut wc).await["holder"], Value::Null);
    let mut d = Viewer::open(&base, "d", "juniper--k3x9", WAIT).await.unwrap();
    let mut w = Viewer::open(&base, "w", "juniper--k3x9", WAIT).await.unwrap();
    // a few of the screen's looks pass: none tells its viewers what they
    // heard as their first word (the Docker rung once heard `null` again here)
    tokio::time::sleep(Duration::from_millis(4 * fragment_bridge::screen::TICK_MS)).await;

    dc.say("take").await.unwrap();
    assert_eq!(next(&mut dc).await["holder"], "d");
    assert_eq!(next(&mut wc).await["holder"], "d", "every viewer hears who took over");
    let taken = jl.read();
    assert_eq!((taken.holder.clone(), taken.epoch), (Holder::Human(Some("d".into())), 1), "the agent's lease names the person's viewer, one epoch on");
    let raw: Value = serde_json::from_slice(&std::fs::read(jl.path()).unwrap()).unwrap();
    assert_eq!((raw["holder"].clone(), raw["viewer_id"].clone(), raw["epoch"].clone()), (json!("human"), json!("d"), json!(1)), "as Hermes reads it: {raw}");
    assert_eq!(fl.read(), lease::Lease::default(), "fred's lease is fred's: untouched");

    d.pointer(1, 1, 0).await.unwrap();
    w.pointer(2, 2, 0).await.unwrap();
    until("the holder's pointer on juniper's desktop", || juniper.pointers().contains(&(1, 1))).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(juniper.pointers(), vec![(1, 1)], "the holder's input reaches the desktop; a watcher's does not");
    assert!(fred.pointers().is_empty(), "nor anyone's another agent's desktop");

    // replay and invalid: nothing changes, nothing is written
    dc.say("take").await.unwrap();
    wc.say("give").await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(jl.read().epoch, 1, "taken again, or given back by a watcher: no change");
    assert!(jl.read().holder.is("d"));

    // the runtime gives it back (Hermes' `screen stop --force`): heard, and obeyed
    jl.change(|l| lease::give(l, None, lease::now_s())).unwrap();
    assert_eq!(next(&mut dc).await["holder"], Value::Null, "a change by the runtime reaches the viewers");
    assert_eq!(next(&mut wc).await["holder"], Value::Null);
    d.pointer(3, 3, 0).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(!juniper.pointers().contains(&(3, 3)), "a viewer the lease no longer names sends no input");

    // Give back, and the holder leaving
    dc.say("take").await.unwrap();
    assert_eq!(next(&mut dc).await["holder"], "d");
    assert_eq!(next(&mut wc).await["holder"], "d");
    assert_eq!(jl.read().epoch, 3);
    dc.say("give").await.unwrap();
    assert_eq!(next(&mut wc).await["holder"], Value::Null);
    assert_eq!((jl.read().holder, jl.read().epoch), (Holder::Agent, 4), "Give back is the agent's lease again");
    dc.say("take").await.unwrap();
    assert_eq!(next(&mut wc).await["holder"], "d");
    drop(dc);
    assert_eq!(next(&mut wc).await["holder"], Value::Null, "the holder leaving gives it back");
    assert_eq!((jl.read().holder, jl.read().epoch), (Holder::Agent, 6));
    assert_eq!(fl.read(), lease::Lease::default(), "fred's lease untouched throughout");
    drop((d, w, juniper, fred));
    running.stop().await;
}

/// Goal (restart): a lease a person held when the bridge last stopped is
/// the agent's again at the next start, before anyone opens the screen (no
/// viewer survives a restart, and a person's lease left behind would keep
/// the agent's tools refusing for good); the agent's own is left alone.
/// And an agent that leaves the computer takes its screen with it: its
/// sockets close, and it is refused after.
#[tokio::test]
async fn an_earlier_lives_lease_is_given_back_and_a_gone_agents_screen_closes() {
    let fake = Fake::start("127.0.0.1:0", &["juniper", "fred"]).await;
    let dir = support::dir("screens-life");
    let juniper = Display::start(0, "hermes:juniper--k3x9").await;
    let fred = Display::start(0, "hermes:fred--k3x9").await;
    let screens = [("juniper--k3x9", "juniper--k3x9", juniper.target()), ("fred--k3x9", "fred--k3x9", fred.target())];
    write_screens(&dir, &screens);
    write_ready(&dir, &["juniper--k3x9", "fred--k3x9"]);
    let (jl, fl) = (lease_of(&dir, "juniper--k3x9"), lease_of(&dir, "fred--k3x9"));
    for _ in 0..4 {
        jl.change(|l| if l.holder == Holder::Agent { lease::take(l, "gone", 1.0) } else { lease::give(l, None, 2.0) }).unwrap();
    }
    jl.change(|l| lease::take(l, "gone", 3.0)).unwrap();
    assert_eq!((jl.read().holder.is("gone"), jl.read().epoch), (true, 5));
    fl.change(|l| lease::take(l, "x", 1.0)).unwrap();
    fl.change(|l| lease::give(l, None, 2.0)).unwrap();
    let fred_before = fl.read();
    let (running, base) = bridge(&fake, &dir).await;
    until("the earlier life's lease given back", || jl.read().holder == Holder::Agent).await;
    assert_eq!(jl.read().epoch, 6, "given back once, one epoch on");
    assert_eq!(fl.read(), fred_before, "the agent's own lease: left alone");

    let mut fc = control(&base, "w", "fred--k3x9").await;
    assert_eq!(next(&mut fc).await["agent"], "fred--k3x9");
    fake.remove_agent("fred");
    write_screens(&dir, &screens[..1]);
    write_ready(&dir, &["juniper--k3x9"]);
    let closed = fc.next().await;
    assert!(closed.is_err(), "fred gone, its screen's socket closes: {closed:?}");
    let why = fragment_bridge::net::connect_ws(&base, "/control?viewer=w&agent=fred--k3x9", &[]).await.err().unwrap_or_default();
    assert!(why.contains("404"), "and it is refused after: {why}");
    let mut jc = control(&base, "w", "juniper--k3x9").await;
    assert_eq!(next(&mut jc).await["holder"], Value::Null, "juniper's screen stays");
    drop((juniper, fred));
    running.stop().await;
}
