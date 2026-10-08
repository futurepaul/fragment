// Split: shared costs for a trip or a house. People signed in join it
// (`join`); an expense is paid by one of them and shared equally among
// some (`add`), and a payment between two is `settle`. `ledger`, the
// page's live query, works out in SQL what each has paid and owes, and
// suggests the fewest payments that settle it. `scan` reads a receipt
// photo with a model (a job's text step: the cheap tier's model reads
// images) and adds what it found as the snapper's expense; what it says
// back goes to them on `scans`.
import { DurableObject } from "cloudflare:workers";

const PEOPLE_MAX = 50;
// Expenses kept, and the newest `ledger` lists.
const EXPENSES_MAX = 5000;
const EXPENSES_SHOWN = 200;
const LINES_MAX = 60;
const CENTS_MAX = 100000000;
const READ = `You read receipts. Answer with JSON only: {"what": "Trattoria Roma", "total": 45.5, "lines": [{"name": "Pasta", "amount": 30}]}: the shop's name, the total paid, and each line with its amount, as numbers in the receipt's currency. A photo that is no receipt is {"what": null}.`;

const cents = (n) => (Number.isFinite(n) ? Math.min(CENTS_MAX, Math.max(0, Math.round(n * 100))) : null);

// What a model's answer says of a receipt, or null when it is not the JSON asked for.
function receipt(text) {
  try {
    const r = JSON.parse(text.slice(text.indexOf("{"), text.lastIndexOf("}") + 1));
    const total = cents(r.total);
    if (typeof r.what !== "string" || !r.what.trim() || !total) return null;
    const lines = (Array.isArray(r.lines) ? r.lines : [])
      .filter((l) => typeof l?.name === "string" && l.name.trim() && cents(l.amount) !== null)
      .slice(0, LINES_MAX)
      .map((l) => ({ name: l.name.trim().slice(0, 80), cents: cents(l.amount) }));
    return { what: r.what.trim().slice(0, 120), cents: total, lines };
  } catch {
    return null;
  }
}

export class App extends DurableObject {
  constructor(ctx, env) {
    super(ctx, env);
    const sql = ctx.storage.sql;
    sql.exec("CREATE TABLE IF NOT EXISTS about (one INTEGER PRIMARY KEY CHECK (one = 1), title TEXT NOT NULL, currency TEXT NOT NULL)");
    sql.exec("CREATE TABLE IF NOT EXISTS people (id TEXT PRIMARY KEY, name TEXT NOT NULL, at INTEGER NOT NULL)");
    sql.exec(`CREATE TABLE IF NOT EXISTS expenses (id INTEGER PRIMARY KEY AUTOINCREMENT, what TEXT NOT NULL, cents INTEGER NOT NULL,
      paid_by TEXT NOT NULL, kind TEXT NOT NULL, lines TEXT NOT NULL, by TEXT NOT NULL, at INTEGER NOT NULL)`);
    sql.exec("CREATE TABLE IF NOT EXISTS shares (expense INTEGER NOT NULL, person TEXT NOT NULL, cents INTEGER NOT NULL, PRIMARY KEY (expense, person))");
  }

  about({ title, currency }) {
    const code = currency.toUpperCase();
    if (!/^[A-Z]{3}$/.test(code)) throw new Error("a currency is its three-letter code, like EUR");
    this.ctx.storage.sql.exec("INSERT INTO about (one, title, currency) VALUES (1, ?, ?) ON CONFLICT (one) DO UPDATE SET title = excluded.title, currency = excluded.currency", title, code);
    return { title, currency: code };
  }

  // the caller, a person signed in, is in the split (again: by a new name)
  join({ name }, call) {
    if (!call.principal.startsWith("id:")) throw new Error("sign in to join the split");
    const sql = this.ctx.storage.sql;
    const known = sql.exec("SELECT 1 FROM people WHERE id = ?", call.principal).toArray().length > 0;
    if (!known && sql.exec("SELECT COUNT(*) AS n FROM people").one().n >= PEOPLE_MAX) throw new Error(`a split has at most ${PEOPLE_MAX} people`);
    sql.exec("INSERT INTO people (id, name, at) VALUES (?, ?, ?) ON CONFLICT (id) DO UPDATE SET name = excluded.name", call.principal, name.trim(), Date.now());
    return { id: call.principal, name: name.trim() };
  }

