// The operators' admin page (docs/billing.md, "The operator's admin";
// decision 59): people, orgs, trial codes, billing's health and the log
// of what operators did, through `/api/admin/*` with the operator's own
// platform session (`x-fragment-shell`, same-origin only). The API
// decides who may: anyone else is answered 403 and sees only that. Every
// value reaches the page as text, never as markup.

const $ = (id) => document.getElementById(id);
const status = $("status");

function say(text, bad = false) {
  status.textContent = text;
  status.className = bad ? "bad" : "";
}

async function api(method, path, body) {
  const headers = { "x-fragment-shell": "1" };
  if (body !== undefined) headers["content-type"] = "application/json";
  const r = await fetch(path, { method, headers, body: body === undefined ? undefined : JSON.stringify(body), credentials: "same-origin" });
  const v = await r.json().catch(() => ({}));
  if (!r.ok) throw new Error(v.message || `${method} ${path}: ${r.status}`);
  return v;
}

/** A table row of cells, each text; `open` runs when it is clicked. */
function row(cells, open) {
  const tr = document.createElement("tr");
  for (const c of cells) {
    const td = document.createElement("td");
    if (c instanceof Node) td.append(c);
    else td.textContent = c ?? "";
    tr.append(td);
  }
  if (open) tr.addEventListener("click", open);
  return tr;
}

function el(tag, text, cls) {
  const e = document.createElement(tag);
  if (text !== undefined) e.textContent = text;
  if (cls) e.className = cls;
  return e;
}

function when(ms) {
  return ms ? new Date(ms).toISOString().slice(0, 16).replace("T", " ") : "";
}

function seatText(s) {
  if (!s) return "";
  const kind = s.kind === "seat_always_on" ? "$200" : "$100";
  return `${kind} ${s.comped ? "comped" : "paid"}${s.good ? "" : " (lapsed)"}`;
}

function guard(fn) {
  return async (...args) => {
    try {
      await fn(...args);
    } catch (e) {
      say(e.message, true);
    }
  };
}

let me = null;

// ---- people
let peopleNext = null;
let peopleQ = "";

async function people(more = false) {
  const p = new URLSearchParams();
  if (peopleQ) p.set("q", peopleQ);
  if (more && peopleNext) p.set("after", peopleNext);
  const v = await api("GET", `/api/admin/people?${p}`);
  const rows = $("people-rows");
  if (!more) rows.replaceChildren();
  for (const person of v.people) {
    rows.append(row([person.email || person.npub, seatText(person.seat), person.org?.name, when(person.joinedAt), when(person.lastSignInAt)], guard(() => openPerson(person.npub))));
  }
  peopleNext = v.next;
  $("people-more").hidden = !v.next;
  say(`${rows.children.length} people`);
}

async function openPerson(npub) {
  const v = await api("GET", `/api/admin/people/${encodeURIComponent(npub)}`);
  const box = $("person");
  box.replaceChildren(el("h2", v.person.email || npub));
  const facts = el("dl");
  const fact = (k, val) => facts.append(el("dt", k), el("dd", val ?? ""));
  fact("npub", npub);
  fact("org", v.person.org ? `${v.person.org.name}${v.person.admin ? " (admin)" : ""}` : "none");
  fact("seat", seatText(v.person.seat) || "none");
  fact("plan", v.ledger.plan);
  fact("standing", v.ledger.standing?.why ? `${v.ledger.standing.standing}: ${v.ledger.standing.why}` : v.ledger.standing?.standing);
  fact("balance", `$${(v.ledger.balanceMicros / 1e6).toFixed(2)}`);
  fact("computer", v.computer ? `${v.computer.phase}${v.computer.alwaysOn ? ", always on" : ""}` : "none");
  box.append(facts);
  if (v.person.seat?.comped) {
    const end = el("button", "End this comp");
    end.addEventListener("click", guard(async () => {
      if (!confirm(`End the comped seat of ${v.person.email}? Their agents stop.`)) return;
      await api("DELETE", `/api/admin/seats/${encodeURIComponent(v.person.seat.id)}`);
      await openPerson(npub);
      await people();
      say("comp ended");
    }));
    box.append(end);
  }
  const grant = el("form");
  const usd = el("input");
  usd.type = "number";
  usd.min = "0.01";
  usd.step = "0.01";
  usd.placeholder = "dollars";
  usd.required = true;
  const why = el("input");
  why.placeholder = "why";
  why.required = true;
  grant.append(usd, why, el("button", "Grant credit"));
  grant.addEventListener("submit", guard(async (e) => {
    e.preventDefault();
    const id = `admin-${crypto.randomUUID()}`;
    await api("POST", `/api/ledger/${encodeURIComponent(npub)}/grant`, { id, micros: Math.round(Number(usd.value) * 1e6), by: me.id, why: why.value });
    say(`granted $${usd.value}`);
    await openPerson(npub);
  }));
  box.append(grant);
  box.hidden = false;
}

// ---- orgs
let orgsNext = null;

async function orgs(more = false) {
  const p = new URLSearchParams();
  if (more && orgsNext) p.set("after", orgsNext);
  const v = await api("GET", `/api/admin/orgs?${p}`);
  const rows = $("org-rows");
  if (!more) rows.replaceChildren();
  for (const o of v.orgs) {
    const st = el("span", o.status || "never paid", o.status && !["trialing", "active", "past_due"].includes(o.status) ? "lapsed" : "");
    rows.append(row([o.name, st, String(o.paidSeats), String(o.compedSeats), String(o.pending), String(o.admins), o.customer || ""], guard(() => openOrg(o.id))));
  }
  orgsNext = v.next;
  $("orgs-more").hidden = !v.next;
  say(`${rows.children.length} orgs`);
}

