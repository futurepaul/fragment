# The Hermes image's Bot Mode keeper (images/hermes/boot/src/bots.rs;
# docs/computers.md, "Bot Mode in our Hermes image"), started by hermes-boot
# as the hermes user, in Hermes' own Python, for the computer's life.
#
# A bot's `message_agent` reaches its teammate's Bot Chat (its own chat with
# its owner, whose session the image's hook titles "Bot Chat") through
# Hermes' own path: a turn of Hermes' own outside the gateway, so outside
# the bridge, unless that Bot Chat has a live owner (v0.21.6:
# tools/bot_mode_dm.py, tools/bot_live_delivery.py; Hermes Desktop is one).
# The keeper is that owner, for each bot in the boot's bots file, about
# once a second:
#
# - it holds the bot's Bot Chat session (the compression tip of the session
#   titled "Bot Chat", when that session is its own chat's) as its live
#   owner: an active-session lease marked `bot_live_delivery_consumer`,
#   so a teammate's message is queued in the bot's mailbox;
# - it takes each message queued there and posts it into the bot's own
#   chat as the teammate that sent it, a hand-off to the bot (its `to` names
#   the bot; acting as the sender for their owner, as `fragment ask` does,
#   but adding the sender to no chat), so the bot's answer is an ordinary
#   turn of the bridge's in that chat, which its owner sees; once that turn
#   ends, its replies settle the delivery, which wakes the sender.
#
# A bot answers a teammate once. Hermes' roster tells a bot that a teammate
# messaged to answer with `message_agent`, and its sender is woken by the
# delivery's answer as well, so each said it twice (the sender relaying the
# answer again, the teammate acknowledging that). So the posted message
# ends with a line telling the bot its reply there goes back on its own;
# and when it messages its sender back anyway while answering it, both
# deliveries are settled with their answer under a line telling the woken
# bot it has had it already: to end its turn with Hermes' `[SILENT]`
# (gateway/response_filters.py), which its gateway says nothing for.
#
# It writes nothing while the platform holds the computer (`FRAGMENT_HOLD`),
# so a save never copies a mailbox or a lease it is writing.
#
# This file is the image's, not the repo's tooling: it runs inside Hermes'
# own Python, whose session and mailbox code only it can call.
import json
import os
import re
import sys
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

RUN = os.environ.get("FRAGMENT_RUN", "/var/lib/fragment-run")
BOTS = os.path.join(RUN, "bots.json")
HOLD = os.environ.get("FRAGMENT_HOLD", "/run/computer/hold")
# The platform's API, as the computer reaches it (docs/computers.md, "The
# fragment API"): a request names the agent it acts as, and the computer
# signs it as that agent.
API = os.environ.get("FRAGMENT_API", "http://api.fragment.internal").rstrip("/")
TITLE = "Bot Chat"
EVERY_S = 1.0
# How long a teammate's answer is waited for (`fragment ask`'s longest
# wait): Hermes' sender waits 30 minutes at most on its side too
# (tools/bot_mode_dm.py). It is looked for this often.
ANSWER_WAIT_S = 1800
POLL_S = 2.0
# One request to the platform, at most this long.
CALL_S = 30
# Pages of a channel one look reads, at most (1000 records each), and how
# far back a message posted before (a replay of the same post) has its
# turn looked for (`fragment ask`'s REPLAY_BACK_RECORDS).
PAGES_MAX = 10
PAGE_RECORDS = 1000
REPLAY_BACK = 2000
# A bot that messages its sender back while answering it does so in its
# turn, which has ended by the time its own answer is read: its message is
# taken within the keeper's next pass, so an answer waits this long for it.
REPLIED_WAIT_S = 3.0
# The line a teammate's message ends with, in the chat it is posted to.
ANSWER_HERE = "\n\n(Your reply here goes back to {name} on its own: answer here, and don't message_agent {name} about this.)"
# What a woken bot is told above an answer it has had already.
HAD_IT = {
    "answered": "{name} also messaged you this answer directly, in your own chat, and you passed it on there. If it adds nothing for your person, end this turn with exactly [SILENT].",
    "reply": "{name} read your message in their own chat and answered there. If this asks nothing more of you, end this turn with exactly [SILENT].",
}
# Hermes' attribution of a teammate's message (tools/bot_mode_dm.py): the
# sender's friendly name.
ATTRIBUTION = re.compile(r"Message from \S+ (.+?) \(@[^)]+\): ")
# Messages being delivered at once, per bot at most: past it the rest wait
# in its mailbox (the bridge runs one of the bot's turns in a chat at a
# time anyway).
IN_FLIGHT_MAX = 4
# What a delivery's answer carries back, in characters at most (Hermes'
# message bound, tools/bot_mode_dm.py's MESSAGE_MAX_CHARS).
REPLY_MAX_CHARS = 16000