  // paid by `paidBy` (the caller unless named), shared equally among
  // `among` (everyone unless named); a cent left over goes to the first
  add({ what, cents, paidBy, among, lines = [] }, call) {
    const sql = this.ctx.storage.sql;
    const by = this.#in(call.principal);
    const everyone = sql.exec("SELECT id FROM people ORDER BY at").toArray().map((p) => p.id);
    const payer = this.#in(paidBy ?? by);
    const split = (among ?? everyone).map((id) => this.#in(id));
    if (sql.exec("SELECT COUNT(*) AS n FROM expenses").one().n >= EXPENSES_MAX) throw new Error(`a split keeps ${EXPENSES_MAX} expenses: start a new one`);
    const { id } = sql.exec("INSERT INTO expenses (what, cents, paid_by, kind, lines, by, at) VALUES (?, ?, ?, 'expense', ?, ?, ?) RETURNING id",
      what, cents, payer, JSON.stringify(lines), by, Date.now()).one();
    const each = Math.floor(cents / split.length);
    split.forEach((person, i) => sql.exec("INSERT INTO shares (expense, person, cents) VALUES (?, ?, ?)", id, person, each + (i < cents - each * split.length ? 1 : 0)));
    return { id };
  }

  // the caller paid `to` back
  settle({ to, cents }, call) {
    const sql = this.ctx.storage.sql;
    const from = this.#in(call.principal);
    if (this.#in(to) === from) throw new Error("a payment is to someone else");
    const { id } = sql.exec("INSERT INTO expenses (what, cents, paid_by, kind, lines, by, at) VALUES ('Payment', ?, ?, 'payment', '[]', ?, ?) RETURNING id", cents, from, from, Date.now()).one();
    sql.exec("INSERT INTO shares (expense, person, cents) VALUES (?, ?, ?)", id, to, cents);
    return { id };
  }

  // by who paid it or who added it, or an editor
  remove({ id }, call) {
    const sql = this.ctx.storage.sql;
    const e = sql.exec("SELECT paid_by, by FROM expenses WHERE id = ?", id).toArray()[0];
    if (!e) throw new Error(`no expense ${id}`);
    if (![e.paid_by, e.by].includes(call.principal) && !["editor", "owner"].includes(call.role)) throw new Error("only who paid it or added it removes it");
    sql.exec("DELETE FROM shares WHERE expense = ?", id);
    sql.exec("DELETE FROM expenses WHERE id = ?", id);
    return { id };
  }

  // who has paid and owes what, the newest expenses, and the payments that settle it all
  ledger(input, call) {
    const sql = this.ctx.storage.sql;
    const about = sql.exec("SELECT title, currency FROM about").toArray()[0] ?? { title: "", currency: "USD" };
    const people = sql.exec(`SELECT id, name,
        COALESCE((SELECT SUM(cents) FROM expenses WHERE paid_by = people.id), 0) AS paid,
        COALESCE((SELECT SUM(cents) FROM shares WHERE person = people.id), 0) AS owes
      FROM people ORDER BY at`).toArray().map((p) => ({ ...p, balance: p.paid - p.owes }));
    const expenses = sql.exec("SELECT id, what, cents, paid_by AS paidBy, kind, lines, by, at FROM expenses ORDER BY id DESC LIMIT ?", EXPENSES_SHOWN).toArray();
    const among = new Map(expenses.map((e) => [e.id, []]));
    if (expenses.length) {
      for (const s of sql.exec("SELECT expense, person FROM shares WHERE expense >= ?", expenses.at(-1).id).toArray()) among.get(s.expense)?.push(s.person);
    }
    const spent = sql.exec("SELECT COALESCE(SUM(cents), 0) AS n FROM expenses WHERE kind = 'expense'").one().n;
    return {
      ...about,
      you: call.principal,
      people,
      expenses: expenses.map((e) => ({ ...e, lines: JSON.parse(e.lines), among: among.get(e.id) })),
      settle: settle(people),
      spent,
    };
  }

  // a receipt photo (base64 of a JPEG) read by a model, and added as the caller's expense
  async scan({ image }, job) {
    const who = job.principal;
    const say = (text, expense = null) => job.publish("scans", { for: who, text, expense });
    const { people } = await job.call("ledger", {});
    if (!people.some((p) => p.id === who)) {
      await say("Join the split first.");
      return { added: null };
    }
    if (!image.startsWith("/9j/")) {
      await say("That photo is not a JPEG.");
      return { added: null };
    }
    const { text } = await job.ai.text({
      messages: [
        { role: "system", content: READ },
        { role: "user", content: [{ type: "text", text: "Read this receipt." }, { type: "image_url", image_url: { url: `data:image/jpeg;base64,${image}` } }] },
      ],
      max_tokens: 2000,
    });
    const read = receipt(text);
    if (!read) {
      await say("I couldn't read that receipt. Try a closer, flatter photo, or add it by hand.");
      return { added: null };
    }
    const { id } = await job.call("add", read);
    await say(`Added ${read.what} from your receipt.`, id);
    return { added: id };
  }

  #in(id) {
    if (!id.startsWith("id:")) throw new Error("sign in to change the split");
    if (!this.ctx.storage.sql.exec("SELECT 1 FROM people WHERE id = ?", id).toArray().length) throw new Error(`${id} is not in the split: join it first`);
    return id;
  }
}

// The fewest payments that settle the balances, near enough: the one who
// owes most pays the one owed most, as much as they can, until none is left.
function settle(people) {
  const owe = people.filter((p) => p.balance < 0).map((p) => ({ id: p.id, left: -p.balance })).sort((a, b) => b.left - a.left);
  const owed = people.filter((p) => p.balance > 0).map((p) => ({ id: p.id, left: p.balance })).sort((a, b) => b.left - a.left);
  const out = [];
  for (let i = 0, j = 0; i < owe.length && j < owed.length; ) {
    const cents = Math.min(owe[i].left, owed[j].left);
    out.push({ from: owe[i].id, to: owed[j].id, cents });
    owe[i].left -= cents;
    owed[j].left -= cents;
    if (owe[i].left === 0) i++;
    if (owed[j].left === 0) j++;
  }
  return out;
}
