// The pet: the latest frame of its computer's screen, and who drove it
// last. The computer (computer/pet.mjs, an editor here) stores each frame
// through `frame`, and every page follows `screen` live. People drive it
// by posting to `control`, which the computer follows. Nothing here keeps
// history: one row, replaced. Its agent (`do`) drives it too: goose on the
// computer with Cua Driver as its hands (computer/do.mjs).
import { DurableObject } from "cloudflare:workers";

// goose's CLI and Cua Driver, each a release at least two weeks old (the
// dependency cooldown), checked against the SHA-256 GitHub lists for it
const GOOSE_VERSION = "1.50.0";
const GOOSE_URL = `https://github.com/aaif-goose/goose/releases/download/v${GOOSE_VERSION}/goose-x86_64-unknown-linux-musl.tar.gz`;
const GOOSE_SHA256 = "ff8c51428180142e5c92e2a0b67b3d1762698499d028f6f5fe370fdbec8c84af";
const CUA_VERSION = "0.28.1";
const CUA_URL = `https://github.com/trycua/cua/releases/download/cua-driver-rs-v${CUA_VERSION}/cua-driver-rs-${CUA_VERSION}-linux-x86_64-binary.tar.gz`;
const CUA_SHA256 = "71aa92533de90a68a0a2af930243f1770d23e45b896b57d67a1763da4bfaeaf7";
// each release unpacked whole, as it ships, under the computer's home
const GOOSE_DIR = `.local/share/goose-${GOOSE_VERSION}`;
const CUA_DIR = `.local/share/cua-driver-${CUA_VERSION}`;
const GOOSE_BIN = `${GOOSE_DIR}/goose`;
const CUA_BIN = `${CUA_DIR}/cua-driver`;
// the cap on the agent's run (the exec ends it), and on installing
const DO_MS = 10 * 60 * 1000;
const STEP_MS = 3 * 60 * 1000;

// each once: fetched, checked, unpacked beside its place and then moved
// there; then both answer --version (Cua Driver needs libXi, which
// computer/pet.mjs installs with the display)
const INSTALL = `set -eu
export CUA_DRIVER_RS_TELEMETRY_ENABLED=false
get() {
  [ -d "$HOME/$3" ] && return
  mkdir -p "$HOME/.local/share" && t=$(mktemp -d "$HOME/.local/share/.get-XXXXXX")
  curl -fsSL -o "$t/a.tar.gz" "$1"
  echo "$2  $t/a.tar.gz" | sha256sum -c - > /dev/null
  mkdir "$t/x" && tar -xzf "$t/a.tar.gz" -C "$t/x" && mv "$t/x" "$HOME/$3" && rm -rf "$t"
}
get '${GOOSE_URL}' ${GOOSE_SHA256} '${GOOSE_DIR}'
get '${CUA_URL}' ${CUA_SHA256} '${CUA_DIR}'
fragment model --help | grep -q -- --serve || { echo "this computer's fragment CLI predates 'model --serve': install its latest release" >&2; exit 1; }
"$HOME/${GOOSE_BIN}" --version && "$HOME/${CUA_BIN}" --version`;

// a command's answer, or why it failed
function ok(out, what) {
  if (out.code !== 0) throw new Error(`${what} failed (${out.code}): ${(out.stderr || out.stdout).slice(-500)}`);
  return out;
}

export class App extends DurableObject {
  constructor(ctx, env) {
    super(ctx, env);
    ctx.storage.sql.exec(`CREATE TABLE IF NOT EXISTS screen (
      id INTEGER PRIMARY KEY CHECK (id = 1), jpeg TEXT NOT NULL, width INTEGER NOT NULL, height INTEGER NOT NULL,
      title TEXT NOT NULL, driver TEXT, at INTEGER NOT NULL)`);
  }

  frame({ jpeg, width, height, title = "", driver = null }) {
    // a JPEG's first bytes (FF D8 FF), in base64
    if (!jpeg.startsWith("/9j/")) throw new Error("a frame is a base64 JPEG");
    const at = Date.now();
    // a frame that names no driver (its computer just started) keeps the last one
    this.ctx.storage.sql.exec(
      `INSERT INTO screen (id, jpeg, width, height, title, driver, at) VALUES (1, ?, ?, ?, ?, ?, ?)
       ON CONFLICT (id) DO UPDATE SET jpeg = excluded.jpeg, width = excluded.width, height = excluded.height,
         title = excluded.title, driver = coalesce(excluded.driver, screen.driver), at = excluded.at`,
      jpeg, width, height, title, driver, at,
    );
    return { at };
  }

  // the screen as the computer last sent it, or null before its first frame
  screen() {
    return this.ctx.storage.sql.exec("SELECT jpeg, width, height, title, driver, at FROM screen").toArray()[0] ?? null;
  }

  // A job, for editors (the owner, and their agent, capped at editor): one
  // command on the computer, as the computer, answered once it ended:
  // {code, stdout, stderr, truncated}. It wakes the computer as a page does.
  async run({ command }, job) {
    return job.computer.exec(command);
  }

  // A job, for editors (it spends its owner's budget): the pet's agent does
  // `task` on its screen, which everyone here watches and may click too.
  // Each step is durable: goose and Cua Driver are installed once (pinned,
  // checked), then computer/do.mjs runs goose headless for at most 10
  // minutes, posting each step to `work`. `work` also gets the run's start
  // and end. Answers {message, code}: goose's last words and its exit code.
  async do({ task }, job) {
    const exec = (command, opts) => job.computer.exec(command, { timeout: STEP_MS, ...opts });
    await job.publish("work", { run: job.run, kind: "start", task, asker: job.principal });
    try {
      ok(await exec(INSTALL), "installing goose and Cua Driver");
      const env = { TASK: task, RUN: String(job.run), ASKER: job.principal, GOOSE_BIN, CUA_BIN };
      const run = await exec("node computer/do.mjs", { timeout: DO_MS, env });
      const message = run.stdout.trim().slice(-2000) || run.stderr.trim().slice(-2000);
      await job.publish("work", { run: job.run, kind: "end", code: run.code, message });
      return { message, code: run.code };
    } catch (e) {
      await job.publish("work", { run: job.run, kind: "end", error: String(e?.message ?? e).slice(0, 2000) });
      throw e;
    }
  }
}
