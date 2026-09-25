// Continuous sync: OS events (debounced) + a live `__watch` websocket
// from the cell + periodic sweeps as the correctness floor. Falls back to
// polling when native watching is unavailable. A wakeup costs what it
// needs: the folder's echo of a pull and the feed's echo of our own commit
// cost nothing, a local change is one pass, and an idle sweep is a scan
// and one head read.
use crate::api::Client;
use crate::codestorage::{CsError, Held, MAIN};
use crate::sync::{self, Report, SyncError, SyncLock, SyncOptions};
use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Clone)]
pub struct WatchConfig {
    pub live: bool, // listen on the cell's live change channel
}

impl Default for WatchConfig {
    fn default() -> Self {
        WatchConfig { live: true }
    }
}

// tuned constants: event debounce 300ms (max-latency 3x), poll fallback
// 5s, full sweep every 60s. Nobody ever set the knobs; the defaults are
// the product now.
const DEBOUNCE_MS: u64 = 300;
const POLL_INTERVAL_SECS: u64 = 5;
const RESCAN_SECS: u64 = 60;
/// Wakeups one batch takes from the queue at most: a burst (a checkout,
/// an editor's save and its droppings) settles into one pass however many
/// it queued; any past this make the next batch.
const WAKEUPS_BATCH_MAX: usize = 100_000;

/// Whether an OS event touched a path that syncs (`sync::syncable`): any
/// of its paths, taken relative to the folder. An event that names no path
/// (the OS asking for a rescan), or a path outside the folder (a root the
/// OS reports by its real path), counts.
fn relevant(roots: &[PathBuf], paths: &[PathBuf]) -> bool {
    if paths.is_empty() {
        return true;
    }
    paths.iter().any(|p| match roots.iter().find_map(|root| p.strip_prefix(root).ok()) {
        Some(rel) => sync::syncable(&rel.to_string_lossy().replace('\\', "/")),
        None => true,
    })
}

/// The debounce window: hold the latest wakeups until things go quiet, or
/// until the max-latency cap forces a flush, then run ONE pass.
struct Debounced {
    pending: Arc<AtomicBool>,
    debounce: Duration,
    max_latency: Duration,
}

impl Debounced {
    /// block until quiet (or the cap), then clear
    fn wait(&self) {
        let started = Instant::now();
        let mut quiet_for = Duration::ZERO;
        // bounded by max_latency: a folder that never goes quiet (a long
        // copy) still gets a pass every max_latency
        loop {
            std::thread::sleep(Duration::from_millis(50));
            if self.pending.swap(false, Ordering::Relaxed) {
                quiet_for = Duration::ZERO; // still churning
            } else {
                quiet_for += Duration::from_millis(50);
                if quiet_for >= self.debounce {
                    return;
                }
            }
            if started.elapsed() >= self.max_latency {
                return;
            }
        }
    }
}

/// What woke the watcher.
#[derive(Debug, Clone, PartialEq)]
enum Wakeup {
    /// the folder changed (OS events, or the poll fallback's tick)
    Events,
    /// the change feed says main is at this commit (`None`: no commit yet)
    Head(Option<String>),
    /// a feed frame this CLI does not read: taken as a move
    Moved,
    /// the periodic sweep
    Sweep,
}

/// What one batch of wakeups asks of the watcher.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
struct Due {
    /// the folder may have changed
    local: bool,
    /// main moved somewhere a pass has not seen
    remote: bool,
    sweep: bool,
}

/// What the watcher did with a batch.
#[derive(Debug)]
enum Served {
    /// nothing to do: echoes, or OS events that changed nothing that syncs
    Nothing,
    /// an idle sweep: the folder matched its journal and main its last head
    Checked,
    Passed(Box<Report>),
}

/// One folder's watcher, between passes.
struct Watcher<'a> {
    client: &'a Client,
    name: &'a str,
    dir: &'a Path,
    opts: &'a SyncOptions,
    /// one code.storage client and token for the watcher's life
    storage: Held,
    /// main's head as the last pass left it: the feed's frame naming it is
    /// our own commit's echo, or a move a pass already took
    head: Option<String>,
}

