// The shell's Billing (docs/billing.md, "The shell"; docs/api.md, Seats
// and orgs, Billing): a person's seat, their credit, and their org's.
// A guest buys a seat, or starts a trial with a code; a seat's holder sees
// it, lets a $200 seat's computer sleep, and buys credit; an org's admin
// adds, changes and removes seats and admins, and opens Stripe's portal for
// card and invoices. Stripe's Checkout and portal are Stripe's pages: the
// shell goes there, and Checkout's return (`/settings?checkout=…`) comes
// back here (`returning`). Every value is text, never markup.

const KIND = { seat: "$100 seat", seat_always_on: "$200 always-on seat" };

/**
 * The shell's helpers this module uses: `{api, el, section, line, usd,
 * reopen, firstAgent}` (`firstAgent`: home's first run, for a person with
 * no chats yet; otherwise null).
 */
let h = null;

function button(text, onclick) {
  const b = h.el("button", "quiet", text);
  b.type = "button";
  b.onclick = async () => {
    b.disabled = true;
    try {
      await onclick();
    } catch (e) {
      note(b.closest("section"), e.message);
    } finally {
      b.disabled = false;
    }
  };
  return b;
}

/** A line of warning at the end of `box`, replacing the last one. */
function note(box, text) {
  if (!box) return;
  box.querySelector(":scope > .billing-note")?.remove();
  box.append(h.el("p", "settings-warning billing-note", text));
}

function kindSelect(value = "seat") {
  const s = h.el("select");
  for (const [k, label] of Object.entries(KIND)) {
    const o = h.el("option", null, label);
    o.value = k;
    s.append(o);
  }
  s.value = value;
  return s;
}

async function goCheckout(body) {
  const v = await h.api("POST", "/api/billing/checkout", body);
  location.assign(v.url);
}

/**
 * Checkout's return, or a trial mailed (`/settings?checkout=`, `?pack=`,
 * `?trial=`): the purchase applied at once, whatever Stripe's webhook's
 * timing. Answers what to say, and a trial code to offer, if any.
 */
export async function returning(helpers) {
  h = helpers;
  const q = new URLSearchParams(location.search);
  const session = q.get("checkout") || q.get("pack");
  const trial = q.get("trial");
  if (!session && !trial) return { said: null, trial: null };
  history.replaceState(null, "", location.pathname);
  if (session === "canceled") return { said: "Checkout was canceled: nothing was bought.", trial: null };
  if (!session) return { said: null, trial };
  try {
    await h.api("POST", `/api/billing/sessions/${encodeURIComponent(session)}`, {});
    return { said: q.get("pack") ? "Paid: the credit is yours." : "Paid: your seat is ready.", trial: null };
  } catch (e) {
    return { said: `Your purchase is not here yet: ${e.message}`, trial: null };
  }
}

/** The Billing section, and an admin's Org section. */
export async function billingSections(helpers, ledger, back) {
  h = helpers;
  const { el, section, line, usd } = h;
  const mine = await h.api("GET", "/api/seat").catch(() => null);
  if (!mine) return [section("Billing", el("p", "muted", "Your seat could not be read."))];
  const billing = section("Billing");
  if (back?.said) billing.append(el("p", "settings-line", back.said));
  const seat = mine.seat;
  if (seat) {
    const kind = KIND[seat.kind] + (seat.comped ? " (given)" : "");
    billing.append(line("Seat", kind), line("Org", seat.org.name + (seat.admin ? " (you admin it)" : "")));
    if (seat.trialEnds) billing.append(line("Trial ends", new Date(seat.trialEnds * 1000).toLocaleDateString()));
    if (seat.good && h.firstAgent) {
      const first = el("button", "primary", "Make your first agent");
      first.type = "button";
      first.onclick = () => {
        first.disabled = true;
        h.firstAgent();
      };
      const go = el("div", "settings-actions");
      go.append(first);
      billing.append(go);
    }
    if (!seat.good) {
      billing.append(el("p", "settings-warning", "Your seat lapsed: your agents are stopped. Nothing of yours is gone."));
      if (seat.admin) billing.append(button("Pay again", () => goCheckout({ kind: seat.kind })));
      else billing.append(el("p", "muted", "Ask your org's admin to pay for it again."));
    }
    if (seat.good && seat.kind === "seat_always_on") {
      const sleeps = el("input");
      sleeps.type = "checkbox";
      sleeps.checked = seat.sleeps;
      sleeps.onchange = async () => {
        try {
          await h.api("PUT", "/api/seat", { sleeps: sleeps.checked });
        } catch (e) {
          sleeps.checked = !sleeps.checked;
          note(billing, e.message);
        }
      };
      const label = el("label", "settings-line");
      label.append(sleeps, " Let my computer sleep when nothing is open");
      billing.append(label);
    }
  } else if (!mine.org || mine.admin) {
    billing.append(el("p", "muted", "You are a guest: you see and edit what is shared with you. A seat lets you make agents and apps."));
    const buy = el("div", "settings-actions");
    buy.append(button("Get a $100 seat", () => goCheckout({ kind: "seat" })), button("Get a $200 always-on seat", () => goCheckout({ kind: "seat_always_on" })));
    billing.append(buy);
    const trial = el("form", "settings-actions");
    const code = el("input");
    code.placeholder = "Trial code";
    code.value = back?.trial || "";
    code.autocomplete = "off";
    const start = el("button", "quiet", "Start a trial");
    start.type = "submit";
    trial.append(code, start);
    trial.onsubmit = async (e) => {
      e.preventDefault();
      try {
        await goCheckout({ trialCode: code.value });
      } catch (err) {
        note(billing, err.message);
      }
    };
    billing.append(trial, el("p", "muted", "Prices are monthly, before tax. A trial takes a card first, and charges it when the trial ends unless you cancel."));
  } else {
    billing.append(el("p", "muted", `You are in ${mine.org.name}: its admins give you a seat.`));
  }
  for (const o of mine.offered) billing.append(el("p", "muted", `${o.org.name} offered you a ${KIND[o.kind]}; you are in another org.`));
  if (ledger) {
    billing.append(line("Credit this month", `${usd(ledger.availableMicros)} left`));
    if (seat?.good) billing.append(button("Buy $25 of credit", async () => location.assign((await h.api("POST", "/api/billing/packs", {})).url)));
  }
  const out = [billing];
  if (mine.admin) out.push(await orgSection());
  return out;
}