from hermes_state import SessionDB  # noqa: E402
from hermes_cli.active_sessions import release_active_session, try_acquire_active_session  # noqa: E402
from tools.bot_failure_reasons import classify_agent_error  # noqa: E402
from tools.bot_live_delivery import claim_pending_delivery, complete_delivery, find_canonical_live_owner, has_mailbox  # noqa: E402


def ev(event, **fields):
    """One JSON line on stderr, as the boot's and the bridge's events: ids, never text."""
    sys.stderr.write(json.dumps({"at": int(time.time() * 1000), "event": "botmode." + event, **fields}) + "\n")
    sys.stderr.flush()


def held():
    return os.path.exists(HOLD)


def until_unheld():
    # bounded by the platform's hold, which it lifts after its save
    while held():
        time.sleep(0.2)


def read_bots():
    try:
        with open(BOTS, encoding="utf-8") as f:
            bots = json.load(f)["bots"]
    except (OSError, ValueError, KeyError, TypeError):
        return []
    keys = ("agent", "identity", "owner", "profile", "home", "chat", "sessionKey")
    return [b for b in bots if isinstance(b, dict) and all(isinstance(b.get(k), str) and b[k] for k in keys)]


def bot_chat(bot):
    """The bot's Bot Chat session as Hermes finds its owner (the compression tip of the session
    titled "Bot Chat"), when that session is its own chat's; None before its chat's first turn."""
    path = Path(bot["home"]) / "state.db"
    if not path.is_file():
        return None
    db = SessionDB(db_path=path, read_only=True)
    try:
        titled = db.get_session_by_title(TITLE)
        if titled is None or titled.get("session_key") != bot["sessionKey"]:
            return None
        return db.get_compression_tip(titled["id"]) or titled["id"]
    finally:
        db.close()


class Owner:
    """The keeper's live ownership of one bot's Bot Chat session."""

    def __init__(self, bot):
        self.home = str(Path(bot["home"]).resolve())
        self.live = "fragment-bot-mode:" + bot["profile"]
        self.session = None
        self.lease = None
        self.in_flight = 0
        self.lock = threading.Lock()

    def pin(self):
        """The owner a delivery for this bot is pinned to (tools/bot_live_delivery.py)."""
        return {"profile_home": self.home, "session_id": self.session, "lease_id": self.lease.lease_id, "live_session_id": self.live}

    def hold(self, bot, session):
        """Holds `session` as the bot's Bot Chat's live owner: whether it does."""
        if self.lease is not None and self.session == session:
            # still the canonical owner Hermes finds (a lease pruned or taken is taken again)
            found = find_canonical_live_owner(self.home)
            if found is not None and found.get("lease_id") == self.lease.lease_id:
                return True
        if held():
            return False
        if self.lease is not None:
            release_active_session(self.lease)
            self.lease = None
        lease, refusal = try_acquire_active_session(
            session_id=session, surface="fragment-bot-mode", config=None,
            metadata={"live_session_id": self.live, "bot_live_delivery_consumer": True},
            registry_home=self.home,
        )
        if lease is None:
            ev("own_refused", agent=bot["agent"], session=session, error=str(refusal)[:300])
            return False
        self.session, self.lease = session, lease
        ev("owned", agent=bot["agent"], session=session)
        return True


class Refused(Exception):
    """The platform answered a request, and refused it."""


def call(method, path, sender, body=None):
    """One request to the platform as `sender` (an agent of this computer), acting for its owner."""
    url = f"{API}{path}{'&' if '?' in path else '?'}for={urllib.parse.quote(sender['owner'], safe='')}"
    data = None if body is None else json.dumps(body).encode()
    headers = {"x-fragment-agent": sender["agent"]} | ({"content-type": "application/json"} if data is not None else {})
    try:
        with urllib.request.urlopen(urllib.request.Request(url, data=data, method=method, headers=headers), timeout=CALL_S) as r:
            return json.loads(r.read() or b"null")
    except urllib.error.HTTPError as e:
        raise Refused(f"{method} {path.split('?')[0]}: {e.code} {e.read()[:300].decode(errors='replace')}") from None