impl<'a> Watcher<'a> {
    fn new(client: &'a Client, name: &'a str, dir: &'a Path, opts: &'a SyncOptions) -> Watcher<'a> {
        Watcher { client, name, dir, opts, storage: Held::new(name, opts.codestorage.as_deref()), head: None }
    }

    /// Folds one wakeup into its batch.
    fn note(&self, due: &mut Due, wakeup: Wakeup) {
        match wakeup {
            Wakeup::Events => due.local = true,
            Wakeup::Head(sha) if sha == self.head => {}
            Wakeup::Head(_) | Wakeup::Moved => due.remote = true,
            Wakeup::Sweep => due.sweep = true,
        }
    }

    /// Serves a batch. A full pass runs when main moved or the folder
    /// differs from its journal; a sweep also passes when the feed is down
    /// (`live_up` false) or one head read finds main moved. Anything else
    /// costs no request: an OS event is a local scan, a feed echo nothing.
    fn serve(&mut self, due: Due, live_up: bool) -> Result<Served, SyncError> {
        if !(due.local || due.remote || due.sweep) {
            return Ok(Served::Nothing);
        }
        let local = sync::read_local(self.dir, self.name, self.opts)?;
        let full = due.remote || !local.matches_journal() || (due.sweep && (!live_up || self.head_moved()?));
        if !full {
            return Ok(if due.sweep { Served::Checked } else { Served::Nothing });
        }
        let (client, name, dir, opts) = (self.client, self.name, self.dir, self.opts);
        let passed = self.storage.get(client).map_err(SyncError::from).and_then(|storage| sync::pass_over(client, storage, name, dir, opts, local));
        let report = self.refused_on_auth(passed)?;
        if report.landed {
            sync::refresh_pins(client, name);
        }
        self.head = report.head.clone();
        Ok(Served::Passed(Box::new(report)))
    }

    /// Whether main moved from the head the last pass left: one request.
    fn head_moved(&mut self) -> Result<bool, SyncError> {
        let read = self.storage.get(self.client).and_then(|storage| storage.branch_head(MAIN)).map_err(SyncError::from);
        Ok(self.refused_on_auth(read)? != self.head)
    }

    /// A token code.storage refused is minted again by the next `get`.
    fn refused_on_auth<T>(&mut self, answer: Result<T, SyncError>) -> Result<T, SyncError> {
        if matches!(answer, Err(SyncError::Cs(CsError::Auth(_)))) {
            self.storage.refused();
        }
        answer
    }
}

/// Blocks for the next wakeup (or the sweep's deadline), lets a burst
/// settle, then folds everything queued into one batch: a burst of saves
/// spanning a pass is one more pass, not one per save.
fn next_batch(rx: &Receiver<Wakeup>, gate: &Debounced, watcher: &Watcher<'_>) -> Due {
    let first = match rx.recv_timeout(Duration::from_secs(RESCAN_SECS)) {
        Ok(wakeup) => wakeup,
        Err(RecvTimeoutError::Timeout) => Wakeup::Sweep,
        Err(RecvTimeoutError::Disconnected) => unreachable!("run holds a sender for as long as it loops"),
    };
    if first != Wakeup::Sweep {
        gate.wait(); // let a burst settle into one pass
    }
    let mut due = Due::default();
    watcher.note(&mut due, first);
    for wakeup in rx.try_iter().take(WAKEUPS_BATCH_MAX) {
        watcher.note(&mut due, wakeup);
    }
    due
}

