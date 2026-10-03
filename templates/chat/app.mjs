// The chat template's code (docs/chat-records.md, "Push"; docs/cloudflare-v1.md,
// decisions 9 and 40). It runs from the platform's release, like the page,
// on one trigger (fragment.json): each record an agent of the chat posts on
// `chat` (its replies: `rp:<turn>:<n>`, each whole, never a draft) starts
// `notify_reply`, which pushes the reply to the chat's people who are not
// looking at it. A person's page subscribes for its person (`fragment.push.
// register(<their identity>)`), and says in its presence whether the
// chat is on screen (`looking`).
import { DurableObject } from "cloudflare:workers";

// A push's body: the reply's first characters.
const BODY_CHARS = 120;
// The chat's people one reply is pushed to, at most (a step each: a chat
// is you and your agents, decision 8).
const PEOPLE_MAX = 32;

/// The reply's text as a notification shows it: one line, its first
/// characters; a reply with no words says what it carries.
function summary(body) {
  const text = typeof body.text === "string" ? body.text.replace(/\s+/g, " ").trim() : "";
  if (text) return [...text].length > BODY_CHARS ? `${[...text].slice(0, BODY_CHARS - 1).join("")}…` : text;
  const files = Array.isArray(body.attachments) ? body.attachments : [];
  if (files.some((f) => typeof f?.type === "string" && f.type.startsWith("audio/"))) return "Sent a voice memo";
  return files.length === 1 ? "Sent a file" : files.length ? `Sent ${files.length} files` : "Replied";
}

/// An agent's reply, as docs/chat-records.md shapes it: `{text, turn,
/// attachments?}` (any other `kind` on `chat` is a page's own).
function isReply(body) {
  return !!body && typeof body === "object" && !Array.isArray(body) && typeof body.turn === "string" && (body.kind === undefined || body.kind === "message");
}

export class App extends DurableObject {
  async notify_reply({ record } = {}, job) {
    // its trigger's runs alone (as the chat itself): a member who calls it pushes nothing
    if (job.via !== "channel") return { pushed: 0, why: "only the chat's trigger pushes" };
    if (!isReply(record?.body)) return { pushed: 0, why: "not a reply" };
    const members = await job.members();
    // the trigger takes only an agent's records; asked again of who it is now
    if (members.find((m) => m.principal === record.principal)?.kind !== "agent") return { pushed: 0, why: "not an agent's" };
    const looking = new Set((await job.presence()).filter((p) => p.data?.looking === true).map((p) => p.principal));
    const people = members.filter((m) => m.kind === "person" && !looking.has(m.principal)).slice(0, PEOPLE_MAX);
    if (!people.length) return { pushed: 0, why: "no one away to tell" };
    const name = (await job.people([record.principal]))?.[record.principal]?.name;
    const payload = {
      title: typeof name === "string" && name ? name.charAt(0).toUpperCase() + name.slice(1) : "Your agent",
      body: summary(record.body),
      tag: job.fragment,
      url: "./",
    };
    let pushed = 0;
    // bounded: at most PEOPLE_MAX, one step each
    for (const person of people) pushed += (await job.push(person.principal, payload)).queued;
    return { pushed, to: people.length };
  }
}
