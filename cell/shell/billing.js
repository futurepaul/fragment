// The shell's Billing (docs/billing.md, "The shell"; docs/api.md, Seats
// and orgs, Billing): a person's seat, their credit, and their org's,
// from `GET /api/seat` (`mine`: their seat, whether they are a guest, and
// what is sold, its prices core's). A guest's seat and trial code are the
// welcome's (`seatOffer`, for shell.js `welcome`), which Billing links to;
// a seat's holder sees it, lets an always-on seat's computer sleep, and
// buys credit; an org's admin adds and removes seats and admins, and opens
// Stripe's portal for card and invoices. One seat is sold, always on
// (Paul, 2026-10-15); a `seat` is an operator's comp. An org is shown once
// someone besides its admin is in it (a
// seat, held or invited, or another admin): its admin alone, who pays for
// themselves, sees only Payment and invoices, and asks for the org's seats
// to add one for someone else. Stripe's Checkout and portal are Stripe's
// pages: the shell goes there; a seat's Checkout returns to the welcome
// (`/welcome?checkout=…`), a credit pack's here (`/settings?pack=…`,
// `returning`). Every value is text, never markup.

const KIND = { seat: "Seat whose computer sleeps when idle", seat_always_on: "Always-on seat" };
// an offer's amount, in whole dollars where it is whole ($200, not $200.00)
const price = (micros) => (micros / 1_000_000).toLocaleString(undefined, { style: "currency", currency: "USD", minimumFractionDigits: 0 });

/** What a seat gives a guest, at its price (`offer`: `GET /api/seat`'s). */
export function seatGives(offer) {
  return `A seat, ${price(offer.seatMicros)} a month, gives you agents of your own, the always-on computer they run on, apps of your own, and ${price(offer.includedMicros)} of credit a month.`;
}

/**
 * The shell's helpers this module uses: `{api, el}` for the welcome, and
 * `{section, line, usd, reopen}` besides for settings.
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

async function goCheckout(body) {
  const v = await h.api("POST", "/api/billing/checkout", body);
  location.assign(v.url);
}

/**
 * A credit pack's Checkout returning (`/settings?pack=`): the credit
 * applied at once, whatever Stripe's webhook's timing. Answers what to say.
 */
export async function returning(helpers) {
  h = helpers;
  const pack = new URLSearchParams(location.search).get("pack");
  if (!pack) return { said: null };
  history.replaceState(null, "", location.pathname);
  if (pack === "canceled") return { said: "Checkout was canceled: nothing was bought." };
  try {
    await h.api("POST", `/api/billing/sessions/${encodeURIComponent(pack)}`, {});
    return { said: "Paid: the credit is yours." };
  } catch (e) {
    return { said: `Your purchase is not here yet: ${e.message}` };
  }
}

/**
 * The welcome's seat for a guest (`mine`: `GET /api/seat`'s), to Stripe's
 * Checkout, and a trial code (`trial`: one a mailed link named); what
 * Checkout or a code refuses is said under them, then what more credit
 * costs. Someone in an org whose admins give its seats is told so instead.
 */
export function seatOffer(helpers, mine, trial) {
  h = helpers;
  const { el } = h;
  if (!mine) return [el("p", "form-error", "The seat could not be read: reload to try again.")];
  if (mine.org && !mine.admin) return [el("p", "muted", `You are in ${mine.org.name}: its admins give you a seat.`)];
  const { offer } = mine;
  const said = el("p", "form-error");
  said.hidden = true;
  const plans = el("div", "plans");
  const form = el("form", "trial-code");
  const code = el("input");
  code.placeholder = "Trial code";
  code.setAttribute("aria-label", "Trial code");
  code.value = trial || "";
  code.autocomplete = "off";
  code.required = true;
  const begin = el("button", "quiet", "Start a trial");
  begin.type = "submit";
  form.append(code, begin);
  // one Checkout at a time; a refusal is said; every choice is open again
  // after, as Billing's buttons are (a page Back brings back is usable)
  const go = async (body) => {
    const choices = [...plans.children, begin];
    for (const b of choices) b.disabled = true;
    said.hidden = true;
    try {
      await goCheckout(body);
    } catch (e) {
      said.textContent = e.message;
      said.hidden = false;
    } finally {
      for (const b of choices) b.disabled = false;
    }
  };
  // the one seat sold (decision 25; docs/ledger.md, Plans), as a guest takes it
  const b = el("button", "plan");
  b.type = "button";
  b.dataset.plan = offer.kind;
  b.append(
    el("strong", null, `${price(offer.seatMicros)} a month, always on`),
    el("span", null, `Your agents, on a computer of their own that stays awake (its time is not metered), apps of your own, and ${price(offer.includedMicros)} of credit included each month.`),
  );
  b.onclick = () => go({ kind: offer.kind });
  plans.append(b);
  form.onsubmit = (e) => {
    e.preventDefault();
    go({ trialCode: code.value });
  };
  return [
    plans,
    form,
    said,
    el("p", "muted", `Monthly, before tax. A trial takes a card first, and charges it when the trial ends unless you cancel. With a seat, more credit is ${price(offer.packMicros)} a pack, kept until spent.`),
  ];
}