pub fn run(client: &Client, name: &str, dir: &Path, opts: &SyncOptions, cfg: &WatchConfig) -> Result<()> {
    let _lock = SyncLock::acquire(dir)?; // one watcher per folder, ever
    let debounce = Duration::from_millis(DEBOUNCE_MS);
    let gate = Debounced { pending: Arc::new(AtomicBool::new(false)), debounce, max_latency: debounce * 3 };

    // channel of wakeups; `tx` lives as long as the loop below
    let (tx, rx) = std::sync::mpsc::channel::<Wakeup>();

    // ---- backend 1: OS events (with poll fallback) ----
    let mut backend_in_use = "native";
    let _watcher = match spawn_native(dir, tx.clone(), gate.pending.clone()) {
        Ok(w) => Some(w),
        Err(e) => {
            eprintln!("warning: native watcher unavailable ({e}); falling back to polling every {}s", POLL_INTERVAL_SECS);
            backend_in_use = "poll";
            let tick = tx.clone();
            std::thread::spawn(move || loop {
                std::thread::sleep(Duration::from_secs(POLL_INTERVAL_SECS));
                let _ = tick.send(Wakeup::Events);
            });
            None
        }
    };

    // ---- backend 2: live channel from the cell ----
    let live_up = Arc::new(AtomicBool::new(false));
    let mut live_state = "off";
    if cfg.live {
        let host = client.host.trim_end_matches('/').to_string();
        let url = match view_token(client, name) {
            Some(t) => format!("{}/f/{}/__watch?view={}", host.replace("http", "ws"), name, t),
            None => format!("{}/f/{}/__watch", host.replace("http", "ws"), name),
        };
        let (signer, feed, up) = (client.id.clone(), tx.clone(), live_up.clone());
        std::thread::spawn(move || live_listener(&url, &signer, feed, up));
        live_state = "connecting";
    }

    println!(
        "sync {name} ({}) — watch: {backend_in_use}, live: {live_state}, sweep: every {}s",
        dir.display(),
        RESCAN_SECS
    );

    // ---- main loop: a batch of wakeups → at most one pass ----
    let mut watcher = Watcher::new(client, name, dir, opts);
    // the first pass runs at once: nothing is known yet
    let mut due = Due { remote: true, ..Due::default() };
    let mut err_backoff = 1u64;
    loop {
        // a transient sync error must not exit: under launchd KeepAlive an
        // exiting watcher respawns into a full TLS + scan cycle every few
        // seconds for as long as the host has a bad window (found live:
        // banner/error churn in watch.log during a server blip). Log it,
        // back off, keep the process and its warm watcher alive.
        match watcher.serve(due, live_up.load(Ordering::Relaxed)) {
            Ok(served) => {
                err_backoff = 1;
                if let Served::Passed(report) = served {
                    if !report.pulled.is_empty() || !report.pushed.is_empty() || !report.deleted_remote.is_empty() || !report.deleted_local.is_empty() || !report.conflicts.is_empty() {
                        let at = chrono_like();
                        println!("{at} pushed {} pulled {} deleted {} conflicts {}", report.pushed.len(), report.pulled.len(), report.deleted_remote.len() + report.deleted_local.len(), report.conflicts.len());
                    }
                }
            }
            Err(e) => {
                let at = chrono_like();
                eprintln!("{at} sync failed (retrying in {err_backoff}s): {e}");
                std::thread::sleep(Duration::from_secs(err_backoff));
                err_backoff = (err_backoff * 2).min(60);
            }
        }
        due = next_batch(&rx, &gate, &watcher);
        // a failed pass is retried by the next batch: the folder still
        // differs from its journal, or main from the head we hold
    }
}

fn view_token(client: &Client, name: &str) -> Option<String> {
    let status: fragment_proto::FragmentStatus = client.call_as(client.get(&format!("/api/f/{name}/status")).ok()?).ok()?;
    status.view_token
}

/// native OS watcher; the boxed return keeps the watcher alive (dropping it
/// stops events)
fn spawn_native(dir: &Path, tx: Sender<Wakeup>, pending: Arc<AtomicBool>) -> Result<Box<dyn Send>> {
    use notify_debouncer_full::{new_debouncer, DebounceEventResult};
    let dir_owned: PathBuf = dir.to_path_buf();
    // the folder as given, and as the OS may report it (macOS names
    // /var/… by its real path, /private/var/…)
    let roots = vec![dir.to_path_buf(), dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf())];
    let mut debouncer = new_debouncer(
        Duration::from_millis(200),
        None,
        move |res: DebounceEventResult| {
            if let Ok(events) = res {
                if events.iter().any(|e| relevant(&roots, &e.paths)) {
                    pending.store(true, Ordering::Relaxed);
                    let _ = tx.send(Wakeup::Events);
                }
            }
        },
    )
    .context("start watcher")?;
    debouncer.watch(dir_owned, notify::RecursiveMode::Recursive)
        .context("watch folder")?;
    Ok(Box::new(debouncer))
}

