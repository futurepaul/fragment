# Workflow Templates

Use the installed binary first. Replace it with `node build/app/index.js cli` only when the binary is unavailable and you are working inside the checked-out product repo with built artifacts.

## Profile Lookup

Inspect the tool if needed:

```bash
nostr-agent-interface cli getProfile --help
```

Run it:

```bash
nostr-agent-interface cli getProfile --pubkey npub...
```

## Generic Event Query

```bash
nostr-agent-interface cli queryEvents --kinds '[1]' --limit 5 --json
```

Retry once with explicit relays when a relay-default query fails:

```bash
nostr-agent-interface cli queryEvents \
  --kinds '[1]' \
  --limit 5 \
  --relays '["wss://relay.damus.io","wss://nos.lol"]' \
  --json
```

## Note Posting

Prefer stdin so the private key does not land in argv:

```bash
# Operator-provisioned input JSON fields: privateKey, content
nostr-agent-interface cli postNote --stdin --json < /path/to/operator-provided-input.json
```

Read first when the user wants context before posting.

## Event Create / Sign / Publish

```bash
# Operator-provisioned input JSON fields: kind, content, privateKey
nostr-agent-interface cli createNostrEvent --stdin --json < /path/to/operator-provided-input.json
```

Then sign:

```bash
# Operator-provisioned input JSON fields: privateKey, event
nostr-agent-interface cli signNostrEvent --stdin --json < /path/to/operator-provided-input.json
```

Then publish:

```bash
printf '%s' '{"signedEvent":{"id":"...","pubkey":"...","created_at":123,"kind":1,"tags":[],"content":"hello nostr","sig":"..."},"relays":["wss://relay.damus.io"]}' \
  | nostr-agent-interface cli publishNostrEvent --stdin --json
```

## DM Send / Read

Send NIP-44 DM:

```bash
# Operator-provisioned input JSON fields: privateKey, recipientPubkey, content
nostr-agent-interface cli sendDmNip44 --stdin --json < /path/to/operator-provided-input.json
```

Read inbox:

```bash
# Operator-provisioned input JSON fields: privateKey, limit
nostr-agent-interface cli getDmInboxNip44 --stdin --json < /path/to/operator-provided-input.json
```

## Relay List Lookup / Update

Read first:

```bash
nostr-agent-interface cli getRelayList --pubkey npub... --json
```

Treat relay-list changes as confirmation-worthy because they change account behavior:

```bash
# Operator-provisioned input JSON fields: privateKey, relayList
nostr-agent-interface cli setRelayList --stdin --json < /path/to/operator-provided-input.json
```

## Blossom Upload / Download / List / Delete

Upload:

```bash
# Operator-provisioned input JSON fields: privateKey, filePath
nostr-agent-interface cli uploadBlob --stdin --json < /path/to/operator-provided-input.json
```

Download:

```bash
nostr-agent-interface cli downloadBlob --sha256 <sha256> --server-url https://example.com --json
```

List:

```bash
nostr-agent-interface cli listBlobs --pubkey npub... --server-url https://example.com --json
```

Delete only after explicit confirmation:

```bash
# Operator-provisioned input JSON fields: privateKey, sha256, serverUrl
nostr-agent-interface cli deleteBlob --stdin --json < /path/to/operator-provided-input.json
```

## Error Recovery

1. Re-check `nostr-agent-interface cli <toolName> --help`.
2. Re-check `nostr-agent-interface cli list-tools --json`.
3. Retry once with explicit relays when the tool supports them.
4. Report the exact tool error plus a sanitized argument summary.