def bodies_after(chat, channel, after, sender):
    """The records of `channel` after `after` (at most PAGES_MAX pages), and the cursor after them."""
    out, cursor = [], after
    # bounded: PAGES_MAX pages
    for _ in range(PAGES_MAX):
        page = call("GET", f"/api/f/{chat}/channels/{channel}?after={cursor}", sender)
        records = page.get("records") or []
        out += [r for r in records if isinstance(r, dict) and isinstance(r.get("body"), dict)]
        following = page.get("next", cursor)
        if following <= cursor or len(records) < PAGE_RECORDS:
            return out, max(cursor, following)
        cursor = following
    return out, cursor


def hand_off(bot, sender, message, post):
    """`message` posted into the bot's own chat as `sender` (id `post`), naming the bot, as `fragment
    ask` posts a question (cli/src/ask.rs), but adding no one to the chat; then, once the bot's turn
    of it ends, its replies' text and its error, or (None, why) when it has not ended in ANSWER_WAIT_S."""
    chat, me = bot["chat"], bot["identity"]
    listed = call("GET", f"/api/f/{chat}/channels", sender)["channels"]
    work_seq = next((c.get("seq") or 0 for c in listed if c.get("name") == "work"), 0)
    posted = call("POST", f"/api/f/{chat}/channels/chat", sender, {"id": post, "body": {"text": message, "to": [me]}})
    seq = posted["record"]["seq"]
    # a replay's turn may have run already: read `work` back for it
    cursor = max(0, work_seq - REPLAY_BACK) if posted.get("replayed") else work_seq
    turn, seen, deadline = None, [], time.monotonic() + ANSWER_WAIT_S
    # bounded by the deadline: one look every POLL_S
    while True:
        more, cursor = bodies_after(chat, "work", cursor, sender)
        seen += [r["body"] for r in more]
        if turn is None:
            turn = next((b.get("turn") for b in seen if b.get("kind") == "turn.start" and b.get("agent") == me and (b.get("cause") or {}).get("channel") == "chat" and (b.get("cause") or {}).get("seq") == seq), None)
        if turn is not None:
            end = next((b for b in seen if b.get("kind") == "turn.end" and b.get("turn") == turn), None)
            if end is not None:
                said, _ = bodies_after(chat, "chat", seq, sender)
                text = "\n\n".join(r["body"].get("text") or "" for r in said if r.get("principal") == me and r["body"].get("turn") == turn and r["body"].get("kind") in (None, "message")).strip()
                return text, (end.get("error") or f"its turn ended {end.get('outcome')}") if end.get("outcome") != "idle" else None
            # only its own records matter from here
            seen = [b for b in seen if b.get("turn") == turn]
        else:
            # the start may be in a later page; a record of no turn of its is none of ours
            seen = [b for b in seen if b.get("agent") == me]
        if time.monotonic() >= deadline:
            return None, f"no answer within {ANSWER_WAIT_S} s; it will be in {chat}"
        time.sleep(POLL_S)


class Exchanges:
    """The deliveries being answered now, so a bot's message to its sender while it answers them is
    known for an answer of its: delivery id -> {sender, recipient, answered, reply_to}."""

    def __init__(self):
        self.lock = threading.Lock()
        self.by_id = {}

    def open(self, delivery, sender, recipient):
        """A delivery from `sender` to `recipient` taken: whether it answers one of `recipient`'s to
        `sender` being answered now (then that one is marked as answered directly)."""
        with self.lock:
            answering = next((d for d, x in self.by_id.items() if x["sender"] == recipient and x["recipient"] == sender), None)
            if answering is not None:
                self.by_id[answering]["answered"] = True
            self.by_id[delivery] = {"sender": sender, "recipient": recipient, "answered": False, "reply_to": answering}
            return answering is not None

    def close(self, delivery):
        """The delivery's state, let go."""
        with self.lock:
            return self.by_id.pop(delivery, None) or {}


EXCHANGES = Exchanges()


def name_of(message, fallback):
    """The sender's name as Hermes attributes its message, or `fallback`."""
    found = ATTRIBUTION.match(message or "")
    return found.group(1) if found else fallback