/// The change feed's frames (docs/api.md, `__watch`): `{type: "hello", ref,
/// sha}` on connect, then `{type: "changed", ref: "main", sha, paths}` per
/// move of main. Only where main is matters here.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Frame {
    Hello { sha: Option<String> },
    Changed { sha: Option<String> },
}

fn wakeup_of(frame: &str) -> Wakeup {
    match serde_json::from_str::<Frame>(frame) {
        Ok(Frame::Hello { sha } | Frame::Changed { sha }) => Wakeup::Head(sha),
        // a frame from a newer cell: a pass finds out what it meant
        Err(_) => Wakeup::Moved,
    }
}

/// the cell's change channel: frames arrive per remote mutation; reconnect
/// with backoff forever (degradation is reported by the sweep still
/// working). `up` says whether the socket is open: a sweep while it is not
/// runs a full pass.
fn live_listener(url: &str, signer: &crate::auth::Identity, tx: Sender<Wakeup>, up: Arc<AtomicBool>) {
    use tungstenite::client::IntoClientRequest;
    let mut backoff = 1u64;
    loop {
        // build from the URL so tungstenite generates the handshake headers;
        // a hand-built Request skips them and the server rejects the upgrade
        let mut req = match url.into_client_request() {
            Ok(r) => r,
            Err(_) => return,
        };
        // signed fresh on every connect (NIP-98 events are good for a
        // minute): members-only fragments have no share link to present.
        // The server sees the upgrade as a GET of the http(s) URL.
        let http = url.replacen("ws", "http", 1);
        if let Ok(v) = signer.nip98_header("GET", &http, &[]).parse() {
            req.headers_mut().insert("authorization", v);
        }
        if let Ok((mut socket, _)) = tungstenite::connect(req) {
            backoff = 1;
            up.store(true, Ordering::Relaxed);
            loop {
                match socket.read() {
                    Ok(tungstenite::Message::Text(frame)) => {
                        let _ = tx.send(wakeup_of(&frame));
                    }
                    Ok(tungstenite::Message::Close(_)) => break,
                    Err(_) => break,
                    Ok(_) => {}
                }
            }
            up.store(false, Ordering::Relaxed);
        }
        // cap the value itself, not just the sleep: an uncapped doubling
        // overflows u64 after ~64 reconnects, wraps to 0, and sleep(0)
        // hot-spins the thread at 100% CPU forever (found live: a watcher
        // left pegging a core for hours after a morning of reconnects)
        std::thread::sleep(Duration::from_secs(backoff));
        backoff = (backoff * 2).min(30);
    }
}

fn chrono_like() -> String {
    // UTC wall-clock log prefix; the old hand-roll walked leap years to
    // produce a day-of-year it then discarded.
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("{:02}:{:02}:{:02}", (secs % 86400) / 3600, (secs % 3600) / 60, secs % 60)
}

