---
name: monday-com-finite
description: Use when the user wants to inspect or update Monday.com boards, items, updates, or workspaces through their connected Monday account. Only rely on it when Monday is connected in the shell's Settings.
---

# Monday.com via your owner's connection

Use Monday's GraphQL API (`https://api.monday.com/v2`) through your owner's
connected Monday account. Your computer holds no keys: send the
connection's placeholder as the token and name yourself, and the computer
swaps in a short-lived token:

```bash
monday() {
  curl -sS -X POST "https://api.monday.com/v2" \
    -H "Authorization: fragment-connection:monday" \
    -H "x-fragment-agent: $FRAGMENT_AS_AGENT" \
    -H "API-Version: 2024-10" \
    -H "Content-Type: application/json" \
    --data "$(jq -cn --arg q "$1" '{query: $q}')"
}
monday '{ me { name email } }'
```

## First checks

- A 403 `not_connected` means your owner has not connected Monday: tell
  them to connect it in the shell's Settings, under Connections. A 403
  `forbidden` means they kept this agent from it.
- Do not ask for a shared Monday API key.
- Do not ask the human to paste a personal token anywhere.

## Working style

- Start read-only: list boards, inspect columns, and understand board-specific status labels before mutating anything.
- When changing items or updates, make the smallest targeted change possible and summarize exactly what changed.
- Read a board's columns (`boards(ids: [ID]) { columns { id title type settings_str } }`) before writing a column value: status and people columns take board-specific JSON.

## Good uses

- List boards, groups, items, and updates
- Inspect board schema before changing statuses or people columns
- Create or update items after confirming the target board and columns
- Read or post updates on behalf of the connected user

## Examples

```bash
monday '{ boards(limit: 10) { id name workspace { name } } }'
monday '{ boards(ids: [1234567890]) { groups { id title } items_page(limit: 20) { items { id name column_values { id text } } } } }'
monday 'mutation { create_update(item_id: 1234567890, body: "Done, see the attached notes.") { id } }'
```
