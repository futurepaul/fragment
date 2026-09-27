// The pet: the latest frame of its computer's screen, and who drove it
// last. The computer (computer/pet.mjs, an editor here) stores each frame
// through `frame`, and every page follows `screen` live. People drive it
// by posting to `control`, which the computer follows. Nothing here keeps
// history: one row, replaced. Its agent (`do`) drives it too: the hands
// every computer has (goose, a session per chat: docs/agent-computer.md),
// with Stagehand on this screen's Chrome, and Cua Driver for other apps.
import { DurableObject } from "cloudflare:workers";

// Cua Driver, a release past the dependency cooldown (two days), checked
// against the SHA-256 GitHub lists for it, unpacked whole, as it ships
const CUA_VERSION = "0.28.3";
const CUA_URL = `https://github.com/trycua/cua/releases/download/cua-driver-rs-v${CUA_VERSION}/cua-driver-rs-${CUA_VERSION}-linux-x86_64-binary.tar.gz`;
const CUA_SHA256 = "51de56e37e1e57cca613cff06e22a3c64bf8285ed97e518660f3d1cb2a4e6fcf";
const CUA_DIR = `.local/share/cua-driver-${CUA_VERSION}`;
// the Cua Driver tools goose offers the model, for desktop apps (it has 62,
// each described at length), and the long edge of the screenshots they show
const TOOLS = ["launch_app", "list_windows", "get_window_state", "click", "type_text", "press_key", "hotkey", "scroll"];
const SHOT_MAX = 768;
// the browser's MCP server and Stagehand (computer/browser: its lockfile pins
// and checks every package), installed once per lockfile
const BROWSER_DIR = ".local/share/pet-browser";
// the cap on the agent's run (the exec ends it), and on installing
const DO_MS = 10 * 60 * 1000;
const STEP_MS = 3 * 60 * 1000;
// what the task says first: this computer's screen, and which tools for what
const SCREEN = `You are on a Linux computer whose 1024×640 screen people watch live and may click too. For anything on the web, use the browser tools: they drive the Chrome on this screen through each page's structure (open an address, then act, extract, or observe; a screenshot only when the look matters). Use the cua tools only for other desktop apps: launch_app or list_windows, get_window_state to see a window and its elements, then click, type_text, press_key, hotkey, or scroll, and look again to check what happened. When the task is done, say in a sentence or two what you did.`;

// Cua Driver once (fetched, checked, unpacked beside its place and then
// moved there; it needs libXi, which computer/pet.mjs installs with the
// display), its screenshots capped; the browser's server and Stagehand
// (`npm ci` from the lockfile, no install scripts) whenever the lockfile
// changed; then goose's extensions for them: Cua Driver's MCP server on the
// pet's display and session bus, no telemetry, no update checks, and the
// browser's, with Node as a job finds it (goose's config, which the hands
// read for each new session, and the task client brings a loaded one's
// extensions to; JSON is YAML). Their environments are in their commands
// (`env VAR=… cmd`): goose moves no extension's `envs` into a session.
const INSTALL = `set -eu
export CUA_DRIVER_RS_TELEMETRY_ENABLED=false
if [ ! -d "$HOME/${CUA_DIR}" ]; then
  mkdir -p "$HOME/.local/share" && t=$(mktemp -d "$HOME/.local/share/.get-XXXXXX")
  curl -fsSL -o "$t/a.tar.gz" '${CUA_URL}'
  echo "${CUA_SHA256}  $t/a.tar.gz" | sha256sum -c - > /dev/null
  mkdir "$t/x" && tar -xzf "$t/a.tar.gz" -C "$t/x" && mv "$t/x" "$HOME/${CUA_DIR}" && rm -rf "$t"
fi
"$HOME/${CUA_DIR}/cua-driver" --version
mkdir -p "$HOME/.cua-driver" && echo '{"max_image_dimension": ${SHOT_MAX}}' > "$HOME/.cua-driver/config.json"
b="$HOME/${BROWSER_DIR}"
if ! cmp -s computer/browser/package-lock.json "$b/package-lock.json"; then
  t=$(mktemp -d "$HOME/.local/share/.get-XXXXXX")
  cp computer/browser/package.json computer/browser/package-lock.json "$t"
  (cd "$t" && npm ci --ignore-scripts --no-audit --no-fund --loglevel=error)
  rm -rf "$b" && mv "$t" "$b"
fi
cp computer/browser/browser-mcp.mjs computer/browser/answer.mjs "$b"
mkdir -p "$HOME/.config/goose" && cat > "$HOME/.config/goose/config.yaml" << EOF
{"extensions": {"cua": {"enabled": true, "type": "stdio", "name": "cua", "description": "desktop apps on the pet's screen", "cmd": "/usr/bin/env", "args": ["DISPLAY=:99", "DBUS_SESSION_BUS_ADDRESS=unix:path=$HOME/.pet/bus", "CUA_DRIVER_RS_TELEMETRY_ENABLED=false", "CUA_DRIVER_RS_UPDATE_CHECK=false", "$HOME/${CUA_DIR}/cua-driver", "mcp"], "timeout": 120, "available_tools": ${JSON.stringify(TOOLS)}},
"browser": {"enabled": true, "type": "stdio", "name": "browser", "description": "the web, in the Chrome on the pet's screen", "cmd": "$(command -v node)", "args": ["$b/browser-mcp.mjs"], "timeout": 120}}}
EOF`;

// the task, in the session of the chat that asked (the hands' task client);
// each step marks the agent as the pet's driver (~/.pet/agent, which
// computer/pet.mjs reads). It drives the pet's own display, which pet.mjs
// starts on its Sprite.
const DO = `if command -v sprite-env > /dev/null && [ ! -e /tmp/.X11-unix/X99 ]; then echo "The pet's screen is not up yet (its first start installs a browser): try again in a minute." >&2; exit 1; fi
mkdir -p "$HOME/.pet" && MARK="$HOME/.pet/agent" exec node "$HOME/.fragment/agent/task.mjs"`;

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
  // `task` on its screen, which everyone here watches and may click too, in
  // the session of the chat that asked (`chat`; none: this page's own). Each
  // step is durable: Cua Driver and the browser's Stagehand are installed
  // once (pinned, checked), then the hands do the task for at most 10
  // minutes, posting each step to the chat's `work` (this fragment's, for
  // its page). This `work` gets the run's start and end. Answers {message,
  // code}: goose's last words and the task's exit code.
  async do({ task, chat }, job) {
    const exec = (command, opts) => job.computer.exec(command, { timeout: STEP_MS, ...opts });
    await job.publish("work", { run: job.run, kind: "start", task, asker: job.principal });
    try {
      ok(await exec(INSTALL), "installing Cua Driver and the browser's Stagehand");
      const env = { PROMPT: `${SCREEN}\n\n${task}`, CHAT: chat ?? "", RUN: String(job.run), ASKER: job.principal };
      const run = await exec(DO, { timeout: DO_MS, env });
      const message = run.stdout.trim().slice(-2000) || run.stderr.trim().slice(-2000);
      await job.publish("work", { run: job.run, kind: "end", code: run.code, message });
      return { message, code: run.code };
    } catch (e) {
      await job.publish("work", { run: job.run, kind: "end", error: String(e?.message ?? e).slice(0, 2000) });
      throw e;
    }
  }
}
