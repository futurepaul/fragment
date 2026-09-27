// The pet's computer: a virtual display with a browser on it, driven by the
// fragment's `control` channel and shown through its `frame` operation. The
// platform runs this from ~/fragment (the fragment's live files) while the
// computer is awake, as fragment.json's `computer.start` says, and restarts
// it when it exits and after a deploy; what it prints is in ~/fragment.log.
// The `fragment` CLI here is signed in as the computer, an editor, and
// FRAGMENT_NAME names the fragment. Its agent (the `do` job: the computer's
// hands, with Cua Driver) drives the same display, and marks each step it
// takes in ~/.pet/agent, which makes it the driver the frames name.
//
// PET_FAKE_SCREEN=<a JPEG> shows that fixed image instead: nothing is
// installed or started, and control records are logged, not applied.
import { execFile, execFileSync, spawn, spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { createInterface } from "node:readline";
import { promisify } from "node:util";

const SCREEN = { width: 1024, height: 640 };
const DISPLAY = ":99";
// a frame at most each second for a minute after someone drives, else at
// most each 5 seconds; and only when the screen changed
const FAST_MS = 1000;
const SLOW_MS = 5000;
const DRIVEN_MS = 60_000;
// the longest JPEG `frame` takes (its input schema), as base64
const FRAME_MAX_CHARS = 180_000;
// a control record older than this is skipped: its poster saw another screen
const STALE_MS = 30_000;
const KEYS = { Enter: "Return", Backspace: "BackSpace", Escape: "Escape", Tab: "Tab", ArrowUp: "Up", ArrowDown: "Down", ArrowLeft: "Left", ArrowRight: "Right" };
// with what Cua Driver needs: libXi, and AT-SPI 2 on a session bus for the browser's accessibility tree
const APT = ["xvfb", "openbox", "xdotool", "imagemagick", "fonts-liberation", "fonts-noto-color-emoji", "libxi6", "at-spi2-core", "dbus"];
const PLAYWRIGHT = "playwright@1.63.0";

const FAKE = process.env.PET_FAKE_SCREEN;
const CLI = process.env.FRAGMENT_BIN ?? "fragment";
const NAME = process.env.FRAGMENT_NAME ?? fail("FRAGMENT_NAME names the fragment (the platform sets it)");
// the control cursor, the frame being sent, pid files, the display's and
// browser's logs, the browser's profile
const STATE = path.join(os.homedir(), ".pet");
const CURSOR = path.join(STATE, "applied");
const FRAME = path.join(STATE, "frame.json");
const BUS = path.join(STATE, "bus");
const AGENT = path.join(STATE, "agent");
const START = `file://${path.resolve("computer/start.html")}`;

const log = (...parts) => console.log(new Date().toISOString(), ...parts);
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const run = promisify(execFile);
const has = (cmd) => spawnSync("sh", ["-c", 'command -v "$0"', cmd]).status === 0;
const kids = {};
let chrome;
let shot;
let sandbox = true;
let applied = Number(fs.existsSync(CURSOR) && fs.readFileSync(CURSOR, "utf8")) || 0;
let driver = null;
// someone just opened the page (that is what woke the computer): quick frames
let droveAt = Date.now();

function install() {
  if (spawnSync("dpkg", ["-s", ...APT], { stdio: "ignore" }).status === 0) return;
  log(`installing ${APT.join(" ")}`);
  const apt = (...args) => execFileSync("sudo", ["-n", "env", "DEBIAN_FRONTEND=noninteractive", "apt-get", "-q", "-y", ...args], { stdio: ["ignore", "inherit", "inherit"] });
  apt("update");
  apt("install", "--no-install-recommends", ...APT);
}

// A browser that runs here: Playwright's Chromium, with the libraries it
// needs (apt), since Ubuntu packages its own as a snap.
function browserPath() {
  const runs = (bin) => spawnSync(bin, ["--version"], { timeout: 10_000 }).status === 0;
  const find = () => [process.env.PET_BROWSER, ...playwrights()].find((b) => b && runs(b));
  if (!find()) {
    log(`installing Chromium (${PLAYWRIGHT})`);
    execFileSync("npx", ["-y", PLAYWRIGHT, "install", "--with-deps", "chromium"], { stdio: ["ignore", "inherit", "inherit"] });
  }
  return find() ?? fail("no browser runs here");
}

function playwrights() {
  const root = path.join(os.homedir(), ".cache/ms-playwright");
  if (!fs.existsSync(root)) return [];
  return fs.readdirSync(root, { recursive: true }).filter((p) => p.startsWith("chromium-") && path.basename(p) === "chrome").map((p) => path.join(root, p));
}

function fail(why) {
  throw new Error(why);
}

// A child of this run, its output in ~/.pet/<label>.log unless `stdio` says
// otherwise. Its pid is kept, so the next run ends it if this one could not.
function daemon(label, cmd, args, stdio) {
  const out = stdio ? null : fs.openSync(path.join(STATE, `${label}.log`), "a");
  const child = spawn(cmd, args, { stdio: stdio ?? ["ignore", out, out] });
  if (out !== null) fs.closeSync(out);
  fs.writeFileSync(path.join(STATE, `${label}.pid`), String(child.pid));
  child.on("exit", (code, signal) => {
    child.gone = true;
    log(`${label} exited (${signal ?? code})`);
  });
  return (kids[label] = child);
}

function browser() {
  const started = Date.now();
  const flags = ["--no-first-run", "--no-default-browser-check", "--disable-dev-shm-usage", "--password-store=basic", "--hide-crash-restore-bubble", "--start-maximized", "--force-renderer-accessibility"];
  const child = daemon("browser", chrome, [...flags, `--user-data-dir=${path.join(STATE, "browser")}`, ...(sandbox ? [] : ["--no-sandbox"]), START]);
  child.on("exit", () => {
    // a kernel that does not allow its sandbox ends it at once
    if (sandbox && Date.now() - started < 10_000) {
      sandbox = false;
      log("the browser's sandbox did not start here: it runs without one");
    }
  });
}

// What the run before this one left (the platform restarts it on exit and
// after a deploy).
function reap() {
  for (const label of ["follow", "browser", "openbox", "xvfb", "bus"]) {
    try {
      process.kill(Number(fs.readFileSync(path.join(STATE, `${label}.pid`), "utf8")), "SIGTERM");
    } catch {}
  }
}

function stop(code) {
  for (const child of Object.values(kids)) child.kill();
  process.exit(code);
}

async function desktop() {
  await sleep(1000);
  // a display's lock a pause left behind
  for (const f of ["/tmp/.X99-lock", "/tmp/.X11-unix/X99"]) fs.rmSync(f, { force: true });
  daemon("xvfb", "Xvfb", [DISPLAY, "-screen", "0", `${SCREEN.width}x${SCREEN.height}x24`, "-nolisten", "tcp"]);
  for (let i = 0; i < 50 && spawnSync("xdotool", ["getmouselocation"]).status !== 0; i++) await sleep(200);
  // a session bus, where the browser's accessibility tree reaches AT-SPI (Cua Driver reads it)
  if (has("dbus-daemon")) {
    fs.rmSync(BUS, { force: true });
    daemon("bus", "dbus-daemon", ["--session", "--nofork", "--nopidfile", `--address=unix:path=${BUS}`]);
    for (let i = 0; i < 25 && !fs.existsSync(BUS); i++) await sleep(200);
    process.env.DBUS_SESSION_BUS_ADDRESS = `unix:path=${BUS}`;
  }
  daemon("openbox", "openbox", []);
  // no blinking caret: each blink would be a frame
  const gtk = path.join(os.homedir(), ".config/gtk-3.0/settings.ini");
  if (!fs.existsSync(gtk)) {
    fs.mkdirSync(path.dirname(gtk), { recursive: true });
    fs.writeFileSync(gtk, "[Settings]\ngtk-cursor-blink = false\n");
  }
  browser();
}

async function capture() {
  if (FAKE) return fs.readFileSync(FAKE);
  const [cmd, ...pre] = shot;
  const args = [...pre, "-silent", "-display", DISPLAY, "-window", "root", "-quality", "70", "-define", "jpeg:extent=85kb", "jpeg:-"];
  return (await run(cmd, args, { encoding: "buffer", maxBuffer: 16 << 20 })).stdout;
}

async function title() {
  if (FAKE) return "";
  return run("xdotool", ["getactivewindow", "getwindowname"]).then((o) => o.stdout.trim().slice(0, 300), () => "");
}

// Each change of the screen (or of who drives it) as the fragment's frame.
async function frames() {
  let sent = {};
  for (;;) {
    await sleep(Date.now() - droveAt < DRIVEN_MS ? FAST_MS : SLOW_MS);
    if (!FAKE && (kids.xvfb.gone || kids.openbox.gone)) {
      log("the display stopped: starting over");
      stop(1);
    }
    try {
      // closed from the page: open it again
      if (!FAKE && kids.browser.gone) browser();
      // the agent took a step since anyone else drove it
      const agent = fs.statSync(AGENT, { throwIfNoEntry: false });
      if (agent && agent.mtimeMs > droveAt) {
        droveAt = agent.mtimeMs;
        driver = `agent:${fs.readFileSync(AGENT, "utf8").trim()}`;
      }
      const jpeg = (await capture()).toString("base64");
      const frame = { jpeg, width: SCREEN.width, height: SCREEN.height, title: await title(), ...(driver && { driver }) };
      if (frame.jpeg === sent.jpeg && frame.title === sent.title && frame.driver === sent.driver) continue;
      if (jpeg.length > FRAME_MAX_CHARS) {
        log(`a frame of ${jpeg.length} characters is over ${FRAME_MAX_CHARS}: skipped`);
        continue;
      }
      // from a file: a frame is more than one argument holds (128 KiB)
      fs.writeFileSync(FRAME, JSON.stringify(frame));
      await run(CLI, ["call", NAME, "frame", "--input", `@${FRAME}`]);
      sent = frame;
    } catch (e) {
      log(`frame: ${String(e.stderr || e.message).trim().slice(0, 300)}`);
    }
  }
}

// A control record as xdotool commands, or null when it is not one.
function input(b) {
  const within = (v, max) => Number.isInteger(v) && v >= 0 && v < max;
  switch (b?.kind) {
    case "click":
      return within(b.x, SCREEN.width) && within(b.y, SCREEN.height) ? [["mousemove", "--sync", String(b.x), String(b.y), "click", "1"]] : null;
    case "type":
      return typeof b.text === "string" && b.text.length <= 500 ? [["type", "--delay", "15", "--", b.text]] : null;
    case "key":
      return KEYS[b.key] ? [["key", "--clearmodifiers", KEYS[b.key]]] : null;
    case "open":
      return /^https?:\/\/\S{1,2000}$/i.test(b.url ?? "") ? [["key", "--clearmodifiers", "ctrl+l"], ["type", "--delay", "5", "--", b.url], ["key", "Return"]] : null;
    default:
      return null;
  }
}

function apply({ seq, at, principal, body }) {
  if (!(seq > applied)) return;
  // the cursor moves first: a record is applied at most once, never twice
  applied = seq;
  fs.writeFileSync(CURSOR, String(seq));
  const what = `#${seq} ${JSON.stringify(body).slice(0, 120)}`;
  // (only people signed in post here: `control` says signedIn)
  if (Date.now() - at > STALE_MS) return log(`${what}: stale, skipped`);
  const steps = input(body);
  if (!steps) return log(`${what}: not a control record, ignored`);
  driver = principal;
  droveAt = Date.now();
  if (FAKE) return log(`${what}: not applied (a fake screen)`);
  for (const args of steps) execFileSync("xdotool", args);
  log(`${what}: applied`);
}

function follow() {
  const child = daemon("follow", CLI, ["channel", NAME, "control", "--follow", "--after", String(applied)], ["ignore", "pipe", "inherit"]);
  createInterface({ input: child.stdout }).on("line", (line) => {
    try {
      const frame = JSON.parse(line);
      if (frame.type === "record") apply(frame);
    } catch (e) {
      log(`control: ${e.message}`);
    }
  });
  child.on("exit", () => setTimeout(follow, 5000));
}

fs.mkdirSync(STATE, { recursive: true });
log(`the pet of ${NAME}${FAKE ? `, showing ${FAKE}` : ""}`);
for (const signal of ["SIGTERM", "SIGINT"]) process.on(signal, () => stop(0));
reap();
if (!FAKE) {
  // it installs packages and runs a display: only on its own Sprite
  if (!has("sprite-env")) fail("not on a Sprite: PET_FAKE_SCREEN=<a JPEG> shows that image instead");
  process.env.DISPLAY = DISPLAY;
  install();
  chrome = browserPath();
  shot = has("magick") ? ["magick", "import"] : ["import"];
  await desktop();
}
follow();
frames();
