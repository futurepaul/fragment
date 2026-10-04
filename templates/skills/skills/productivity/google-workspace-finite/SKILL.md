---
name: google-workspace-finite
description: Gmail, Calendar, Drive, Contacts, Sheets, and Docs through the person's connected Google account, with the bundled helper (Python's standard library only). No OAuth setup, no keys on the computer.
version: 2.0.0
author: Nous Research (rewritten for fragment)
license: MIT
metadata:
  hermes:
    tags: [Google, Gmail, Calendar, Drive, Sheets, Docs, Contacts, Email]
    homepage: https://github.com/NousResearch/hermes-agent
---

# Google Workspace

Gmail, Calendar, Drive, Contacts, Sheets, and Docs through your owner's
connected Google account. Your computer holds no Google credential:
`GOOGLE_OAUTH_ACCESS_TOKEN` holds the connection's placeholder (it names
you), the helper sends it as a bearer token to Google's own hosts, and the
computer swaps in a short-lived token for your owner's account. There is
no OAuth setup to run, no client secret and no token file: never ask the
person for one, and never create a Google Cloud project for them.

## Connected?

```bash
GAPI="python3 ${HERMES_SKILL_DIR}/scripts/google_api.py"
$GAPI check
```

- `{"status": "connected", "email": …}`: go on.
- `not_connected`: your owner has not connected Google, or must authorize
  it again (`GOOGLE_OAUTH_ACCESS_TOKEN` is then unset). Ask them to connect
  it in the shell's Settings, under Connections, then check again: your
  computer is given it within seconds.
- `forbidden`: your owner narrowed your connections and left Google out, or
  Google refused the scope. Say which, and stop.

The deployment's Google connection (`google`) is swapped for the hosts
`gmail.googleapis.com`, `www.googleapis.com`, `people.googleapis.com`,
`sheets.googleapis.com` and `docs.googleapis.com` only. The scopes it grants
are in `references/google-workspace-scopes.json`.

## References

- `references/gmail-search-syntax.md`: Gmail search operators (is:unread, from:, newer_than:, etc.)
- `references/google-workspace-scopes.json`: the scopes the connection grants

The helper exposes Docs read only: do not imply that it can write a Doc or
use Apps Script.

## Login emails

When the human explicitly asks you to retrieve or use a login code for the
current task, you may read the connected mailbox and use the newest
matching message. Verify the connected address, service/sender, and
freshness; do not echo the token. Ask the human only when mailbox access
fails or the match is ambiguous.

## Usage

### Gmail

```bash
# Search (returns JSON array with id, from, subject, date, snippet)
$GAPI gmail search "is:unread" --max 10
$GAPI gmail search "from:boss@company.com newer_than:1d"
$GAPI gmail search "has:attachment filename:pdf newer_than:7d"

# Read full message (returns JSON with body text)
$GAPI gmail get MESSAGE_ID

# Send
$GAPI gmail send --to user@example.com --subject "Hello" --body "Message text"
$GAPI gmail send --to user@example.com --subject "Report" --body "<h1>Q4</h1><p>Details...</p>" --html

# Reply (automatically threads and sets In-Reply-To)
$GAPI gmail reply MESSAGE_ID --body "Thanks, that works for me."

# Labels
$GAPI gmail labels
$GAPI gmail modify MESSAGE_ID --add-labels LABEL_ID
$GAPI gmail modify MESSAGE_ID --remove-labels UNREAD
```

### Calendar

```bash
# List events (defaults to next 7 days)
$GAPI calendar list
$GAPI calendar list --start 2026-03-01T00:00:00Z --end 2026-03-07T23:59:59Z

# Create event (ISO 8601 with timezone required)
$GAPI calendar create --summary "Team Standup" --start 2026-03-01T10:00:00-06:00 --end 2026-03-01T10:30:00-06:00
$GAPI calendar create --summary "Lunch" --start 2026-03-01T12:00:00Z --end 2026-03-01T13:00:00Z --location "Cafe"
$GAPI calendar create --summary "Review" --start 2026-03-01T14:00:00Z --end 2026-03-01T15:00:00Z --attendees "alice@co.com,bob@co.com"

# Delete event
$GAPI calendar delete EVENT_ID
```

### Drive

```bash
$GAPI drive search "quarterly report" --max 10
$GAPI drive search "mimeType='application/pdf'" --raw-query --max 5
```

### Contacts

```bash
$GAPI contacts list --max 20
```

### Sheets

```bash
# Read
$GAPI sheets get SHEET_ID "Sheet1!A1:D10"

# Write
$GAPI sheets update SHEET_ID "Sheet1!A1:B2" --values '[["Name","Score"],["Alice","95"]]'

# Append rows
$GAPI sheets append SHEET_ID "Sheet1!A:C" --values '[["new","row","data"]]'
```

### Docs

```bash
$GAPI docs get DOC_ID
```

### Anything else

Any other Google REST call, or Google's own client libraries given the
variable as their access token, works the same way: send it as a bearer
token, and the computer swaps the token in.

```bash
curl -s "https://www.googleapis.com/calendar/v3/users/me/calendarList" \
  -H "Authorization: Bearer $GOOGLE_OAUTH_ACCESS_TOKEN"
```

## Output Format

All commands return JSON. Parse with `jq` or read directly. Key fields:

- **Gmail search**: `[{id, threadId, from, to, subject, date, snippet, labels}]`
- **Gmail get**: `{id, threadId, from, to, subject, date, labels, body}`
- **Gmail send/reply**: `{status: "sent", id, threadId}`
- **Calendar list**: `[{id, summary, start, end, location, description, htmlLink}]`
- **Calendar create**: `{status: "created", id, summary, htmlLink}`
- **Drive search**: `[{id, name, mimeType, modifiedTime, webViewLink}]`
- **Contacts list**: `[{name, emails: [...], phones: [...]}]`
- **Sheets get**: `[[cell, cell, ...], ...]`

A failure is one JSON line on stderr, `{error, status, message, hint}`, and
exit 1.

## Rules

1. **Never send email, share files, accept invites, or create/delete events
   without confirming with the user first.** Show the draft content and ask
   for approval.
2. **Check the connection before first use** (`$GAPI check`).
3. **Use the Gmail search syntax reference** for complex queries: load it with `skill_view("google-workspace-finite", file_path="references/gmail-search-syntax.md")`.
4. **Calendar times must include timezone**: always ISO 8601 with offset (e.g., `2026-03-01T10:00:00-06:00`) or UTC (`Z`).
5. **Respect rate limits**: avoid rapid-fire sequential API calls. Batch reads when possible.

## Troubleshooting

| Problem | Fix |
|---------|-----|
| `not_connected` | Your owner connects Google in Settings → Connections |
| `forbidden` | Your owner left Google off this agent's connections, or the scope is missing |
| `HttpError 403: Access Not Configured` | That API is not enabled for the deployment's Google client: tell the person, and stop |
