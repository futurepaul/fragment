# Hermes' Relay

What Hermes' Relay is, as read from its code, for the bridge in our
Hermes image (decision 21; docs/bridge.md is how the bridge speaks it).
The platform carries no Hermes code (docs/cloudflare-v1.md, "The rule");
the connector the cell had until the cut is at tag `celld-final`
(`cell/src/relay.rs`).

Pinned to Hermes v0.21.6 (tag `v0.21.6`, commit `818c13be1dc4`,
2026-10-08; images/hermes/Dockerfile names its image), contract version
1, read from `gateway/relay/` (its code wins over its contract
document). Read first from v0.21.5 (tag `v2026.9.24`); v0.21.6 only drops
its plugin-compat names and takes its prompts' labels from translations
(English as before): the wire is unchanged. Relay is marked
experimental: it may change without a deprecation cycle, so a Hermes
upgrade re-reads it, and the real-Hermes lane
(`images/bridge/tests/docker.rs`) runs the real image against it.

## The dial

- Hermes is told `GATEWAY_RELAY_URL` (it appends `/relay`, `http` →
  `ws`), `GATEWAY_RELAY_ID` and `GATEWAY_RELAY_SECRET`. With the id and
  secret pinned it provisions nothing (`self_provision_relay`).
- Its token: `Authorization: Bearer base64url_unpadded("{id}:{exp}:{hex(HMAC-SHA256(key = secret's UTF-8 bytes, msg = "{id}:{exp}"))}")`,
  `exp` five minutes on, fresh each dial (`gateway/relay/auth.py`). A
  refusal is close 4401: reason `expired` (it dials again) or
  `unauthorized` (after a handshake once, it re-dials once with a fresh
  token, then stops for good).
- Frames are JSON objects, one per line, each ending with `\n`.
  Keepalive is WebSocket pings, from Hermes (30 s, 60 s timeout). It
  reconnects with backoff from 1 s to 30 s.
- The same base URL serves its media plane: `POST /relay/media` (the
  bytes, `content-type`, `x-media-filename`) → `{id}`, and
  `GET /relay/media/<id>`, both behind the same token. At most 25 MiB.

## Frames

- Hermes: `hello {platform, botId}` (one per fronted identity) → the
  connector: `descriptor` (the nine keys Hermes requires, and
  `supported_ops`; draft streaming needs both
  `supports_draft_streaming: true` and `draft` in the ops).
