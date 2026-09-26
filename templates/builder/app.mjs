// Builder: a fragment that builds fragments. It declares a computer (its
// own Sprite, paired as its owner's, the `fragment` CLI signed in there as
// itself), and `build({task})` is a job on it, each step a durable
// `job.computer.exec`: goose (pinned, checked) is installed, then run
// headless with the task and the `fragment` CLI in its shell, and makes and
// deploys a new fragment (its owner's). goose's model calls go through
// `fragment model --serve`, signed as the computer and paid by the owner:
// no model key is on the computer. The run answers with what it built.
import { DurableObject } from "cloudflare:workers";

// goose's CLI release, at least two weeks old (the dependency cooldown),
// checked against the SHA-256 GitHub lists for the asset
const GOOSE_VERSION = "1.50.0";
const GOOSE_URL = `https://github.com/aaif-goose/goose/releases/download/v${GOOSE_VERSION}/goose-x86_64-unknown-linux-musl.tar.gz`;
const GOOSE_SHA256 = "ff8c51428180142e5c92e2a0b67b3d1762698499d028f6f5fe370fdbec8c84af";
const GOOSE = `"$HOME/.local/bin/goose-${GOOSE_VERSION}"`;
// the cap on a build's goose run (the exec ends it), and on the other steps
const BUILD_MS = 10 * 60 * 1000;
const STEP_MS = 3 * 60 * 1000;

// goose once, then the CLI's guide as goose's hints (current with the CLI)
const INSTALL = `set -eu
if [ ! -x ${GOOSE} ]; then
  t=$(mktemp -d)
  curl -fsSL -o "$t/goose.tar.gz" '${GOOSE_URL}'
  echo "${GOOSE_SHA256}  $t/goose.tar.gz" | sha256sum -c - >/dev/null
  tar -xzf "$t/goose.tar.gz" -C "$t"
  mkdir -p "$HOME/.local/bin" && mv "$t/goose" ${GOOSE} && rm -rf "$t"
fi
fragment model --help | grep -q -- --serve || { echo "this computer's fragment CLI predates 'model --serve': install its latest release" >&2; exit 1; }
mkdir -p "$HOME/.config/goose" && fragment guide > "$HOME/.config/goose/.goosehints"
${GOOSE} --version`;

// goose headless in the run's own folder, its model the platform's through
// a model endpoint of the run's own (a free port), stopped with it; the
// answer is the end of what goose said
const RUN = `set -u
cd && mkdir -p "$WORK" && cd "$WORK"
fragment model --serve --port 0 > serve.log 2>&1 &
serve=$!
trap 'kill $serve 2>/dev/null' EXIT
port=
for i in $(seq 100); do port=$(sed -n 's#.*http://127\\.0\\.0\\.1:\\([0-9]*\\)/v1.*#\\1#p' serve.log); [ -n "$port" ] && break; sleep 0.1; done
[ -n "$port" ] || { cat serve.log >&2; exit 1; }
OPENAI_BASE_URL="http://127.0.0.1:$port/v1" ${GOOSE} run --quiet --no-session --with-builtin developer --max-turns 60 --text "$PROMPT" > goose.log 2>&1
code=$?
tail -c 3000 goose.log
exit $code`;

const GOOSE_ENV = { GOOSE_PROVIDER: "openai", GOOSE_MODEL: "fragment", GOOSE_MODE: "auto", GOOSE_MAX_TOKENS: "4096", GOOSE_DISABLE_KEYRING: "1" };

const prompt = (task) => `You are on a Linux computer with the \`fragment\` CLI installed and signed in; its manual is in your hints (\`fragment guide\` prints it). Build this as a new fragment and deploy it live:

${task}

Make a new fragment for it with \`fragment create <label>\` (a short label of your choosing), write its files in a folder here, deploy it with \`fragment deploy\`, and check its page answers. Change no other fragment. End with one short paragraph: what you built, and its URL.`;

const FRAGMENT_NAME = /^[a-z0-9-]{1,63}\.[a-z0-9-]{1,63}$/;

// a command's answer, or why it failed; `data`: a CLI answer's (`--json`)
function ok(out, what) {
  if (out.code !== 0) throw new Error(`${what} failed (${out.code}): ${(out.stderr || out.stdout).slice(-500)}`);
  return out;
}
const data = (out, what) => JSON.parse(ok(out, what).stdout).data;

export class App extends DurableObject {
  constructor(ctx, env) {
    super(ctx, env);
    ctx.storage.sql.exec(`CREATE TABLE IF NOT EXISTS builds (
      run INTEGER PRIMARY KEY, task TEXT NOT NULL, status TEXT NOT NULL, built TEXT NOT NULL, message TEXT NOT NULL, at INTEGER NOT NULL)`);
  }

  // A job: each `await job.…` is a durable step, so a crash resumes here
  // without installing, building, or recording twice.
  async build({ task }, job) {
    const exec = (command, opts) => job.computer.exec(command, { timeout: STEP_MS, ...opts });
    const record = (status, rest = {}) => job.call("record", { run: job.run, task, status, ...rest });
    await record("installing");
    try {
      ok(await exec(INSTALL), "installing goose");
      const before = data(await exec("fragment list --json"), "fragment list").fragments.map((f) => f.name);
      await record("building");
      const env = { ...GOOSE_ENV, WORK: `builds/run-${job.run}`, PROMPT: prompt(task) };
      const run = await exec(RUN, { timeout: BUILD_MS, env });
      const fresh = data(await exec("fragment list --json"), "fragment list").fragments.map((f) => f.name).filter((n) => !before.includes(n) && FRAGMENT_NAME.test(n));
      const built = [];
      for (const name of fresh.slice(0, 20)) {
        const s = data(await exec(`fragment status ${name} --json`), `fragment status ${name}`);
        built.push({ name, url: s.urls.canonical, live: Boolean(s.pins.live) });
      }
      const message = run.stdout.trim().slice(-2000) || run.stderr.trim().slice(-2000);
      const url = built.find((b) => b.live)?.url ?? null;
      await record(url && run.code === 0 ? "built" : "failed", { built, message });
      return { url, built, message, code: run.code };
    } catch (e) {
      await record("failed", { message: String(e?.message ?? e).slice(0, 2000) });
      throw e;
    }
  }

  record({ run, task, status, built = [], message = "" }) {
    this.ctx.storage.sql.exec(
      `INSERT INTO builds (run, task, status, built, message, at) VALUES (?, ?, ?, ?, ?, ?)
       ON CONFLICT (run) DO UPDATE SET status = excluded.status, built = excluded.built, message = excluded.message, at = excluded.at`,
      run, task, status, JSON.stringify(built), message, Date.now());
    return { run, status };
  }

  builds() {
    const rows = this.ctx.storage.sql.exec("SELECT run, task, status, built, message, at FROM builds ORDER BY run DESC LIMIT 50").toArray();
    return { builds: rows.map((b) => ({ ...b, built: JSON.parse(b.built) })) };
  }
}