/// Streams a channel's records as JSON lines from the fragment's live
/// socket, a page of backlog at a time until it is live, resuming after
/// the last record seen when the socket drops. Frames are
/// `fragment_proto::live`'s: each line printed is a record frame as sent.
pub fn follow_channel(client: &Client, name: &str, channel: &str, after: i64) -> Result<()> {
    use fragment_proto::live::{Cursor, LiveIn, LiveOut, Subscribe};
    use tungstenite::client::IntoClientRequest;
    let http = format!("{}/f/{name}/__live", client.host.trim_end_matches('/'));
    let ws_url = http.replacen("http", "ws", 1);
    let subscribe = |after: i64| LiveIn::Subscribe(Subscribe { channel: channel.to_string(), from: Cursor::After(after) }).encode();
    let mut last = after;
    let mut backoff = 1u64;
    loop {
        let mut req = ws_url.as_str().into_client_request().context("the live socket URL")?;
        req.headers_mut().insert("authorization", client.id.nip98_header("GET", &http, &[]).parse().context("auth header")?);
        match tungstenite::connect(req) {
            Ok((mut socket, _)) => {
                backoff = 1;
                socket.send(tungstenite::Message::Text(subscribe(last).into()))?;
                loop {
                    match socket.read() {
                        Ok(tungstenite::Message::Text(t)) => match serde_json::from_str::<LiveOut>(&t) {
                            Ok(LiveOut::Record(r)) if r.channel == channel => {
                                last = r.seq;
                                println!("{t}");
                            }
                            // a page at a time: the socket follows the channel live
                            // only once a page reaches the end
                            Ok(LiveOut::Subscribed { channel: c, next, more: true }) if c == channel => {
                                last = next.max(last);
                                socket.send(tungstenite::Message::Text(subscribe(last).into()))?;
                            }
                            Ok(LiveOut::Error { message }) => anyhow::bail!("the live socket refused: {message}"),
                            Ok(_) => {}
                            // a frame from a newer fragment: records still come as records
                            Err(e) => eprintln!("warning: a live frame this CLI does not read: {e}"),
                        },
                        Ok(tungstenite::Message::Close(_)) | Err(_) => break,
                        Ok(_) => {}
                    }
                }
            }
            Err(tungstenite::Error::Http(resp)) if resp.status() == 401 || resp.status() == 403 || resp.status() == 404 => {
                let body = resp.body().as_deref().map(String::from_utf8_lossy).unwrap_or_default().to_string();
                anyhow::bail!("the live socket refused ({}): {body}", resp.status());
            }
            Err(_) => {}
        }
        std::thread::sleep(Duration::from_secs(backoff));
        backoff = (backoff * 2).min(30);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth;
    use crate::mockcs::MockServer;
    use crate::sync::Mode;
    use std::collections::BTreeMap;
    use std::fs;

    /// Goal: an editor's own state is not a change, however an event
    /// names it. Method: `relevant` over events' path lists; the old rule
    /// tested `.obsidian/workspace` against one path segment, so it never
    /// matched, and it read only an event's first path.
    #[test]
    fn an_editors_workspace_write_is_not_a_change() {
        let root = PathBuf::from("/tmp/folder");
        let roots = [root.clone()];
        assert!(!relevant(&roots, &[root.join(".obsidian/workspace.json")]));
        assert!(!relevant(&roots, &[root.join(".DS_Store"), root.join("node_modules/a/b.js"), root.join(".fragment/state.json")]));
        assert!(relevant(&roots, &[root.join(".obsidian/workspace.json"), root.join("notes/a.md")]), "every path of an event counts");
        assert!(relevant(&roots, &[root.join("notes/a.md")]));
        assert!(relevant(&roots, &[]), "a rescan names no path");
        assert!(relevant(&roots, &[PathBuf::from("/elsewhere/a.md")]), "a path outside the folder counts");
    }

    fn counts(routes: &[(&str, u32)]) -> BTreeMap<String, u32> {
        routes.iter().map(|(route, n)| (route.to_string(), *n)).collect()
    }

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("fragment-watch-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn opts() -> SyncOptions {
        SyncOptions { mode: Mode::Mirror, writer_id: "deadbeef".into(), ..Default::default() }
    }

    const REMOTE: Due = Due { local: false, remote: true, sweep: false };
    const LOCAL: Due = Due { local: true, remote: false, sweep: false };
    const SWEEP: Due = Due { local: false, remote: false, sweep: true };

    /// Goal: the change feed's frames decode to where main is, and one this
    /// CLI cannot read counts as a move.
    #[test]
    fn feed_frames_say_where_main_is() {
        let sha = "a".repeat(40);
        assert_eq!(wakeup_of(&format!(r#"{{"type":"hello","ref":"main","sha":"{sha}"}}"#)), Wakeup::Head(Some(sha.clone())));
        assert_eq!(wakeup_of(&format!(r#"{{"type":"changed","ref":"main","sha":"{sha}","paths":["a.md"]}}"#)), Wakeup::Head(Some(sha)));
        assert_eq!(wakeup_of(r#"{"type":"hello","ref":"main","sha":null}"#), Wakeup::Head(None));
        assert_eq!(wakeup_of(r#"{"type":"presence"}"#), Wakeup::Moved);
        assert_eq!(wakeup_of("not json"), Wakeup::Moved);
    }

    /// Goal: a burst of wakeups queued while a pass ran is one batch, and
    /// the feed's echo of the head we hold asks for nothing. Method: queue
    /// five OS events and an echo, and take one batch.
    #[test]
    fn a_burst_is_one_batch_and_an_echo_asks_nothing() {
        let (tx, rx) = std::sync::mpsc::channel();
        let gate = Debounced { pending: Arc::new(AtomicBool::new(false)), debounce: Duration::ZERO, max_latency: Duration::ZERO };
        let c = Client::new("http://127.0.0.1:9", auth::fixed(7));
        let dir = tmpdir("burst");
        let o = opts();
        let mut watcher = Watcher::new(&c, "t", &dir, &o);
        watcher.head = Some("b".repeat(40));
        for _ in 0..5 {
            tx.send(Wakeup::Events).unwrap();
        }
        tx.send(Wakeup::Head(Some("b".repeat(40)))).unwrap();
        assert_eq!(next_batch(&rx, &gate, &watcher), LOCAL);
        assert!(rx.try_recv().is_err(), "the batch took every queued wakeup");
        tx.send(Wakeup::Head(Some("b".repeat(40)))).unwrap();
        assert_eq!(next_batch(&rx, &gate, &watcher), Due::default(), "an echo alone asks nothing");
        tx.send(Wakeup::Head(Some("c".repeat(40)))).unwrap();
        assert_eq!(next_batch(&rx, &gate, &watcher), REMOTE);
        fs::remove_dir_all(&dir).ok();
    }

    /// Goal: one local write is one pass (one head read, one listing, one
    /// commit), and neither its echoes nor an editor's workspace write
    /// start another. Method: a watcher over the fake, served the batches
    /// a save makes, counting the fake's requests. Before, each wakeup was
    /// a pass of its own (a token, three head reads, two listings), and the
    /// feed's frame for our own commit started one more.
    #[test]
    fn one_local_write_is_one_pass_and_its_echoes_none() {
        let mock = MockServer::start();
        mock.seed_repo("t", &[("a.md", b"a")]);
        let c = Client::new(&mock.url, auth::fixed(7));
        let dir = tmpdir("one-write");
        let o = opts();
        let mut watcher = Watcher::new(&c, "t", &dir, &o);
        assert!(matches!(watcher.serve(REMOTE, true).unwrap(), Served::Passed(r) if r.pulled == ["a.md"]));
        mock.take_requests("");

        fs::write(dir.join("b.md"), "b").unwrap();
        let Served::Passed(report) = watcher.serve(LOCAL, true).unwrap() else { panic!("a local write is a pass") };
        assert_eq!(report.pushed, ["b.md"]);
        assert_eq!(
            mock.take_requests(""),
            counts(&[("GET branch", 1), ("GET files/metadata", 1), ("POST commit-pack", 1), ("POST refresh", 1)]),
            "one pass, with the watcher's token"
        );

        // the feed's frame for our own commit, and the folder's events
        let mut due = Due::default();
        watcher.note(&mut due, Wakeup::Head(mock.branch("t", "main")));
        watcher.note(&mut due, Wakeup::Events);
        assert!(matches!(watcher.serve(due, true).unwrap(), Served::Nothing));
        // an editor's workspace state changes nothing that syncs
        fs::create_dir_all(dir.join(".obsidian")).unwrap();
        fs::write(dir.join(".obsidian/workspace.json"), "{}").unwrap();
        assert!(matches!(watcher.serve(LOCAL, true).unwrap(), Served::Nothing));
        assert_eq!(mock.take_requests(""), counts(&[]), "no request for echoes or editor state");
        fs::remove_dir_all(&dir).ok();
    }

    /// Goal: an idle sweep costs one head read and writes no journal; a
    /// sweep still finds a move the feed never announced, and with the
    /// feed down it runs the full pass. Method: sweeps over the fake,
    /// counting requests and watching the journal's inode.
    #[cfg(unix)]
    #[test]
    fn an_idle_sweep_reads_one_head_and_a_missed_move_is_pulled() {
        use std::os::unix::fs::MetadataExt;
        let mock = MockServer::start();
        mock.seed_repo("t", &[("a.md", b"a")]);
        let c = Client::new(&mock.url, auth::fixed(7));
        let dir = tmpdir("sweep");
        let o = opts();
        let mut watcher = Watcher::new(&c, "t", &dir, &o);
        watcher.serve(REMOTE, true).unwrap();
        let journal = || fs::metadata(dir.join(".fragment/state.json")).unwrap().ino();
        let written = journal();
        mock.take_requests("");

        assert!(matches!(watcher.serve(SWEEP, true).unwrap(), Served::Checked));
        assert_eq!(mock.take_requests(""), counts(&[("GET branch", 1)]), "an idle sweep is one head read");
        assert_eq!(journal(), written, "and writes no journal");

        // a commit whose frame never came: the sweep's head read finds it
        mock.silent_commit("t", "main", &[("b.md", Some(b"b"))], "missed");
        assert!(matches!(watcher.serve(SWEEP, true).unwrap(), Served::Passed(r) if r.pulled == ["b.md"]));
        assert_eq!(fs::read(dir.join("b.md")).unwrap(), b"b");

        // with the feed down, a sweep is a full pass: a commit made while
        // the socket was down is pulled within one sweep
        mock.take_requests("");
        mock.external_commit("t", "main", &[("c.md", Some(b"c"))], "while down");
        assert!(matches!(watcher.serve(SWEEP, false).unwrap(), Served::Passed(r) if r.pulled == ["c.md"]));
        // one listing: three files are two of the fake's pages
        assert_eq!(mock.take_requests(""), counts(&[("GET branch", 1), ("GET files/metadata", 2), ("GET file", 1)]));
        let written = journal();
        assert!(matches!(watcher.serve(SWEEP, false).unwrap(), Served::Passed(r) if r.pulled.is_empty()));
        assert_eq!(journal(), written, "a pass that changes nothing writes no journal");
        fs::remove_dir_all(&dir).ok();
    }

    /// Goal: a token code.storage refuses is minted again, and until then
    /// the watcher's token serves every pass. Method: revoke the fake's
    /// tokens between passes.
    #[test]
    fn a_refused_token_is_minted_again_by_the_next_pass() {
        let mock = MockServer::start();
        mock.seed_repo("t", &[("a.md", b"a")]);
        let c = Client::new(&mock.url, auth::fixed(7));
        let dir = tmpdir("refused");
        let o = opts();
        let mut watcher = Watcher::new(&c, "t", &dir, &o);
        watcher.serve(REMOTE, true).unwrap();
        fs::write(dir.join("b.md"), "b").unwrap();
        watcher.serve(LOCAL, true).unwrap();
        assert_eq!(mock.take_requests("").get("GET storage-token"), Some(&1), "one token for both passes");

        mock.revoke_tokens();
        fs::write(dir.join("c.md"), "c").unwrap();
        assert!(matches!(watcher.serve(LOCAL, true), Err(SyncError::Cs(CsError::Auth(_)))));
        let Served::Passed(report) = watcher.serve(LOCAL, true).unwrap() else { panic!("the retry passes") };
        assert_eq!(report.pushed, ["c.md"]);
        assert_eq!(mock.take_requests("").get("GET storage-token"), Some(&1), "a new token after the refusal");
        fs::remove_dir_all(&dir).ok();
    }
}
