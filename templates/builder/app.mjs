// Builder: a fragment that builds fragments. It declares a computer (its
// own Sprite, paired as its owner's, the `fragment` CLI signed in there as
// itself), and `build({task, chat?, fragments?})` is a job on it, each step
// a durable `job.computer.exec`: the computer's hands (goose, one session
// per chat: docs/agent-computer.md), with the `fragment` CLI in their
// shell, make and deploy a new fragment (its owner's), or change the owner's
// `fragments`, once the owner's agent made this computer an editor of each. goose's model calls go through
// `fragment model --serve`, signed as the computer and paid by the owner:
// no model key is on the computer. The run answers with what it built.
import { DurableObject } from "cloudflare:workers";

// the cap on a build's task (the exec ends it), and on the other steps
const BUILD_MS = 10 * 60 * 1000;
const STEP_MS = 3 * 60 * 1000;

const ON = "You are on a Linux computer with the `fragment` CLI installed and signed in; its manual is in your hints (`fragment guide` prints it).";

const prompt = (task) => `${ON} Build this as a new fragment and deploy it live:

${task}

Make a new fragment for it with \`fragment create <label>\` (a short label of your choosing), write its files in a folder here, deploy it with \`fragment deploy\`, and check its page answers. Change no other fragment. End with one short paragraph: what you built, and its URL.`;

const change = (task, fragments) => `${ON} Change your owner's ${fragments.length > 1 ? "fragments" : "fragment"} ${fragments.join(", ")} (you are an editor of each) and deploy the change live:

${task}

Get each one's files into a new folder with \`mkdir <folder> && fragment sync <name> --dir <folder>\`, change them there, put them live with \`fragment deploy <name> --dir <folder>\`, and check its page answers. Change no other fragment. End with one short paragraph: what you changed, and its URL.`;

// until the owner's agent made this computer an editor of each (it does,
// once the computer paired: agent/src/handoff.rs), at most about 2 minutes
const editor = (names) =>
  names.map((n) => `i=0; until fragment status ${n} --json 2>/dev/null | grep -Eq '"role": ?"(editor|owner)"'; do i=$((i+1)); [ $i -lt 40 ] || exit 1; sleep 3; done`).join("; ");

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
  // without building or recording twice.
  async build({ task, chat, fragments = [] }, job) {
    const exec = (command, opts) => job.computer.exec(command, { timeout: STEP_MS, ...opts });
    const record = (status, rest = {}) => job.call("record", { run: job.run, task, status, ...rest });
    await record("building");
    try {
      const bad = fragments.find((n) => !FRAGMENT_NAME.test(n));
      if (bad) throw new Error(`${JSON.stringify(bad)} is not a fragment's name`);
      if (fragments.length) ok(await exec(editor(fragments)), `being made an editor of ${fragments.join(", ")}`);
      const before = data(await exec("fragment list --json"), "fragment list").fragments.map((f) => f.name);
      // the task, in the session of the chat that asked (the hands' task client)
      const env = { PROMPT: fragments.length ? change(task, fragments) : prompt(task), CHAT: chat ?? "", RUN: String(job.run), ASKER: job.principal };
      const run = await exec(`node "$HOME/.fragment/agent/task.mjs"`, { timeout: BUILD_MS, env });
      const fresh = data(await exec("fragment list --json"), "fragment list").fragments.map((f) => f.name).filter((n) => !before.includes(n) && FRAGMENT_NAME.test(n));
      const built = [];
      for (const name of fresh.slice(0, 20 - fragments.length)) {
        const s = data(await exec(`fragment status ${name} --json`), `fragment status ${name}`);
        built.push({ name, url: s.urls.canonical, live: Boolean(s.pins.live) });
      }
      for (const name of fragments) {
        const s = data(await exec(`fragment status ${name} --json`), `fragment status ${name}`);
        built.push({ name, url: s.urls.canonical, live: Boolean(s.pins.live), changed: true });
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