/** The Billing section, and an admin's Org section (`mine`: `GET /api/seat`'s). */
export async function billingSections(helpers, ledger, mine, back) {
  h = helpers;
  const { el, section, line, actionLine, usd } = h;
  if (!mine) return [section("Billing", el("p", "muted", "Your seat could not be read."))];
  const { offer } = mine;
  const billing = section("Billing");
  if (back?.said) billing.append(el("p", "settings-line", back.said));
  // an admin's org, and whether they are alone in it
  const org = mine.admin ? await h.api("GET", "/api/org").catch((e) => ({ error: e.message })) : null;
  const alone = org?.members?.length === 1;
  const seat = mine.seat;
  if (seat) {
    const kind = KIND[seat.kind] + (seat.comped ? " (given)" : seat.kind === offer.kind ? `, ${price(offer.seatMicros)} a month` : "");
    billing.append(line("Seat", kind));
    if (!alone) billing.append(line("Org", seat.org.name + (seat.admin ? " (you admin it)" : "")));
    if (seat.trialEnds) billing.append(line("Trial ends", new Date(seat.trialEnds * 1000).toLocaleDateString()));
    if (!seat.good) {
      billing.append(el("p", "settings-warning", "Your seat lapsed: your agents are stopped. Nothing of yours is gone."));
      if (seat.admin) billing.append(actionLine("Renewal", button("Pay again", () => goCheckout({}))));
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
  } else if (mine.guest) {
    billing.append(el("p", "muted", `You are a guest: you see and edit what is shared with you. ${seatGives(offer)}`));
    // the seats and a trial code are the welcome's, as a new person's are
    const get = el("a", "quiet", "Get a seat");
    get.href = "/welcome";
    billing.append(actionLine("Seat", get));
  } else {
    // no seat of theirs: the deployment's plan gives everyone one
    billing.append(line("Seat", `${KIND[ledger?.plan] ?? "A seat"}, given to everyone here`));
  }
  for (const o of mine.offered) billing.append(el("p", "muted", `${o.org.name} offered you a ${KIND[o.kind].toLowerCase()}; you are in another org.`));
  // credit is a seat's (or a plan the deployment gives): a guest pays for
  // nothing, so has none to show
  if (ledger && !mine.guest) {
    const topUp = seat?.good ? button(`Buy ${price(offer.packMicros)} of credit`, async () => location.assign((await h.api("POST", "/api/billing/packs", {})).url)) : null;
    billing.append(line("Credit this month", `${usd(ledger.availableMicros)} left`, topUp));
  }
  if (!alone) return org ? [billing, orgSection(org, offer)] : [billing];
  const more = el("div", "settings-actions");
  const others = el("button", "quiet", "Add seats for others");
  others.type = "button";
  others.onclick = () => {
    more.closest(".settings-action-line")?.remove();
    billing.after(orgSection(org, offer));
  };
  more.append(...(seat?.comped ? [] : [portal()]), others);
  billing.append(actionLine("Organization", more));
  return [billing];
}

/** Stripe's portal: the org's card, invoices, and cancelling. */
function portal() {
  return button("Payment and invoices", async () => location.assign((await h.api("POST", "/api/billing/portal", {})).url));
}

/** An admin's org (`GET /api/org`'s): its seats and admins, and Stripe's portal; `offer`, what a seat added costs. */
function orgSection(org, offer) {
  const { el, section, actionLine } = h;
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
      actions.append(button("Remove seat", async () => {
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
  const go = el("button", "quiet", "Add a seat");
  go.type = "submit";
  add.append(email, go);
  add.onsubmit = async (e) => {
    e.preventDefault();
    try {
      await h.api("POST", "/api/org/seats", { email: email.value.trim() });
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
  const pay = el("div", "settings-actions");
  pay.append(portal());
  box.append(actionLine("Add a seat", add), actionLine("Add an admin", admin), el("p", "muted", `A seat added, ${price(offer.seatMicros)} a month and always on, bills from now, prorated; one removed is credited on the next invoice. Cancel in Payment and invoices.`), actionLine("Payment", pay));
  return box;
}
