// The page's dev mode: an in-memory mind standing in for ./__fragment.js,
// loaded by index.html only for `?mock` and never on a fragment's own host.
// It answers the mind's operations (docs/optchat.md, "Operations") over a
// made-up history: a year of notes imported from an older memory, three
// dozen threads across topics, two hand-offs (one still running), and the
// view folded from it all as the spec folds it (§5.2). Saying something
// runs a turn: settling, a look through memory, a streamed reply, and with
// a persona that has hands, a hand-off whose steps and words stream too.
//
// `?mock&act=<step>;;<step>…` drives it for screenshots once the page is
// up: `click:<selector>`, `wait:<ms>`, `type:<selector>=<text>`,
// `scroll:<selector>=<px|bottom>`, `key:<key>`.

const NOW = Date.now();
const MIN = 60_000;
const DAY = 86_400_000;
const NODE = 512;
const VIEW = 128_000;

// ---- a deterministic random ----
let seed = 7;
const rnd = () => ((seed = (seed * 1103515245 + 12345) % 2147483648) / 2147483648);
const pick = (a) => a[Math.floor(rnd() * a.length)];
const hex = (n) => [...Array(n)].map(() => Math.floor(rnd() * 16).toString(16)).join("");

// ---- the mind's tables ----
const personas = [
  { id: "p_mind", name: "Mind", emoji: "🌿", instructions: "Plain, warm, brief. Answer the question first; then what you remember that matters.", hands: false },
  { id: "p_builder", name: "Builder", emoji: "🛠️", instructions: "Does things on the computer: files, the shell, the web. Says what it will do, does it, and reports what changed.", hands: true },
  { id: "p_coach", name: "Coach", emoji: "🧭", instructions: "Asks one question at a time. Never lectures. Helps me find my own answer.", hands: false },
];
let defaultPersona = "p_mind";
let about = "I'm Paul. I build fragment (Rust, Cloudflare Workers). I live in Austin with Sam; we garden, bake bread, and I'm training for a half marathon in March.";

const topics = [
  { id: "tp_garden", name: "Garden", description: "The raised beds, the drip line, what's growing.", made: NOW - 200 * DAY },
  { id: "tp_kitchen", name: "Cooking", description: "Recipes, sourdough, what's for dinner.", made: NOW - 200 * DAY },
  { id: "tp_fragment", name: "fragment", description: "Work on fragment: the cell, the CLI, previews, the e2e.", made: NOW - 200 * DAY },
  { id: "tp_running", name: "Running", description: "Training for the half in March.", made: NOW - 150 * DAY },
  { id: "tp_lisbon", name: "Lisbon trip", description: "October in Lisbon with Sam.", made: NOW - 90 * DAY },
  { id: "tp_reading", name: "Reading", description: "Books, notes, what to read next.", made: NOW - 120 * DAY },
  { id: "tp_money", name: "Money", description: "Budget, taxes, the house fund.", made: NOW - 120 * DAY },
];

const log = []; // {i, kind, text, at, thread, persona, task}
const threads = new Map(); // id -> {id, title, persona, started, last, first_i, last_i}
const threadTopic = new Map(); // thread -> [{id, p}]
const tasks = new Map();

// ---- a year of imported notes ----
const NOTE_KINDS = [
  ["run", (d) => `ran ${(5 + rnd() * 11).toFixed(1)} km ${pick(["easy", "easy", "tempo", "with strides", "long, hilly"])}, ${pick(["5:48", "6:02", "6:10", "5:31", "6:15"])}/km${rnd() < 0.3 ? `; ${pick(["left calf tight", "felt great", "hot, 31°C", "with Sam"])}` : ""}`],
  ["garden", () => pick(["watered beds 15 min", "planted basil by the fence", "drip zone 2 = fence bed, 6am", "tomatoes flowering, first fruit set", "turned the compost; too wet, added cardboard", "harvested 2 lb tomatoes", "strawberry runners pinned", "aphids on kale, sprayed soap", "mulched the back bed"])],
  ["bake", () => `${pick(["fed Gus (starter) 1:1:1", "sourdough bake", "focaccia for Sam's friends", "baguettes, too pale", "rye loaf, dense"])}${rnd() < 0.5 ? `, ${pick(["72%", "75%", "78%", "80%"])} hydration` : ""}`],
  ["read", () => `read ${10 + Math.floor(rnd() * 60)} pages of ${pick(["Piranesi", "The Overstory", "Gödel, Escher, Bach", "A Psalm for the Wild-Built", "The Dispossessed", "Designing Data-Intensive Applications"])}`],
  ["work", () => pick(["fragment: e2e green on all four shards", "fragment: cut the TypeScript runtime", "fragment: preview deploys to finite.place work", "wrangler dev slow on first boot; cache was cold", "fragment: ledger metering lands", "decided: tooling in Rust only, no shell scripts", "the cell's sealing at rest reviewed", "Sprite cleanup postponed"])],
  ["life", () => pick(["prefers short answers; no bullet lists unless asked", "Sam is vegetarian", "allergic to cashews", "coffee: Chemex 1:16 at 94°C", "mom's birthday Nov 12", "dislikes more than one question at a time", "reads paper books before bed", "wants commit messages terse, no emoji", "dinner with the Okafors on Friday", "dentist moved to the 14th"])],
  ["money", () => pick(["house fund $" + (18 + Math.floor(rnd() * 12)) + "k", "paid Q2 estimated tax", "HYSA at 4.1%", "cancelled the gym membership", "budget: groceries $640 this month"])],
];
// `?mock&fresh`: a mind made a moment ago, with nothing in it yet
const FRESH = new URLSearchParams(location.search).has("fresh");
const NOTES = FRESH ? 0 : 6144;
const NOTE_START = NOW - 380 * DAY;
const NOTE_END = NOW - 110 * DAY;
for (let k = 0; k < NOTES; k++) {
  const [cat, make] = NOTE_KINDS[Math.floor(rnd() * NOTE_KINDS.length)];
  log.push({ i: k, kind: "note", text: make(new Date()), at: NOTE_START + Math.floor(((NOTE_END - NOTE_START) * k) / NOTES), thread: null, persona: null, task: null, cat });
}