- The connector: `inbound {event, bufferId}` → Hermes: `inbound_ack
  {bufferId}` once its handler has taken the event. The event:
  `{text, message_id, source: {platform, chat_id, chat_type, chat_name,
  user_id, user_name, profile, message_id}, media_urls?, media?,
  prompt_response?, context?}`. `source.profile` routes it to a profile of the
  multiplexed gateway. Hermes drops an event it saw by
  `(platform, chat_id, message_id)`, so a replay is harmless. `context`
  (`[{text, source?}]`, read-only channel context: "it never triggers the
  agent") is rendered before the message as `[Recent channel
  messages]\n…\n\n[New message]\n[name] text`
  (`gateway/relay/ws_transport.py`, `_render_relay_context`); the bridge
  sends a turn's note on a cut turn there (docs/bridge.md). No field of an
  event changes how a session's unanswered tail is read: two user
  messages in a row are joined (`_merge_consecutive_users`).
- A quoted reply: `reply_to_message_id` and `reply_to: {text, author?,
  is_own}` (`_event_from_wire`); with both an id and a text, Hermes puts
  `[Replying to: "<text>"]` before the message (`[Replying to your
  previous message: …]` when `is_own`; `gateway/run_inbound.py`,
  `_prepend_inbound_reply_context`), the text whole. `author` is not
  shown.
- A message whose text starts with `/` is a command to Hermes
  (`MessageEvent.is_command`), any of its registry's (hermes_cli/
  commands.py): idle, run as a turn is, bracketed `👀` … `✅`, its answer
  a `send` answering the message; while the chat's session is busy,
  dispatched at once and never queued (`_handle_message_while_active`:
  every command it resolves bypasses; `/stop`, `/new`, `/reset` interrupt
  the turn first), its answer a `send` answering the command, with no
  bracket. No slash access is set (`allow_admin_from`), so every user of
  the chat may run every one: the connector is the gate (the bridge's
  menu, `relay/menu.rs`). `/steer` mid-run lands after the turn's next
  tool call (`_busy_steer_command`), idle it is a message
  (`_hm_cmd_steer`); `/btw` answers from a snapshot of the session in an
  auxiliary call, its answer a later `send` answering nothing (`💬 /btw:
  "<question>"`, `gateway.btw.answer`); `/new` gives the session key a new
  session (`_handle_reset_command`), asking nothing under
  `approvals.destructive_slash_confirm: false`. The manifest
  (`gateway/relay/command_manifest.py`, 28 commands with descriptions)
  rides only a Discord `hello` (`command_manifest`), so a Relay connector
  holds its own.
- Hermes: `outbound {requestId, action}` → the connector:
  `outbound_result {requestId, result}` (Hermes waits 30 s). The ops in
  v0.21.5, each sent only when advertised:
  - `send {chat_id, content, reply_to, metadata}`: a reply when it
    answers a message (`reply_to`, or `metadata.reply_to_message_id`, or
    a final's `metadata.notify`), else tool progress or a notice;
  - `edit {message_id, content}`: the whole text again;
  - `delete`, `typing`, `react {message_id, emoji, remove}`;
  - `draft {draft_id, content, final}`: a reply streaming; for any
    platform but Slack the final is a separate `send`. Its stream
    consumer ends a draft segment at every tool boundary with that
    `send` too (`gateway/stream_consumer.py`, `_send_or_edit` with
    `finalize`), so the model's text before a tool call arrives as a
    reply like any other, whatever `display.interim_assistant_messages`
    says; the bridge takes it back as the step's words (docs/bridge.md);
  - `prompt {prompt_kind, prompt_id, content, options: [{id, label,
    style?}], timeout_s?}`: buttons; exec approvals offer `once`,
    `session`, `always`, `deny` (fewer after a smart deny);
  - `send_media {media_kind, source_url, content, filename?}`;
  - `get_chat_info`, `thread_create`, `thread_rename`;
  - `task_card`, `task_card_stop`: only for Slack chats
    (`gateway/run_turn.py`), so Relay's own steps are progress text;
  - `follow_up`: Discord's interaction tokens.
- The connector: `interrupt_inbound {session_key, chat_id}`: Stop, for
  `agent:<profile>:relay:group:<chat_id>` (never a `/stop` message: one
  while the session is busy stalls Hermes' reader).
- A prompt's answer is an `inbound` whose event carries `prompt_response:
  {prompt_id, option_id}`: Hermes resolves it at once, mid-turn, and
  never dispatches it as chat; it confirms with a send answering nothing.
  The first answer wins; prompt ids are its own (`<nonce>.<hex>`).
- Hermes: `going_idle` → the connector: `going_idle_ack`.
- `react`: `👀` on the message as processing starts, off as it ends,
  then `✅` or `❌` (a cancelled turn: neither): `RelayAdapter`'s
  `on_processing_start` and `on_processing_complete`, with its
  `_ACK_EMOJI_DEFAULT` (only Telegram gets others). Hermes has no other
  end-of-turn signal. A message that reaches it while its gateway starts
  (its adapters connect before `_startup_restore_in_progress` clears) is
  bracketed twice: once, empty, then `✅`, as `_handle_message` queues it
  behind the startup restore, then the turn's own as
  `_drain_startup_restore_queue` runs it (`gateway/run_inbound.py`,
  `gateway/run_startup.py`). In the real image that is a boot's first
  turn, 1.3 s apart; every later message is bracketed once. A person's
  turn always says something before its `✅`: an empty answer becomes
  `EMPTY_RESPONSE_EXPLANATION` and a bare silence marker
  `_UNEXPECTED_SILENCE_REPLY` (`gateway/run_turn.py`); a handler that
  raises sends `❌`, then its notice (`_notify_turn_error`).

What Relay does not carry (v0.21.5, and upstream's main on 2026-10-06):
a turn's start or end as a frame of its own (the gateway's frames are
`hello`, `outbound`, `inbound_ack`, `going_idle`, and an `interrupt`
nothing sends); a question to answer
in words as anything but text (an open `clarify` is the base adapter's
`❓ <question>`; one with choices is a `prompt` of `prompt_kind:
"clarify"`, whose "Other" is answered `✏️ Type your answer:`; main
translates both, keeping the glyphs); a tool step as anything but
progress text (`task_card` is Slack's alone). Hermes' supported hooks
(`gateway/hooks.py`: `agent:start`, `agent:step`, `agent:end`; plugins'
`pre_tool_call`, `post_tool_call`) have all three, but outside Relay:
the bridge would need a channel of its own beside it.

## Hermes' settings (images/hermes)

`hermes-boot` writes Hermes' managed overlay (`/etc/hermes/config.yaml`,
merged over each profile's config) and each profile's model block:

```yaml
group_sessions_per_user: false     # one session per chat, "[name] …" each message
gateway: {multiplex_profiles: true}
onboarding: {profile_build: "off"}
streaming: {enabled: true, transport: "draft"}
display: {busy_input_mode: "queue", tool_progress: "all", tool_progress_grouping: "accumulate", tool_preview_length: 140, long_running_notifications: false, interim_assistant_messages: false}
platforms: {relay: {gateway_restart_notification: false}}
approvals: {mode: "smart", timeout: 3600, destructive_slash_confirm: false}
agent: {disabled_toolsets: ["cronjob"]}   # its routines are fragment cron (decision 38)
plugins: {disabled: [platforms/…, dashboard_auth/…]}
```

and in the gateway's environment `GATEWAY_MULTIPLEX_PROFILES=true`
(left to its default, Hermes stays standalone in an s6 container with no
per-profile gateway slots), `RELAY_HOME_CHANNEL=none` (otherwise it asks
each new chat to become its home), `HERMES_GATEWAY_BUSY_INPUT_MODE=queue`
and `HERMES_GATEWAY_NO_SUPERVISE=1`.

Hermes reads `display` for a turn from the profile's own `config.yaml`
with the overlay merged over it (`_load_gateway_config` under the
profile's scope, `gateway/run_turn.py`), so the overlay's
`display.interim_assistant_messages: false` holds for `relay`, which
has no tier of its own in `_PLATFORM_DEFAULTS` and would otherwise get
the global `true`: a model's completed text beside a tool call is never
sent again as a mid-turn message (`interim_assistant_callback` is unset).
`display.show_commentary` (Codex models' commentary channel) rides on
that callback, so it is off with it. `browser` is the exception: Hermes
reads it from the profile's file alone (`read_raw_config`).