async function openOrg(id) {
  const v = await api("GET", `/api/admin/orgs/${encodeURIComponent(id)}`);
  const box = $("org");
  const table = el("table");
  const body = el("tbody");
  for (const m of v.members) {
    body.append(row([m.email, m.person ? "" : "waiting", m.admin ? "admin" : "", m.seat ? `${m.seat === "seat_always_on" ? "$200" : "$100"} ${m.comped ? "comped" : "paid"}` : "", m.id]));
  }
  table.append(body);
  box.replaceChildren(el("h2", v.name), table);
  box.hidden = false;
}

// ---- trials
async function trials() {
  const v = await api("GET", "/api/admin/trials");
  const rows = $("trial-rows");
  rows.replaceChildren();
  for (const t of v.codes) {
    const actions = el("span");
    const raise = el("button", "Places…");
    raise.addEventListener("click", guard(async (e) => {
      e.stopPropagation();
      const n = Number(prompt(`Places for ${t.code} (now ${t.capacity}; only more)`, String(t.capacity + 10)));
      if (!n) return;
      await api("PATCH", `/api/admin/trials/${t.id}`, { revision: t.revision, capacity: n });
      await trials();
    }));
    const toggle = el("button", t.active ? "Turn off" : "Turn on");
    toggle.addEventListener("click", guard(async (e) => {
      e.stopPropagation();
      await api("PATCH", `/api/admin/trials/${t.id}`, { revision: t.revision, active: !t.active });
      await trials();
    }));
    const send = el("button", "Mail…");
    send.addEventListener("click", guard(async (e) => {
      e.stopPropagation();
      const to = prompt(`Mail ${t.code} to`);
      if (!to) return;
      await api("POST", `/api/admin/trials/${t.id}/send`, { email: to });
      say(`mailed ${to}`);
    }));
    actions.append(raise, toggle, send);
    rows.append(row([el("code", t.code), t.name, `${t.days} days, ${t.kind === "seat_always_on" ? "$200" : "$100"}`, `${t.subscribed}/${t.capacity}`, String(t.open), t.active ? "on" : "off", actions]));
  }
  say(`${v.codes.length} codes`);
}

// ---- health and the log
async function health() {
  const v = await api("GET", "/api/admin/health");
  const facts = $("health-facts");
  facts.replaceChildren();
  const fact = (k, val) => facts.append(el("dt", k), el("dd", String(val ?? "")));
  fact("orgs paying", v.orgsPaying);
  fact("orgs lapsed", v.orgsLapsed);
  fact("seats paid", v.seatsPaid);
  fact("seats comped", v.seatsComped);
  fact("plan pushes queued", v.planPushesQueued);
  fact("quantity pushes queued", v.quantityPushesQueued);
  fact("last Stripe event", v.lastEventAt ? when(v.lastEventAt * 1000) : "none");
  fact("oldest subscription copy", v.oldestCopyAt ? when(v.oldestCopyAt) : "none");
  const rows = $("failing-rows");
  rows.replaceChildren(...v.failing.map((f) => row([f.queue, f.target, String(f.tries), when(f.due)])));
  say(v.failing.length ? `${v.failing.length} pushes failing` : "nothing failing");
}

let logNext = null;

async function log(more = false) {
  const p = new URLSearchParams();
  if (more && logNext) p.set("before", String(logNext));
  const v = await api("GET", `/api/admin/log?${p}`);
  const rows = $("log-rows");
  if (!more) rows.replaceChildren();
  for (const e of v.entries) rows.append(row([String(e.n), when(e.at), e.operator, e.action, e.target, e.detail]));
  logNext = v.next;
  $("log-more").hidden = !v.next;
}

// ---- tabs
const load = { people: () => people(), orgs: () => orgs(), trials, health, log: () => log() };

function show(tab) {
  for (const b of document.querySelectorAll("#tabs button")) b.classList.toggle("on", b.dataset.tab === tab);
  for (const s of document.querySelectorAll("main > section")) s.hidden = s.id !== tab;
  history.replaceState(null, "", `/admin#${tab}`);
  return guard(load[tab])();
}

document.addEventListener("DOMContentLoaded", guard(async () => {
  me = await api("GET", "/api/identities/me");
  $("me").textContent = me.subjects?.[0]?.email || me.id;
  for (const b of document.querySelectorAll("#tabs button")) b.addEventListener("click", () => show(b.dataset.tab));
  $("search").addEventListener("submit", guard(async (e) => {
    e.preventDefault();
    peopleQ = e.target.q.value.trim();
    await people();
  }));
  $("comp").addEventListener("submit", guard(async (e) => {
    e.preventDefault();
    const v = await api("POST", "/api/admin/seats", { email: e.target.email.value.trim(), kind: e.target.kind.value });
    e.target.reset();
    await people();
    say(`${v.created ? "comped" : "already comped"}: ${v.seat.email} in ${v.org.name}${v.mailed ? ", mailed" : ""}`);
  }));
  $("trial-new").addEventListener("submit", guard(async (e) => {
    e.preventDefault();
    // a form's own `name` shadows its field of that name: read them by `elements`
    const f = e.target.elements;
    const body = { name: f.name.value.trim(), kind: f.kind.value, days: Number(f.days.value), capacity: Number(f.capacity.value) };
    if (f.code.value.trim()) body.code = f.code.value.trim();
    const t = await api("POST", "/api/admin/trials", body);
    e.target.reset();
    await trials();
    say(`made ${t.code}`);
  }));
  $("people-more").addEventListener("click", guard(() => people(true)));
  $("orgs-more").addEventListener("click", guard(() => orgs(true)));
  $("log-more").addEventListener("click", guard(() => log(true)));
  await show(location.hash.slice(1) in load ? location.hash.slice(1) : "people");
}));