// ---- the threads ----
// d: days ago, h: hour; turns: {u, steps?: [["search", q] | ["zoom"]], a, hand?: {...}}
const THREADS = [
  { d: 100, h: 21, p: "p_mind", t: ["reading"], turns: [{ u: "Can I export my Kindle highlights somewhere useful?", a: "Yes: Amazon's notebook page (read.amazon.com/notebook) lists every highlight by book. The cleanest export is the Clippings.txt file on the Kindle itself. Plug it in, copy `documents/My Clippings.txt`. It's plain text, one highlight per block, so I can turn it into notes per book if you paste it in." }] },
  { d: 95, h: 8, p: "p_coach", t: ["lisbon"], turns: [{ u: "I want to learn some basic Portuguese before Lisbon. Where do I start?", a: "What do you most want to be able to do there: order food, chat with people, or read signs and menus?" }, { u: "Order food and be polite, mostly.", a: "Then start with ten phrases, not an app streak. Which do you want first: greetings, or ordering?" }] },
  { d: 90, h: 17, p: "p_mind", t: ["garden"], turns: [{ u: "How do I start new strawberry plants from the runners?", a: "Pin each runner's first node into a small pot of soil with a bent paperclip, leave it attached to the mother for 2–3 weeks until it roots, then snip. Pinch off any runners past the second node so the plant puts energy into fruit next year." }] },
  { d: 85, h: 18, p: "p_mind", t: ["kitchen"], turns: [{ u: "Quick pickled onions ratio?", a: "1 red onion, thin. 1/2 cup vinegar (apple cider or red wine), 1/2 cup hot water, 1 tbsp sugar, 1.5 tsp salt. Pour over, 30 minutes on the counter, then the fridge. Good for two weeks." }] },
  { d: 80, h: 11, p: "p_mind", t: ["money"], turns: [{ u: "Car insurance renewal went up 22%. Worth shopping around?", a: "Yes. A 22% jump with no claims is usually just the insurer repricing. Get two quotes with the same coverage (you have 100/300/100, $500 deductible) and call your current one with the best: they often match." }] },
  { d: 75, h: 7, p: "p_mind", t: ["running"], turns: [{ u: "Is the Shoal Creek trail a decent bike commute route?", a: "It's lovely but slow: crushed granite in places and busy with runners before 8. For commuting, the Lamar bike lane is faster; take Shoal Creek home when you're not in a hurry." }] },
  { d: 70, h: 19, p: "p_mind", t: [], turns: [{ u: "The bedroom door squeaks like crazy", a: "Lift the hinge pins one at a time, wipe them, rub on a little bar soap or a drop of 3-in-One oil, and tap them back in. WD-40 works for a week, then the squeak comes back." }] },
  { d: 64, h: 14, p: "p_mind", t: ["fragment"], turns: [{ u: "Durable Objects vs D1 for the usage ledger?", a: "A Durable Object per payer. The ledger is a balance you reserve against and settle, which wants one writer and strong consistency per person; D1 is a shared database you would be serializing writes into anyway. Keep D1 (or R2) for the monthly rollups you only read." }] },
  { d: 60, h: 22, p: "p_mind", t: ["reading"], turns: [{ u: "Is The Overstory worth finishing? I'm 200 pages in and it's slow.", a: "If the slow part is the first section (the eight separate stories), yes: it braids together in 'Trunk' and gets much better. If you're past that and still not caring about the people, it's fine to stop. You said in spring you only want to finish books you'd recommend." }] },
  { d: 55, h: 16, p: "p_mind", t: ["kitchen"], turns: [{ u: "Fermented hot sauce from the garden peppers?", a: "Weigh peppers plus garlic, add 2.5% of that weight in salt, pack into a jar, weigh them down, and let them go 1–2 weeks at room temp, burping daily. Blend with a splash of vinegar. Keep it in the fridge." }] },
  { d: 52, h: 12, p: "p_mind", t: [], turns: [{ u: "Ideas for mom's birthday? Nov 12", a: "She loved the garden tour in spring. Some ideas: a good pair of pruners (Felco 2), a basket of your tomatoes and a sourdough loaf, or tickets to the botanical garden's winter lights. You said last year you want to give fewer things and more time." }] },
  { d: 48, h: 10, p: "p_mind", t: ["fragment"], turns: [{ u: "wrangler dev takes 90s to boot on the mac", a: "First boot pulls Chrome for Testing for Browser Rendering (about 145 MB) into target/cache. After that it should be ~6 s. If every boot is slow, check that `XDG_CACHE_HOME` points at target/cache and isn't being cleared by `--clean`." }] },
  { d: 44, h: 6, p: "p_coach", t: ["running"], turns: [{ u: "What should my tempo pace be?", a: "What's a pace you could hold for an hour if you had to, right now, today?" }, { u: "Maybe 5:15/km", a: "Then your tempo is about there: 5:10–5:20. How does that compare to what you've been running them at?" }] },
  { d: 40, h: 9, p: "p_mind", t: ["garden"], turns: [{ u: "Compost is soggy and smells", a: "Too much green, not enough brown. Turn it, mix in shredded cardboard or dry leaves (about two parts brown to one green), and leave the lid cracked for a few days. It should smell like soil again within a week." }] },
  { d: 36, h: 20, p: "p_mind", t: ["money"], turns: [{ u: "Should we refinance? Rates dropped to 5.6", a: "At 6.9% on $318k, 5.6% saves about $270 a month. With roughly $6k in closing costs you break even in 22 months. You plan to stay at least five years, so yes, if the closing costs come in near that." }] },
  { d: 33, h: 18, p: "p_mind", t: ["kitchen"], turns: [{ u: "miso soup ratio", a: "1 tbsp miso per cup of dashi. Whisk the miso into a ladle of warm dashi first, add it off the boil, and never let it boil after: it goes grainy and loses its aroma." }] },
  { d: 30, h: 15, p: "p_mind", t: ["fragment"], turns: [{ u: "should `fragment try` be renamed? people think it's a dry run", a: "Maybe `fragment new --open`? `try` scaffolds and deploys a template, which isn't trying anything. But you said in August you'd rather cut commands than rename them: it could just go, since `new --template` plus the printed link covers it." }] },
  { d: 27, h: 21, p: "p_mind", t: ["reading", "lisbon"], turns: [{ u: "A book for the flight to Lisbon?", a: "The Book of Disquiet, by Pessoa: you can open it anywhere, it's Lisbon on every page, and it suits a long flight. If you want a story instead: Night Train to Lisbon." }] },
  { d: 25, h: 7, p: "p_coach", t: ["running"], turns: [{ u: "Front of my shins hurt after runs", a: "Did your weekly distance go up recently?" }, { u: "From 28 to 40k in two weeks", a: "That's a big jump. What would happen if you dropped back to 30 for a week and saw how they feel?" }] },
  { d: 23, h: 8, p: "p_mind", t: ["garden"], turns: [{ u: "The drip line keeps clogging at the fence bed", a: "It's likely the emitters by the fence: hard water leaves scale. Flush the line (open the end cap, run it 2 minutes), soak the clogged emitters in vinegar overnight, and add a 200-mesh filter at the timer if there isn't one." }] },
  { d: 20, h: 11, p: "p_mind", t: ["money"], turns: [{ u: "How much should the Q3 estimated tax be?", a: "If income is about the same as Q2, pay the same $4,850 by September 15. The safe harbor is 110% of last year's tax split in four, which comes to $4,620 a quarter, so you're already over it." }] },
  { d: 16, h: 22, p: "p_mind", t: ["fragment"], turns: [{ u: "What if the memory was an MCP server? Then Claude Code could read it.", a: "Then every agent you use gets the same past: Claude Code could zoom into the view and search your log like I do. Read-only by default, and `note` as the one write, so an agent can leave you something without editing history." }] },
  { d: 14, h: 6, p: "p_coach", t: ["running"], turns: [{ u: "Build me a 20-week half marathon plan?", a: "Happy to. First: what time are you aiming for in March?" }, { u: "1:45", a: "That's 4:58/km. Your tempo runs are at 5:15 now. How many days a week can you really run, counting the weeks that go badly?" }] },
  { d: 12, h: 17, p: "p_mind", t: ["garden"], turns: [{ u: "Basil keeps bolting", a: "Pinch the flower spikes as soon as they show, and harvest from the top, just above a pair of leaves, every week. Afternoon shade helps in this heat; the fence bed gets sun until 6, which is a lot for basil." }] },
  { d: 10, h: 15, p: "p_mind", t: ["lisbon", "kitchen"], turns: [{ u: "Best pastel de nata in Lisbon, ranked", a: "1. **Manteigaria** (Chiado): crisp, barely sweet, warm all day.\n2. **Pastéis de Belém**: the original; go at 9am to skip the line.\n3. **Aloma**: a little richer, good if you're in Campo de Ourique.\n\nYou'll be in Príncipe Real, so Manteigaria is a ten-minute walk." }] },
  { d: 8, h: 18.5, p: "p_mind", t: ["kitchen"], turns: [{ u: "Weeknight dal, 30 minutes, Sam's vegetarian", a: "Red lentils, 1 cup, rinsed, simmered in 3 cups water with turmeric and salt for 20 minutes. Meanwhile, fry cumin seeds, then onion, garlic, ginger and a chopped tomato in ghee. Stir it into the lentils, finish with lemon and cilantro. It's vegetarian as is." }] },
  { d: 6, h: 21, p: "p_coach", t: [], turns: [{ u: "I want to say no to giving the talk in November but I feel bad", a: "What would you be saying yes to, if you said no?" }, { u: "Time to actually ship the Cloudflare cutover. And not travelling in November.", a: "Those sound like reasons, not excuses. Which weighs more right now: the guilt of saying no, or the cost of going?" }] },
  { d: 5, h: 10, p: "p_mind", t: ["fragment"], turns: [{ u: "shard 3 timed out again on the browser section. 40 min. any idea?", steps: [["search", "shard browser Chrome cache"]], a: "Last time it was Chrome's first launch: Chrome for Testing downloading into target/cache because the cache key missed after a package-lock change. Look in the shard's log for `Downloading Chrome` at the start of the browser section." }, { u: "yep that's it. the lock changed again.", a: "Then the next push to master saves the new cache, and shard 3 should be back to about 11 minutes. Worth a line in the debt ledger: the cache key could name the Chrome version instead of the whole lock." }] },
  { d: 4, h: 20, p: "p_mind", t: ["money"], turns: [{ u: "If I put $1,800 a month into the house fund starting October, when do we hit $60k? We're at $31,400.", a: "$28,600 to go at $1,800 a month is 15.9 months: your 16th deposit, in **January 2028**, takes you to $60,200. At the HYSA's 4.1%, interest gets you there about a month sooner, in December 2027." }] },
  { d: 3, h: 13, p: "p_mind", t: ["reading"], turns: [{ u: "What did I think of Piranesi when I finished it? Someone asked me for a rec", steps: [["search", "Piranesi"], ["zoom"]], a: "You finished it in March and called it \"the calmest book about being lost\". You loved the House and its tides, found the mystery a little thin in the last third, and said you'd give it to anyone who liked *The Library of Babel*.\n\nYour own one-liner: **short, strange, kind.** That's the rec." }] },
  { d: 2, h: 16, p: "p_builder", t: ["fragment"], turns: [{ u: "Delete the claude-* preview fragments older than a week on finite.place, but leave anything named demo-* alone", hand: { id: "k_7c1d2e", say: "On it. I've handed the sweep to your computer; it will skip anything named `demo-*`.", steps: [["shell", "fragment list --host claude-cards.finite.place"], ["shell", "fragment list --host claude-sync.finite.place"], ["shell", "fragment list --host claude-ledger.finite.place"], ["shell", "cargo xtask e2e --hosted --sweep-all --branch claude-cards"], ["shell", "cargo xtask e2e --hosted --sweep-all --branch claude-sync"], ["shell", "cargo xtask e2e --hosted --sweep-all --branch claude-ledger"]], report: "Swept 14 e2e fragments older than 7 days across three previews (claude-cards 6, claude-sync 5, claude-ledger 3). Kept demo-board and demo-notes as asked. That freed about 212 MB of R2. Nothing failed.", after: "Done: 14 old fragments are gone across claude-cards, claude-sync and claude-ledger, about 212 MB freed. I left demo-board and demo-notes alone." } }] },
  { d: 2, h: 9, p: "p_mind", t: ["kitchen"], turns: [{ u: "My sourdough keeps spreading flat. 78% hydration, King Arthur bread flour. What's going wrong?", steps: [["search", "sourdough hydration"]], a: "With bread flour, 78% is high for a loaf that holds its shape, and your kitchen has been warm (26°C last week). In order:\n\n1. Drop to **72%** for the next bake.\n2. Shorten the bulk: at 26°C it's likely done in 4 hours, not 6. Look for a 50% rise and domed edges.\n3. Preshape tighter and rest 20 minutes before the final shape.\n\nChange one thing at a time so you know which one fixed it." }, { u: "72% it is. I'll report back.", a: "Good. Note the rise at 3 and 4 hours; that tells us whether bulk was the problem." }] },
  { d: 1, h: 13, p: "p_mind", t: ["lisbon"], turns: [{ u: "For Lisbon, Alfama or Príncipe Real? We want walkable, not too party-ish, and good coffee.", steps: [["search", "Lisbon"]], a: "**Príncipe Real.** Alfama is beautiful but steep, and the fado bars run late right under the windows. Príncipe Real is calm at night, flat by Lisbon standards, and a ten-minute walk to Chiado and Bairro Alto without being in them.\n\nCoffee: *Hello, Kristof* and *Copenhagen Coffee Lab* are both close. You wanted one day in Sintra: the train from Rossio is fifteen minutes' walk away." }] },
  { d: 1, h: 19.5, p: "p_coach", t: ["running"], turns: [{ u: "Long run this morning felt awful. 14k and I was dying by 9.", a: "That sounds rough. Before we look for a reason: how did you sleep the two nights before?" }, { u: "Badly. Maybe 5 hours Friday. And I skipped breakfast.", a: "That's most of it, then: short sleep, and running fasted past an hour. Your easy pace has been about 6:05/km. What pace did today's run start at?" }] },
  { ago: 9, p: "p_builder", t: ["garden", "fragment"], turns: [{ u: "Can you make me a little web page for the garden log? What I planted where and when, and the watering schedule. Make it a fragment so Sam can see it.", hand: { id: "k_4f2a91", live: true, say: "On it. I've handed this to your computer: it will build the page from what I remember of the beds (the fence bed's tomatoes and basil, the back bed, the drip zones) and publish it as a fragment Sam can open. I'll tell you when it's up.", steps: [["shell", "fragment new garden-log --template blank"], ["mind", "search {\"q\":\"planted\"}"], ["mind", "zoom {\"id\":1024,\"n\":256}"], ["edit", "site/index.html"], ["edit", "site/garden.css"], ["shell", "fragment deploy garden-log"], ["?", "Share garden-log with Sam (sam@okafor.me) as a viewer?"], ["shell", "fragment share garden-log --with sam"], ["browser", "open https://garden-log--paul.fragment.boats/"]], report: "The page is up at https://garden-log--paul.fragment.boats/ and shared with Sam (viewer).\n\n- **Beds**: the fence bed (tomatoes since April 2, basil since May 20) and the back bed (kale, strawberries from runners).\n- **Watering**: drip zone 2 (fence bed) at 6:00 for 15 minutes, zone 1 at 6:20 for 10.\n- It reads from a `log.md` in the fragment, so adding a line there updates the page." } }] },
  { d: 0, h: 9.2, p: "p_mind", t: ["garden"], turns: [{ u: "The tomato leaves are curling up at the edges, mostly the new ones. Too much sun?", steps: [["search", "tomatoes"], ["zoom"], ["search", "drip zone 2"]], a: "Probably heat, not disease. The fence bed gets sun until 6, and you moved zone 2 to one 15-minute run at 6am. Leaf roll on new growth after a hot spell is the plant saving water; it's harmless while the fruit keeps setting.\n\nTwo things to try:\n- A second drip run at 4pm on days over 32°C.\n- 30% shade cloth over the fence bed only.\n\nIf the curl turns yellow or spotty, send me a photo." }, { u: "ok add the 4pm run. remind me what timer we bought?", steps: [["search", "timer"]], a: "The **Orbit B-hyve**, 4 zones, from May; zone 2 is the fence bed. In its app: *Zone 2 → Watering schedule → add a start time*, 4:00 PM, 10 minutes, and turn on *Weather sensitivity* so it skips cool days." }] },
];

