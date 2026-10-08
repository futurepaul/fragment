// When: find a time, or run a quick poll. An editor asks the question and
// lists the options (`setup`); anyone who can open the page votes
// (`vote`, role `public`): a person signed in as themselves, anyone else
// as the anonymous principal the platform gives their browser (a cookie on
// this origin). A ballot is its voter's whole answer, so a vote replaces
// the one before. Every page follows `poll` live, and the tally moves as
// people vote.
import { DurableObject } from "cloudflare:workers";

// Ballots a poll holds: past it a new voter is refused, and `poll`'s
// answer stays far under the 1 MiB a query may return.
const VOTERS_MAX = 500;

export class App extends DurableObject {
  constructor(ctx, env) {
    super(ctx, env);
    const sql = ctx.storage.sql;
    sql.exec("CREATE TABLE IF NOT EXISTS poll (one INTEGER PRIMARY KEY CHECK (one = 1), question TEXT NOT NULL, mode TEXT NOT NULL, closed INTEGER NOT NULL DEFAULT 0, chosen INTEGER)");
    sql.exec("CREATE TABLE IF NOT EXISTS options (id INTEGER PRIMARY KEY AUTOINCREMENT, label TEXT NOT NULL UNIQUE, pos INTEGER NOT NULL)");
    sql.exec("CREATE TABLE IF NOT EXISTS voters (principal TEXT PRIMARY KEY, name TEXT NOT NULL, at INTEGER NOT NULL)");
    sql.exec("CREATE TABLE IF NOT EXISTS picks (principal TEXT NOT NULL, option INTEGER NOT NULL, answer TEXT NOT NULL, PRIMARY KEY (principal, option))");
  }

  // The question and its options, in order. An option whose label stays
  // keeps its id and its votes; a removed one takes its votes with it.
  setup({ question, mode, options }) {
    const sql = this.ctx.storage.sql;
    sql.exec("INSERT INTO poll (one, question, mode) VALUES (1, ?, ?) ON CONFLICT (one) DO UPDATE SET question = excluded.question, mode = excluded.mode", question, mode);
    const kept = new Set(options);
    for (const { id, label } of sql.exec("SELECT id, label FROM options").toArray()) {
      if (!kept.has(label)) {
        sql.exec("DELETE FROM options WHERE id = ?", id);
        sql.exec("DELETE FROM picks WHERE option = ?", id);
      }
    }
    options.forEach((label, pos) => sql.exec("INSERT INTO options (label, pos) VALUES (?, ?) ON CONFLICT (label) DO UPDATE SET pos = excluded.pos", label, pos));
    // a quick poll takes one answer: ballots from a poll of times keep their first yes
    if (mode === "choice") {
      sql.exec("DELETE FROM picks WHERE answer = 'maybe'");
      sql.exec("DELETE FROM picks WHERE rowid NOT IN (SELECT MIN(rowid) FROM picks GROUP BY principal)");
    }
    const chosen = sql.exec("SELECT chosen FROM poll").one().chosen;
    if (chosen !== null && !sql.exec("SELECT 1 FROM options WHERE id = ?", chosen).toArray().length) sql.exec("UPDATE poll SET chosen = NULL");
    return { options: sql.exec("SELECT id, label FROM options ORDER BY pos").toArray() };
  }

  // The caller's ballot: each option they can make, as yes or maybe (a
  // quick poll takes one yes). An empty one keeps their name and no picks.
  vote({ name, picks }, call) {
    const sql = this.ctx.storage.sql;
    const poll = sql.exec("SELECT mode, closed FROM poll").toArray()[0];
    if (!poll) throw new Error("there is nothing to vote on yet");
    if (poll.closed) throw new Error("voting is closed");
    if (poll.mode === "choice" && (picks.length > 1 || picks.some((p) => p.answer !== "yes"))) throw new Error("a quick poll takes one answer");
    const ids = new Set(sql.exec("SELECT id FROM options").toArray().map((o) => o.id));
    for (const p of picks) if (!ids.has(p.option)) throw new Error(`no option ${p.option}`);
    if (new Set(picks.map((p) => p.option)).size !== picks.length) throw new Error("an option is answered once");
    const known = sql.exec("SELECT 1 FROM voters WHERE principal = ?", call.principal).toArray().length > 0;
    if (!known && sql.exec("SELECT COUNT(*) AS n FROM voters").one().n >= VOTERS_MAX) throw new Error(`this poll has its ${VOTERS_MAX} voters`);
    sql.exec("INSERT INTO voters (principal, name, at) VALUES (?, ?, ?) ON CONFLICT (principal) DO UPDATE SET name = excluded.name", call.principal, name.trim() || "someone", Date.now());
    sql.exec("DELETE FROM picks WHERE principal = ?", call.principal);
    for (const p of picks) sql.exec("INSERT INTO picks (principal, option, answer) VALUES (?, ?, ?)", call.principal, p.option, p.answer);
    return { picks: picks.length };
  }

  // Voting stops (or starts again), with the option it settled on, if any.
  close({ closed, chosen = null }) {
    const sql = this.ctx.storage.sql;
    if (!sql.exec("SELECT 1 FROM poll").toArray().length) throw new Error("there is no poll to close");
    if (chosen !== null && !sql.exec("SELECT 1 FROM options WHERE id = ?", chosen).toArray().length) throw new Error(`no option ${chosen}`);
    sql.exec("UPDATE poll SET closed = ?, chosen = ?", closed ? 1 : 0, closed ? chosen : null);
    return { closed, chosen: closed ? chosen : null };
  }

  // The poll as every page shows it: each ballot by its voter's name (the
  // caller's marked `you`), never by principal, oldest voter first.
  poll(input, call) {
    const sql = this.ctx.storage.sql;
    const poll = sql.exec("SELECT question, mode, closed, chosen FROM poll").toArray()[0] ?? { question: "", mode: "times", closed: 0, chosen: null };
    const options = sql.exec("SELECT id, label FROM options ORDER BY pos").toArray();
    const ballots = new Map();
    for (const v of sql.exec("SELECT principal, name FROM voters ORDER BY at, principal").toArray()) {
      ballots.set(v.principal, { name: v.name, you: v.principal === call.principal, picks: {} });
    }
    for (const p of sql.exec("SELECT principal, option, answer FROM picks").toArray()) ballots.get(p.principal).picks[p.option] = p.answer;
    return { question: poll.question, mode: poll.mode, closed: poll.closed === 1, chosen: poll.chosen, options, ballots: [...ballots.values()] };
  }
}