/** An admin's org: its seats and admins, and Stripe's portal. */
async function orgSection() {
  const { el, section } = h;
  const org = await h.api("GET", "/api/org").catch((e) => ({ error: e.message }));
  const box = section(org.name ? `Org: ${org.name}` : "Org");
  if (org.error) {
    box.append(el("p", "muted", org.error));
    return box;
  }
  const table = el("table", "billing-seats");
  for (const m of org.members) {
    const tr = el("tr");
    const who = el("td", null, m.email + (m.person ? "" : " (invited)"));
    const role = el("td", null, [m.admin ? "admin" : "", m.seat ? KIND[m.seat] + (m.comped ? " (given)" : "") : ""].filter(Boolean).join(", "));
    const actions = el("td");
    if (m.seat && !m.comped) {
      const kind = kindSelect(m.seat);
      kind.onchange = async () => {
        try {
          await h.api("PATCH", `/api/org/seats/${encodeURIComponent(m.id)}`, { kind: kind.value });
          await h.reopen();
        } catch (e) {
          kind.value = m.seat;
          note(box, e.message);
        }
      };
      actions.append(kind, button("Remove seat", async () => {
        if (!confirm(`Remove ${m.email}'s seat? Their agents stop.`)) return;
        await h.api("DELETE", `/api/org/seats/${encodeURIComponent(m.id)}`);
        await h.reopen();
      }));
    }
    if (m.admin && org.members.filter((x) => x.admin).length > 1) {
      actions.append(button("Not admin", async () => {
        await h.api("DELETE", `/api/org/admins/${encodeURIComponent(m.id)}`);
        await h.reopen();
      }));
    }
    tr.append(who, role, actions);
    table.append(tr);
  }
  box.append(table);
  const add = el("form", "settings-actions");
  const email = el("input");
  email.type = "email";
  email.placeholder = "email";
  email.required = true;
  const kind = kindSelect();
  const go = el("button", "quiet", "Add a seat");
  go.type = "submit";
  add.append(email, kind, go);
  add.onsubmit = async (e) => {
    e.preventDefault();
    try {
      await h.api("POST", "/api/org/seats", { email: email.value.trim(), kind: kind.value });
      await h.reopen();
    } catch (err) {
      note(box, err.message);
    }
  };
  const admin = el("form", "settings-actions");
  const adminEmail = el("input");
  adminEmail.type = "email";
  adminEmail.placeholder = "email";
  adminEmail.required = true;
  const makeAdmin = el("button", "quiet", "Add an admin");
  makeAdmin.type = "submit";
  admin.append(adminEmail, makeAdmin);
  admin.onsubmit = async (e) => {
    e.preventDefault();
    try {
      await h.api("POST", "/api/org/admins", { email: adminEmail.value.trim() });
      await h.reopen();
    } catch (err) {
      note(box, err.message);
    }
  };
  const portal = el("div", "settings-actions");
  portal.append(button("Payment and invoices", async () => location.assign((await h.api("POST", "/api/billing/portal", {})).url)));
  box.append(add, admin, el("p", "muted", "A seat added bills from now, prorated; one removed is credited on the next invoice. Cancel in Payment and invoices."), portal);
  return box;
}
