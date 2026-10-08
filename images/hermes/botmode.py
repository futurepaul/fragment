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
#   chat as the teammate that sent it, a hand-off to the bot (`fragment ask
#   <bot> --chat <its chat> --id dm-<delivery>`, acting as the sender), so
#   the bot's answer is an ordinary turn of the bridge's in that chat, which
#   its owner sees; once that turn ends, its replies settle the delivery,
#   which wakes the sender.
#
# It writes nothing while the platform holds the computer (`FRAGMENT_HOLD`),
# so a save never copies a mailbox or a lease it is writing.
#
# This file is the image's, not the repo's tooling: it runs inside Hermes'
# own Python, whose session and mailbox code only it can call.
import json
import os
import subprocess
import sys
import threading
import time
from pathlib import Path

RUN = os.environ.get("FRAGMENT_RUN", "/var/lib/fragment-run")
BOTS = os.path.join(RUN, "bots.json")
HOLD = os.environ.get("FRAGMENT_HOLD", "/run/computer/hold")
FRAGMENT = os.environ.get("FRAGMENT_CLI", "/usr/local/bin/fragment")
TITLE = "Bot Chat"
EVERY_S = 1.0
# How long a teammate's answer is waited for (`fragment ask`'s longest
# wait), and the ask itself, a little longer: Hermes' sender waits 30
# minutes at most on its side too (tools/bot_mode_dm.py).
ANSWER_WAIT_S = 1800
ASK_TIMEOUT_S = ANSWER_WAIT_S + 120
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
    keys = ("agent", "owner", "profile", "home", "chat", "sessionKey")
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


def answer_of(out):
    """`fragment --json ask … --wait`'s answer: (replies' text, error), or why there is none."""
    data = json.loads(out.strip().splitlines()[-1])["data"]
    answer = data.get("answer")
    if answer is None:
        return None, f"no answer within {ANSWER_WAIT_S} s; it will be in {data.get('chat')}"
    text = "\n\n".join(r.get("text") or "" for r in answer.get("replies") or []).strip()
    if answer.get("outcome") != "idle" and answer.get("error"):
        return text, answer["error"]
    return text, None


def deliver(bot, sender, owner, claimed):
    """A teammate's message, posted into the bot's own chat as the teammate, and its answer settling it."""
    delivery = claimed["delivery_id"]
    t = time.time()
    status, reply, error, reason = "failed", "", "", ""
    try:
        if sender is None:
            error = "the sender is no agent of this computer"
        else:
            env = {**os.environ, "FRAGMENT_AS_AGENT": sender["agent"], "FRAGMENT_FOR": sender["owner"]}
            run = subprocess.run(
                [FRAGMENT, "--json", "ask", bot["agent"], claimed["message"], "--chat", bot["chat"], "--id", "dm-" + delivery[:32], "--wait", str(ANSWER_WAIT_S)],
                capture_output=True, text=True, env=env, timeout=ASK_TIMEOUT_S, stdin=subprocess.DEVNULL,
            )
            if run.returncode != 0:
                error = (run.stdout.strip() or run.stderr.strip())[-600:] or f"fragment ask exited {run.returncode}"
            else:
                text, failed = answer_of(run.stdout)
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
    until_unheld()
    try:
        complete_delivery(owner.home, delivery, status=status, reply=reply, error=error, reason=reason)
    except Exception as e:
        ev("settle_failed", agent=bot["agent"], delivery=delivery, error=f"{type(e).__name__}: {e}"[:300])
    ev("delivered", agent=bot["agent"], sender=sender["agent"] if sender else None, delivery=delivery, status=status, reason=reason or None, ms=int((time.time() - t) * 1000))
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
        ev("taken", agent=bot["agent"], sender=sender["agent"] if sender else None, delivery=claimed["delivery_id"])
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