const TOPIC_IDS = { garden: "tp_garden", kitchen: "tp_kitchen", fragment: "tp_fragment", running: "tp_running", lisbon: "tp_lisbon", reading: "tp_reading", money: "tp_money" };

function push(kind, text, at, thread, persona, task = null) {
  const m = { i: log.length, kind, text, at, thread, persona, task };
  log.push(m);
  return m;
}

const fills = []; // echoes written once the log is whole
// a thread starts `ago` minutes back, else on day `d` at hour `h` (and before now)
const hourNow = new Date(NOW).getHours() + new Date(NOW).getMinutes() / 60;
for (const def of THREADS) def.start = def.ago ? NOW - def.ago * MIN : Math.min(NOW - def.d * DAY + (def.h - hourNow) * 3600_000, NOW - (90 + def.h) * MIN);
const sorted = FRESH ? [] : [...THREADS].sort((a, b) => a.start - b.start);
if (FRESH) topics.length = 0;
for (const def of sorted) {
  const id = `t_${hex(16)}`;
  let at = def.start;
  const th = { id, title: def.turns[0].u.split("\n")[0].slice(0, 80), persona: def.p, started: at, last: at, first_i: log.length, last_i: log.length };
  threads.set(id, th);
  threadTopic.set(id, def.t.map((t) => ({ id: TOPIC_IDS[t], p: 0.7 + rnd() * 0.29 })));
  for (const turn of def.turns) {
    push("user", turn.u, at, id, def.p);
    at += 40_000;
    for (const [tool, q] of turn.steps ?? []) {
      const call = push("tool", "", (at += 2000), id, def.p);
      const echo = push("echo", "", (at += 1500), id, def.p);
      fills.push({ call, echo, tool, q });
    }
    if (turn.hand) {
      const hd = turn.hand;
      const task = { id: hd.id, thread: id, text: `${turn.u}\n\n(task ${hd.id}, thread ${id})`, state: hd.live ? "running" : "done", report: hd.live ? null : hd.report, started: at, ended: hd.live ? null : at + 6 * MIN };
      // what goose did before the page opened: put on `work` below
      task.done = hd.steps.slice(0, hd.live ? 5 : hd.steps.length);
      tasks.set(hd.id, task);
      task.plan = hd;
      task.i = push("tool", `computer ${JSON.stringify({ task: turn.u })}`, (at += 2000), id, def.p, hd.id).i;
      push("echo", `[${hd.id}] started`, (at += 800), id, def.p, hd.id);
      push("talk", hd.say, (at += 6000), id, def.p);
      if (!hd.live) {
        push("user", `[${hd.id}] ${hd.report}`, (at += 6 * MIN), id, def.p, hd.id);
        push("talk", hd.after, (at += 20_000), id, def.p);
      }
    } else {
      push("talk", turn.a, (at += 9000), id, def.p);
    }
    at += 3 * MIN + rnd() * 6 * MIN;
  }
  th.last = log.at(-1).at;
  th.last_i = log.length - 1;
}