def bot_name(bot):
    """The bot's name as its profile says it (its Bot Mode title: bots.rs), else its label."""
    try:
        import hermes_yaml

        data = hermes_yaml.safe_load((Path(bot["home"]) / "profile.yaml").read_text(encoding="utf-8")) or {}
        title = ((data.get("ui_meta") or {}).get("hermes-bots") or {}).get("title") or data.get("display_name")
        return str(title).strip() or bot["agent"].split(".")[0]
    except Exception:
        return bot["agent"].split(".")[0]


def deliver(bot, sender, owner, claimed):
    """A teammate's message, posted into the bot's own chat as the teammate, and its answer settling it."""
    delivery = claimed["delivery_id"]
    t = time.time()
    status, reply, error, reason, had = "failed", "", "", "", None
    try:
        if sender is None:
            error = "the sender is no agent of this computer"
        else:
            them = name_of(claimed["message"], sender["agent"].split(".")[0])
            text, failed = hand_off(bot, sender, claimed["message"] + ANSWER_HERE.format(name=them), "dm-" + delivery[:32])
            if failed is None or text:
                status, reply = "settled", (text or "")[:REPLY_MAX_CHARS]
                if failed:
                    reply = f"{reply}\n\n({failed})".strip()
            else:
                error = failed
    except Exception as e:  # the delivery is settled whatever went wrong
        error = f"{type(e).__name__}: {e}"[:600]
    if status == "failed":
        reason = classify_agent_error(error)
    else:
        # a message back from the bot to its sender, sent in the turn just
        # read, is taken within the keeper's next pass
        time.sleep(REPLIED_WAIT_S)
    exchange = EXCHANGES.close(delivery)
    if status == "settled":
        had = "reply" if exchange.get("reply_to") else "answered" if exchange.get("answered") else None
        if had is not None:
            reply = HAD_IT[had].format(name=bot_name(bot)) + "\n\n" + reply
    until_unheld()
    try:
        complete_delivery(owner.home, delivery, status=status, reply=reply, error=error, reason=reason)
    except Exception as e:
        ev("settle_failed", agent=bot["agent"], delivery=delivery, error=f"{type(e).__name__}: {e}"[:300])
    ev("delivered", agent=bot["agent"], sender=sender["agent"] if sender else None, delivery=delivery, status=status, reason=reason or None, had=had, ms=int((time.time() - t) * 1000))
    with owner.lock:
        owner.in_flight -= 1


def take(bot, bots, owner):
    """Each message queued for the bot, taken and delivered, up to IN_FLIGHT_MAX at once."""
    if not has_mailbox(owner.home):
        return
    # bounded: IN_FLIGHT_MAX deliveries in flight
    while True:
        with owner.lock:
            if owner.in_flight >= IN_FLIGHT_MAX:
                return
        if held():
            return
        claimed = claim_pending_delivery(owner.home, owner.pin())
        if claimed is None:
            return
        author = str((claimed.get("author") or {}).get("id") or "")
        sender = next((b for b in bots if "bot:" + b["profile"] == author), None)
        replying = EXCHANGES.open(claimed["delivery_id"], sender["agent"] if sender else None, bot["agent"])
        ev("taken", agent=bot["agent"], sender=sender["agent"] if sender else None, delivery=claimed["delivery_id"], replying=replying)
        with owner.lock:
            owner.in_flight += 1
        threading.Thread(target=deliver, args=(bot, sender, owner, claimed), name="deliver-" + claimed["delivery_id"][:12], daemon=True).start()


def main():
    ev("started", pid=os.getpid())
    owners = {}
    failing = {}
    # bounded by the computer's life: one pass per EVERY_S
    while True:
        if not held():
            bots = read_bots()
            # an agent no longer on this computer: its Bot Chat let go
            for gone in [a for a in owners if not any(b["agent"] == a for b in bots)]:
                owner = owners.pop(gone)
                if owner.lease is not None:
                    release_active_session(owner.lease)
                ev("released", agent=gone)
            for bot in bots:
                try:
                    session = bot_chat(bot)
                    if session is not None:
                        owner = owners.setdefault(bot["agent"], Owner(bot))
                        if owner.hold(bot, session):
                            take(bot, bots, owner)
                    failing.pop(bot["agent"], None)
                except Exception as e:  # one bot's trouble never stops another's
                    # said once per kind of failure, not once a second
                    said = f"{type(e).__name__}: {e}"[:300]
                    if failing.get(bot["agent"]) != said:
                        failing[bot["agent"]] = said
                        ev("failed", agent=bot["agent"], error=said)
        time.sleep(EVERY_S)


if __name__ == "__main__":
    main()
