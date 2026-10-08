// Board: a family's chores or a small team's tasks, in three columns.
// Anyone who can open it reads it; people signed in change it, each card
// naming who made it and who it is for. Giving someone a card pushes it
// to them (`call.push` to their identity, which only their own browsers
// subscribe to), once the mutation commits. Every page follows `board`
// live and shows who is here.
import { DurableObject } from "cloudflare:workers";

// Cards to do or doing; past it a new one is refused, so `board` stays bounded.
const OPEN_MAX = 500;
// Done cards `board` lists, the newest first.
const DONE_SHOWN = 50;
const IDENTITY = /^id:[0-9a-f]{32}$/;

export class App extends DurableObject {
  constructor(ctx, env) {
    super(ctx, env);
    ctx.storage.sql.exec(`CREATE TABLE IF NOT EXISTS cards (
      id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL, col TEXT NOT NULL, pos REAL NOT NULL,
      assignee TEXT, by TEXT NOT NULL, at INTEGER NOT NULL)`);
  }

  add({ title }, call) {
    const by = person(call);
    const sql = this.ctx.storage.sql;
    if (sql.exec("SELECT COUNT(*) AS n FROM cards WHERE col != 'done'").one().n >= OPEN_MAX) throw new Error(`the board holds ${OPEN_MAX} open cards: finish some first`);
    const { id } = sql.exec("INSERT INTO cards (title, col, pos, by, at) VALUES (?, 'todo', ?, ?, ?) RETURNING id", title, this.#end("todo"), by, Date.now()).one();
    return { id };
  }

  // to the end of `column`
  move({ id, column }, call) {
    person(call);
    const card = this.#card(id);
    if (card.col !== column) this.ctx.storage.sql.exec("UPDATE cards SET col = ?, pos = ?, at = ? WHERE id = ?", column, this.#end(column), Date.now(), id);
    return { id, column };
  }

  // a card is someone's (an identity) or no one's (null); its new holder
  // hears it on their own browsers, unless they gave it to themselves
  assign({ id, to }, call) {
    const by = person(call);
    if (to !== null && !IDENTITY.test(to)) throw new Error("a card is given to a person (id:…)");
    const card = this.#card(id);
    this.ctx.storage.sql.exec("UPDATE cards SET assignee = ? WHERE id = ?", to, id);
    if (to !== null && to !== by && to !== card.assignee) call.push(to, { title: card.title, body: "This one is yours now.", tag: `card-${id}`, url: "./" });
    return { id, to };
  }

  remove({ id }, call) {
    person(call);
    this.#card(id);
    this.ctx.storage.sql.exec("DELETE FROM cards WHERE id = ?", id);
    return { id };
  }

  // the open cards in their order, then the newest done
  board() {
    const sql = this.ctx.storage.sql;
    const open = sql.exec("SELECT id, title, col, assignee, by FROM cards WHERE col != 'done' ORDER BY pos").toArray();
    const done = sql.exec("SELECT id, title, col, assignee, by FROM cards WHERE col = 'done' ORDER BY at DESC LIMIT ?", DONE_SHOWN).toArray();
    return { cards: [...open, ...done], done: sql.exec("SELECT COUNT(*) AS n FROM cards WHERE col = 'done'").one().n };
  }

  #card(id) {
    const card = this.ctx.storage.sql.exec("SELECT id, title, col, assignee FROM cards WHERE id = ?", id).toArray()[0];
    if (!card) throw new Error(`no card ${id}`);
    return card;
  }

  #end(column) {
    return this.ctx.storage.sql.exec("SELECT COALESCE(MAX(pos), 0) + 1 AS pos FROM cards WHERE col = ?", column).one().pos;
  }
}

// The caller, who must be a person signed in (or an agent acting for one):
// a card names who made it, and a visitor with the link is no one.
function person(call) {
  if (!call.principal.startsWith("id:")) throw new Error("sign in to change the board");
  return call.principal;
}
