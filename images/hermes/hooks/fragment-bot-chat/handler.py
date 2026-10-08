# The Hermes image's Bot Chat hook (images/hermes/boot/src/bots.rs;
# docs/computers.md, "Bot Mode in our Hermes image"): a gateway event hook
# (Hermes' gateway/hooks.py), linked into each agent's profile's `hooks/` by
# hermes-boot, which Hermes' gateway runs in that profile's scope as each of
# its turns starts (`agent:start`), before the turn's agent is built.
#
# Hermes gives a session its teammate roster and its `message_agent` tool
# only when the session is titled exactly "Bot Chat" (v0.21.6:
# tools/bot_mode_probe.py, tools/bot_mode_dm.py), and builds a session's
# system prompt once, at its first turn: its gateway keeps the turn's agent,
# that prompt with it, for the session's next turns. So an agent's own chat
# (bots.rs, `bot_chat`; the boot's bots file says which it is) is titled
# here, before its first turn's prompt is built, through Hermes' session
# API, as a person's `/title` would title it: its row recorded with its
# gateway identity first when the turn has not written it yet, and any other
# session of the agent's that holds the title made to give it up (Hermes'
# own `chat -c "Bot Chat" --create-if-missing` makes one when a teammate
# messages an agent with no chat yet; its history stays). A turn in any
# other chat, or of the gateway's own profile, is left alone.
#
# This file is the image's, not the repo's tooling: it runs inside Hermes'
# gateway, whose session code only it can call.
import json
import os
from pathlib import Path

BOTS = os.path.join(os.environ.get("FRAGMENT_RUN", "/var/lib/fragment-run"), "bots.json")
TITLE = "Bot Chat"


def bot_of(home):
    """The bot whose profile's home is `home`, from the boot's bots file, or None."""
    try:
        with open(BOTS, encoding="utf-8") as f:
            bots = json.load(f)["bots"]
    except (OSError, ValueError, KeyError, TypeError):
        return None
    return next((b for b in bots if isinstance(b, dict) and str(Path(str(b.get("home"))).resolve()) == home), None)


def handle(event_type, context):
    if context.get("platform") != "relay" or not context.get("session_id"):
        return
    from hermes_constants import get_hermes_home
    from hermes_state import SessionDB

    home = str(get_hermes_home().resolve())
    bot = bot_of(home)
    if bot is None or context.get("chat_id") != f"{bot['chat']}/{bot['agent']}":
        return
    session = context["session_id"]
    db = SessionDB(db_path=Path(home) / "state.db")
    try:
        if (db.get_session_title(session) or "") == TITLE:
            return
        if db.get_session(session) is None:
            # the turn's own row comes once its prompt is built: written now,
            # with the identity the gateway gives it, so the title has a row
            db.record_gateway_session_peer(
                session, source="relay", session_key=bot["sessionKey"], chat_id=context["chat_id"],
                chat_type=context.get("chat_type") or "group", user_id=context.get("user_id") or None,
            )
        holder = db.get_session_by_title(TITLE)
        if holder is not None and holder["id"] != session:
            db.set_session_title(holder["id"], "")
        db.set_session_title(session, TITLE)
        print(json.dumps({"event": "botmode.titled", "agent": bot["agent"], "session": session, "freed": holder["id"] if holder else None}), flush=True)
    finally:
        db.close()