// ---- the tree (spec §3), made up but shaped like the real one ----
const nodeCache = new Map();
const cut = (s, max) => (s.length <= max ? s : `${s.slice(0, max - 1).trimEnd()}…`);
const firstSentence = (s) => cut(String(s).replace(/\s+/g, " ").replace(/[*_`#]/g, "").split(/(?<=[.!?])\s/)[0], 150);
const NOTE_PHRASE = { run: "runs", garden: "garden", bake: "baking", read: "reading", work: "fragment", life: "life", money: "money" };

function summarize(from, to) {
  // the messages [from, to): a line as the compactor might write it
  const items = [];
  const notes = [];
  const seen = new Set();
  for (let i = from; i < to && i < log.length; i++) {
    const m = log[i];
    if (m.kind === "note") notes.push(m);
    else if (m.thread && !seen.has(m.thread)) {
      seen.add(m.thread);
      const first = log.slice(i, to).find((x) => x.thread === m.thread && x.kind === "talk");
      items.push(`user: ${cut(threads.get(m.thread).title, 70)}`);
      if (first) items.push(`talk: ${cut(firstSentence(first.text), 90)}`);
      const rep = log.slice(i, to).find((x) => x.thread === m.thread && x.kind === "user" && x.text.startsWith("["));
      if (rep) items.push(`work: ${cut(firstSentence(rep.text.replace(/^\[[^\]]+\]\s*/, "")), 80)}`);
    }
  }
  if (notes.length) {
    const by = new Map();
    for (const n of notes) {
      if (!by.has(n.cat)) by.set(n.cat, []);
      by.get(n.cat).push(n.text);
    }
    const parts = [...by].sort((a, b) => b[1].length - a[1].length).map(([cat, list]) => (list.length === 1 ? list[0] : `${NOTE_PHRASE[cat]} ×${list.length}: ${[...new Set(list)].slice(0, 2).join(", ")}`));
    items.unshift(`note: ${parts.join("; ")}`);
  }
  let out = "";
  for (const it of items) {
    if ((out + "; " + it).length > NODE - 12) break;
    out = out ? `${out}; ${it}` : it;
  }
  return out || cut(items[0] ?? "", NODE - 12);
}

function nodeText(l, i) {
  const k = `${l}:${i}`;
  if (nodeCache.has(k)) return nodeCache.get(k);
  let text;
  if (l === 0) {
    const m = log[i];
    const whole = `${m.kind}: ${m.text.replace(/\n+/g, " ")}`;
    text = whole.length <= NODE ? whole : `${m.kind}: ${cut(firstSentence(m.text), 300)}`;
  } else {
    const a = nodeText(l - 1, 2 * i);
    const b = nodeText(l - 1, 2 * i + 1);
    text = a.length + b.length + 1 <= NODE ? `${a}\n${b}` : summarize(i * 2 ** l, (i + 1) * 2 ** l);
  }
  nodeCache.set(k, text);
  return text;
}
const bytes = (l, i) => new TextEncoder().encode(nodeText(l, i)).length;

// echoes, now that the log is whole
for (const f of fills) {
  if (f.tool === "search") {
    const hits = searchLog(f.q, 5, f.echo.i);
    f.call.text = `search ${JSON.stringify({ q: f.q })}`;
    f.echo.text = hits.length ? hits.map((x) => `${x.i}+1|${x.kind}: ${x.snippet}`).join("\n") : "No results.";
    f.anchor = hits[0]?.i;
  } else {
    const prev = fills.filter((x) => x.echo.thread === f.echo.thread && x.anchor !== undefined && x.echo.i < f.echo.i).at(-1);
    const target = prev?.anchor ?? Math.max(0, f.echo.i - 40);
    const id = Math.floor(target / 8) * 8;
    f.call.text = `zoom ${JSON.stringify({ id, n: 8 })}`;
    f.echo.text = `${id}+4|${nodeText(2, id / 4).replace(/\n/g, " ")}\n${id + 4}+4|${nodeText(2, id / 4 + 1).replace(/\n/g, " ")}`;
  }
}
// the compactor builds in order (spec §4.1), so "built" is a prefix
let summarized = log.length;

// ---- the view, folded (spec §5.2) ----
let view = [];
let viewBytes = 0;
const isBuilt = (l, i) => (i + 1) * 2 ** l <= summarized;
const partBytes = (p) => (isBuilt(p.l, p.i) ? bytes(p.l, p.i) : 36);
function fit(recount = false) {
  const T = log.length;
  if (recount) viewBytes = view.reduce((s, p) => s + partBytes(p), 0);
  while (viewBytes > VIEW) {
    let best = -1;
    let due = -1;
    for (let k = 0; k + 1 < view.length; k++) {
      const a = view[k];
      const b = view[k + 1];
      if (a.l === b.l && a.i % 2 === 0 && b.i === a.i + 1 && isBuilt(a.l + 1, a.i / 2)) {
        const d = (T - a.i * 2 ** a.l) / 2 ** (a.l + 2);
        if (d > due) {
          due = d;
          best = k;
        }
      }
    }
    if (best < 0) break;
    const a = view[best];
    viewBytes -= partBytes(a) + partBytes(view[best + 1]);
    view.splice(best, 2, { l: a.l + 1, i: a.i / 2 });
    viewBytes += partBytes(view[best]);
  }
}
function append(i) {
  view.push({ l: 0, i });
  viewBytes += partBytes(view.at(-1));
  if (viewBytes > VIEW) fit();
}
for (let i = 0; i < log.length; i++) append(i);
fit(true);

// ---- queries ----
function searchLog(q, limit = 20, before = log.length) {
  const words = String(q).toLowerCase().match(/[\p{L}\p{N}]{2,}/gu) ?? [];
  if (!words.length) return [];
  const out = [];
  const said = new Set();
  for (let i = Math.min(before, log.length) - 1; i >= 0 && out.length < limit; i--) {
    const m = log[i];
    if (m.kind === "tool" || m.kind === "echo") continue;
    const low = m.text.toLowerCase();
    if (!words.every((w) => low.includes(w)) || said.has(low)) continue;
    said.add(low);
    const at = low.indexOf(words[0]);
    const from = Math.max(0, at - 50);
    const flat = m.text.replace(/\s+/g, " ");
    const snippet = `${from > 0 ? "…" : ""}${flat.slice(from, from + 150)}${from + 150 < flat.length ? "…" : ""}`;
    out.push({ i, kind: m.kind, thread: m.thread, at: m.at, snippet });
  }
  return out;
}

// what a thread came to, as a compacted line reads: its last exchange
const summaryOf = (th) => {
  const msgs = log.slice(th.first_i, th.last_i + 1).filter((m) => m.thread === th.id);
  const users = msgs.filter((m) => m.kind === "user");
  const a = msgs.filter((m) => m.kind === "talk").at(-1);
  const u = users.length > 1 ? users.at(-1).text.replace(/^\[[^\]]+\]\s*/, "") : th.title;
  const two = a ? a.text.replace(/\s+/g, " ").replace(/[*_`#]/g, "").split(/(?<=[.!?])\s/).slice(0, 2).join(" ") : "";
  return `user: ${cut(u, 64)}; talk: ${cut(two, 200)}`;
};
const publicThread = (th) => ({ id: th.id, title: th.title, persona: th.persona, started: th.started, last: th.last, summary: summaryOf(th), topics: threadTopic.get(th.id) ?? [], count: log.slice(th.first_i, th.last_i + 1).filter((m) => m.thread === th.id).length });
const publicMsg = ({ i, kind, text, at, thread, persona, task }) => ({ i, kind, text, at, thread, persona, task });
// a task as `log` carries it: goose's steps are `work`'s, and its turn is
// left out, so the page finds it the exact way (turn.start's cause)
const publicTask = (t) => ({ id: t.id, thread: t.thread, text: t.text, state: t.state, ...(t.report ? { report: t.report } : {}) });
let turnNow = null; // {thread, since}

const QUERIES = {
  view: () => ({ text: `<chat>\n${view.map((p) => `${p.i * 2 ** p.l}+${2 ** p.l}|${nodeText(p.l, p.i).replace(/\n/g, " ")}`).join("\n")}\n</chat>`, bytes: viewBytes, parts: view.length, T: log.length, settled: view.every((p) => isBuilt(p.l, p.i)) }),
  search: ({ q, limit = 20, thread }) => ({ results: searchLog(q, Math.min(limit, 100)).filter((r) => !thread || r.thread === thread) }),
  threads: ({ topic, before, limit = 30 }) => ({
    threads: [...threads.values()]
      .filter((t) => !topic || (threadTopic.get(t.id) ?? []).some((x) => x.id === topic && x.p >= 0.6))
      .filter((t) => before === undefined || t.last < before)
      .sort((a, b) => b.last - a.last)
      .slice(0, limit)
      .map(publicThread),
  }),
  thread: ({ id, before, limit = 100 }) => {
    const th = threads.get(id);
    if (!th) throw Object.assign(new Error("no such thread"), { status: 404 });
    const all = log.filter((m) => m.thread === id && (before === undefined || m.i < before));
    return { thread: publicThread(th), messages: all.slice(-limit).map(publicMsg), more: all.length > limit };
  },
  context: ({ i, before = 3, after = 3 }) => ({ messages: log.slice(Math.max(0, i - before), i + after + 1).map(publicMsg) }),
  memory: () => ({ parts: view.map((p) => ({ id: p.i * 2 ** p.l, n: 2 ** p.l, text: isBuilt(p.l, p.i) ? nodeText(p.l, p.i) : "", built: isBuilt(p.l, p.i) })), bytes: viewBytes, T: log.length }),
  node: ({ id, n }) => {
    if (n === 1) return { message: publicMsg(log[id]) };
    const l = Math.log2(n);
    const i = id / n;
    return { children: [0, 1].map((k) => ({ id: id + (k * n) / 2, n: n / 2, text: nodeText(l - 1, 2 * i + k) })) };
  },
  date: ({ id }) => ({ text: new Date(log[id]?.at ?? NOW).toISOString() }),
  zoom: ({ id, n }) => ({ text: n === 1 ? `${id}+0|${log[id].kind}: ${log[id].text}` : QUERIES.node({ id, n }).children.map((c) => `${c.id}+${c.n}|${c.text}`).join("\n") }),
  topics: () => ({ topics: topics.map((t) => ({ id: t.id, name: t.name, description: t.description, count: [...threadTopic.values()].filter((l) => l.some((x) => x.id === t.id && x.p >= 0.6)).length })) }),
  personas: () => ({ personas: personas.map((p) => ({ ...p })), default: defaultPersona }),
  tasks: ({ thread } = {}) => ({ tasks: [...tasks.values()].filter((t) => !thread || t.thread === thread).map((t) => ({ ...publicTask(t), i: t.i, steps: [], report: t.report ?? null, started: t.started, ended: t.ended })) }),
  status: () => ({ turn: turnNow ? { running: true, thread: turnNow.thread, since: turnNow.since } : null, queued: queue.length, unbuilt: view.filter((p) => !isBuilt(p.l, p.i)).length, T: log.length, hands: true }),
  settings: () => ({ about }),
};

const MUTATIONS = {
  topic_add: ({ name, description = "" }) => {
    const id = `tp_${hex(8)}`;
    topics.push({ id, name, description, made: Date.now() });
    // a pretend Clef: a thread is in when a word of the name is in it
    setTimeout(() => {
      const words = name.toLowerCase().match(/[\p{L}\p{N}]{4,}/gu) ?? [name.toLowerCase()];
      for (const th of threads.values()) {
        const text = log.slice(th.first_i, th.last_i + 1).map((m) => m.text.toLowerCase()).join(" ");
        if (words.some((w) => text.includes(w))) {
          const list = (threadTopic.get(th.id) ?? []).filter((x) => x.id !== id);
          list.push({ id, p: 0.9 });
          threadTopic.set(th.id, list);
          publish("log", { type: "topics", thread: th.id, topics: list });
        }
      }
      changed();
    }, 900);
    return { id };
  },
  topic_remove: ({ id }) => {
    const at = topics.findIndex((t) => t.id === id);
    if (at >= 0) topics.splice(at, 1);
    for (const [k, list] of threadTopic) threadTopic.set(k, list.filter((x) => x.id !== id));
    return {};
  },
  persona_set: ({ id, name, emoji, instructions, hands }) => {
    const p = personas.find((x) => x.id === id);
    if (p) Object.assign(p, { name, emoji, instructions, hands: !!hands });
    else personas.push({ id: (id = `p_${hex(8)}`), name, emoji, instructions, hands: !!hands });
    return { id };
  },
  persona_remove: ({ id }) => {
    const at = personas.findIndex((p) => p.id === id);
    if (at >= 0 && id !== defaultPersona) personas.splice(at, 1);
    return {};
  },
  persona_default: ({ id }) => {
    if (personas.some((p) => p.id === id)) defaultPersona = id;
    return {};
  },
  settings_set: ({ about: a }) => {
    about = String(a ?? "");
    return {};
  },
  note: ({ text }) => {
    const m = push("note", text, Date.now(), null, null);
    publish("log", { type: "msg", ...publicMsg(m) });
    append(m.i);
    return { i: m.i };
  },
  stop: ({ thread }) => {
    if (turnNow?.thread === thread) turnNow.stop = true;
    return {};
  },
  topic_suggest: () => {
    setTimeout(() => {
      publish("log", { type: "suggest", names: ["Health", "Sam", "Home repairs", "Coffee", "Work trips"] });
      changed();
    }, 1600);
    return { run: `run_${hex(8)}`, status: "running" };
  },
};

// ---- channels, drafts, live queries ----
const channels = { log: [], chat: [], say: [], work: [] };
const subs = [];
const lives = new Set();
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

function publish(channel, body, principal = "fragment") {
  const rec = { type: "record", channel, seq: channels[channel].length + 1, principal, body, at: Date.now() };
  channels[channel].push(rec);
  for (const s of subs) if (s.channel === channel) s.onRecord(rec);
}
function draft(channel, turn, text, principal = "fragment") {
  for (const s of subs) if (s.channel === channel && s.onDraft) s.onDraft({ type: "draft", channel, principal, turn, text, at: Date.now() });
}
let again = 0;
function changed() {
  clearTimeout(again);
  again = setTimeout(() => lives.forEach(runLive), 40);
}
async function runLive(l) {
  if (!lives.has(l)) return;
  try {
    const r = structuredClone(QUERIES[l.op](l.input ?? {}));
    l.onResult(r);
  } catch (e) {
    l.onError?.(e);
  }
}

// ---- hand-offs, as chat-records has them ----
const GOOSE = "id:goose";
const work = (body) => publish("work", body, GOOSE);

/// The mind hands a task over: its message on `chat`, to goose.
function handOver(task) {
  publish("chat", { text: task.text, to: [GOOSE] });
  task.seq = channels.chat.length;
  task.turnId = `wt_${hex(24)}`;
}

// seed the channels with the recent history's records, as they were written
for (const t of [...tasks.values()].sort((a, b) => a.started - b.started)) {
  handOver(t);
  publish("log", { type: "task", ...publicTask(t) });
  work({ kind: "turn.start", turn: t.turnId, agent: GOOSE, cause: { channel: "chat", seq: t.seq } });
  t.done.filter(([tool]) => tool !== "?").forEach(([tool, args], k) => work({ kind: "turn.step", turn: t.turnId, step: k + 1, tool, args, ok: true }));
  t.next = t.done.length;
  if (t.state !== "running") {
    work({ kind: "turn.end", turn: t.turnId, outcome: "idle" });
    publish("chat", { text: t.report, turn: t.turnId }, GOOSE);
  }
}
for (const th of [...threads.values()].sort((a, b) => a.last - b.last).slice(-8)) publish("log", { type: "turn", thread: th.id, state: "done" });

// ---- turns ----
const queue = [];
let turning = false;

function touch(thread, text, personaId) {
  let th = threads.get(thread);
  if (!th) {
    th = { id: thread, title: text.split("\n")[0].slice(0, 80), persona: personaId, started: Date.now(), last: Date.now(), first_i: log.length, last_i: log.length };
    threads.set(thread, th);
    threadTopic.set(thread, []);
    publish("log", { type: "thread", id: thread, title: th.title });
  }
  if (personaId) th.persona = personaId;
  return th;
}

function logged(kind, text, thread, personaId, task = null) {
  const m = push(kind, text, Date.now(), thread, personaId, task);
  const th = threads.get(thread);
  if (th) {
    th.last = m.at;
    th.last_i = m.i;
  }
  append(m.i);
  publish("log", { type: "msg", ...publicMsg(m) });
  changed();
  return m;
}

// a pretend Clef: a thread is in a topic when a word of the topic is in it
function classify(thread) {
  const th = threads.get(thread);
  if (!th) return;
  const text = log.slice(th.first_i).filter((m) => m.thread === thread && (m.kind === "user" || m.kind === "talk")).map((m) => m.text.toLowerCase()).join(" ");
  const list = topics.filter((t) => (`${t.name} ${t.description}`.toLowerCase().match(/[\p{L}]{5,}/gu) ?? []).some((w) => text.includes(w))).map((t) => ({ id: t.id, p: 0.9 }));
  threadTopic.set(thread, list);
  publish("log", { type: "topics", thread, topics: list });
  changed();
}

// a pretend compactor: new messages are summarized a moment later
function pump() {
  setTimeout(() => {
    summarized = log.length;
    fit(true);
    changed();
  }, 2200);
}

const REPLIES = [
  (hit) => `${hit ? `This came up before: on ${new Date(hit.at).toLocaleDateString([], { month: "long", day: "numeric" })} you said “${cut(hit.snippet.replace(/^…/, ""), 90)}”.\n\n` : ""}Here's my take: start small, keep what works, and tell me how it goes. I'll remember either way.`,
  (hit) => `Good question.${hit ? ` Last time we talked about something close to this (message #${hit.i}), and the short version was: ${cut(hit.snippet.replace(/^…/, ""), 100)}` : ""}\n\nIf you want, I can turn this into a checklist.`,
  () => "Got it. I'll keep that in mind for every chat from here on, whichever persona you're talking to.",
];

// as the platform streams: whole text so far, and no closing frame (the
// `talk` record or the turn's end replaces it); one late frame, as a
// throttled stream can send after the record
async function stream(thread, personaId, text) {
  const turn = `turn:${thread}`;
  const words = text.split(/(?<=\s)/);
  let so = "";
  for (const w of words) {
    if (turnNow?.stop) return false;
    so += w;
    draft("log", turn, so);
    await sleep(28 + rnd() * 40);
  }
  logged("talk", text, thread, personaId);
  setTimeout(() => draft("log", turn, so), 120);
  return true;
}

async function runTurn() {
  if (turning) return;
  turning = true;
  while (queue.length) {
    const { thread, text, persona: pid, report } = queue.shift();
    turnNow = { thread, since: Date.now(), stop: false };
    const p = personas.find((x) => x.id === pid) ?? personas[0];
    changed();
    publish("log", { type: "turn", thread, state: "settling" });
    await sleep(700);
    publish("log", { type: "turn", thread, state: "thinking" });
    await sleep(600);
    let ok = true;
    const words = (text.toLowerCase().match(/[\p{L}]{5,}/gu) ?? []).sort((a, b) => b.length - a.length);
    let hit = null;
    if (report) {
      ok = await stream(thread, p.id, `Your computer is done. ${firstSentence(report.report)}\n\nWant me to change anything on it?`);
    } else if (words.length && !turnNow.stop) {
      const q = words.slice(0, 2).join(" ");
      logged("tool", `search ${JSON.stringify({ q })}`, thread, p.id);
      await sleep(500);
      const hits = searchLog(words[0], 4);
      hit = hits.find((x) => x.thread !== thread) ?? null;
      logged("echo", hits.length ? hits.map((x) => `${x.i}+1|${x.kind}: ${x.snippet}`).join("\n") : "No results.", thread, p.id);
      await sleep(400);
    }
    if (report) {
      // answered above
    } else if (p.hands && /\b(make|build|set up|create|fix|clean|check|install|deploy)\b/i.test(text) && !turnNow.stop) {
      const id = `w${hex(8)}-3`;
      const call = logged("tool", `computer ${JSON.stringify({ task: text })}`, thread, p.id, id);
      const task = { id, thread, i: call.i, text: `${text}\n\n(task ${id}, thread ${thread})`, state: "running", report: null, started: Date.now(), ended: null, next: 0, plan: { id, steps: [["shell", "ls ~/work"], ["edit", "notes/plan.md"], ["shell", "fragment deploy"], ["browser", "check the page"]], report: "Made it and checked it in the browser: it works on a phone too. What I did is written up in `notes/plan.md`.", narration: ["Looking at what's in ~/work first. ", "Writing it down as a plan, then the page itself. ", "Deploying it as a fragment. ", "Opening it in the browser to check it. "] } };
      tasks.set(id, task);
      handOver(task);
      publish("log", { type: "task", ...publicTask(task) });
      logged("echo", `[${id}] started`, thread, p.id, id);
      ok = await stream(thread, p.id, `On it. I've handed this to your computer and I'll tell you when it's done.`);
      hands(task, 1400);
    } else if (!turnNow.stop) {
      ok = await stream(thread, p.id, pick(REPLIES)(hit));
    }
    publish("log", { type: "turn", thread, state: ok && !turnNow.stop ? "done" : "stopped" });
    turnNow = null;
    changed();
    pump();
    setTimeout(() => classify(thread), 1200);
  }
  turning = false;
}

/// goose on the computer, pretend, as the bridge writes a turn: its claim
/// on `work`, a step at a time there, its words as `chat`'s draft, its
/// end, and its one reply (the report) on `chat`; then the mind's half.
async function hands(task, every) {
  const turn = task.turnId;
  if (task.next === 0) work({ kind: "turn.start", turn, agent: GOOSE, cause: { channel: "chat", seq: task.seq } });
  const plan = task.plan;
  const narration = plan.narration ?? [
    "Starting from what the mind remembers about the beds. ",
    "I'll scaffold a blank fragment and write the page by hand: no framework, one HTML file and a stylesheet. ",
    "The planting dates come from your notes: the tomatoes went in on April 2, basil by the fence on May 20. ",
    "Watering schedule: zone 2 at 6:00 for 15 minutes, zone 1 at 6:20 for 10. ",
    "Deploying it now, then sharing it with Sam as a viewer. ",
    "Checking the page in the browser to make sure it renders on a phone. ",
  ];
  let said = "";
  let k = task.next;
  let step = plan.steps.slice(0, k).filter(([tool]) => tool !== "?").length;
  let n = 0;
  while (k < plan.steps.length) {
    const words = (narration[n++ % narration.length] ?? "").split(/(?<=\s)/);
    for (const w of words) {
      said += w;
      draft("chat", turn, said, GOOSE);
      await sleep(70);
    }
    await sleep(every);
    const [tool, args] = plan.steps[k];
    k++;
    if (tool === "?") {
      // goose asks the person first (a `turn.prompt`), and waits
      const prompt = `p_${hex(8)}`;
      work({ kind: "turn.prompt", turn, prompt, text: args, options: [{ id: "once", label: "Share it", style: "primary" }, { id: "deny", label: "Not now", style: "danger" }], asks: "id:paul", expiresAt: Date.now() + 10 * MIN });
      changed();
      const option = await new Promise((resolve) => asking.set(prompt, resolve));
      work({ kind: "turn.prompt.closed", turn, prompt, outcome: "answered", option, by: "id:paul" });
      if (option === "deny") k++;
    } else work({ kind: "turn.step", turn, step: ++step, tool, args, ok: true });
    changed();
  }
  await sleep(every);
  work({ kind: "turn.end", turn, outcome: "idle" });
  publish("chat", { text: plan.report, turn }, GOOSE);
  // the mind's half: the task ends, and its report is a message in the thread
  await sleep(600);
  task.state = "done";
  task.report = plan.report;
  task.ended = Date.now();
  publish("log", { type: "task", ...publicTask(task) });
  const th = threads.get(task.thread);
  logged("user", `[${task.id}] ${plan.report}`, task.thread, th?.persona, task.id);
  queue.push({ thread: task.thread, text: "", persona: th?.persona, report: task });
  runTurn();
}

// goose's open questions: prompt -> its answer's resolve
const asking = new Map();

// the hand-off that is running as the page opens
for (const t of tasks.values()) if (t.state === "running") setTimeout(() => hands(t, 3800), 400);

// ---- the browser library's face ----
export function call(op, input = {}) {
  return sleep(40 + rnd() * 60).then(() => {
    if (QUERIES[op]) return structuredClone(QUERIES[op](input));
    if (MUTATIONS[op]) {
      const r = MUTATIONS[op](input);
      changed();
      return r;
    }
    throw Object.assign(new Error(`no operation ${op}`), { status: 404 });
  });
}

export async function post(channel, body) {
  await sleep(60);
  if (channel === "chat" && body?.kind === "prompt_response") {
    // the first answer wins, as on the real channel
    publish("chat", body, "id:paul");
    asking.get(body.prompt)?.(body.option);
    asking.delete(body.prompt);
    return { seq: channels.chat.length };
  }
  if (channel !== "say") throw new Error(`cannot post to ${channel}`);
  publish("say", body, "id:paul");
  touch(body.thread, body.text, body.persona);
  logged("user", body.text, body.thread, body.persona ?? null);
  queue.push({ thread: body.thread, text: body.text, persona: body.persona ?? threads.get(body.thread)?.persona });
  runTurn();
  return { seq: channels.say.length };
}

export function live(op, input, onResult, onError) {
  const l = { op, input, onResult, onError };
  lives.add(l);
  setTimeout(() => runLive(l), 30);
  return () => lives.delete(l);
}

export function subscribe(channel, onRecord, { last = null, after = 0, onDraft = null } = {}) {
  const s = { channel, onRecord, onDraft };
  setTimeout(() => {
    const backlog = last != null ? channels[channel].slice(-last) : channels[channel].filter((r) => r.seq > after);
    for (const r of backlog) onRecord(r);
    subs.push(s);
  }, 20);
  return () => subs.splice(subs.indexOf(s), 1);
}

export const presence = { set() {}, on: () => () => {} };
export const closed = () => () => {};
export const me = () => Promise.resolve({ id: "s1", principal: "id:paul", role: "owner" });
export const people = async (ids) => ({ profiles: Object.fromEntries(ids.map((id) => [id, { kind: "person", username: "paul", name: "Paul Miller", picture: "" }])) });

// ---- screenshots: `?mock&act=` ----
const act = new URLSearchParams(location.search).get("act");
if (act) {
  setTimeout(async () => {
    for (const step of act.split(";;")) {
      const [verb, ...rest] = step.split(":");
      const arg = rest.join(":");
      if (verb === "wait") await sleep(Number(arg) || 300);
      else if (verb === "click") document.querySelector(arg)?.click();
      else if (verb === "key") document.dispatchEvent(new KeyboardEvent("keydown", { key: arg, bubbles: true }));
      else if (verb === "type" || verb === "scroll") {
        const [sel, val] = arg.split("=");
        const node = document.querySelector(sel);
        if (!node) continue;
        if (verb === "type") {
          node.value = val;
          node.dispatchEvent(new Event("input", { bubbles: true }));
        } else node.scrollTop = val === "bottom" ? node.scrollHeight : Number(val);
      }
      await sleep(250);
    }
  }, 700);
}
