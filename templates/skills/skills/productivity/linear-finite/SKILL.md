---
name: linear-finite
description: Manage Linear issues, teams, workflow states, projects, users, and comments via a bundled GraphQL helper script, through your owner's connected Linear account.
version: 1.1.0
author: Hermes Agent
license: MIT
metadata:
  tags: [Linear, Project Management, Issues, GraphQL, API, Productivity]
---

# Linear

Use the bundled helper instead of hand-writing GraphQL curl commands.

Script path:

```bash
python3 ${SKILL_DIR}/scripts/linear_api.py --help
```

Your computer holds no keys. The helper acts through your owner's connected
Linear account: `LINEAR_API_KEY` holds the connection's placeholder (it
names you), the helper sends it to `api.linear.app`, and the computer swaps
in a short-lived token. Unset, your owner has not connected Linear, or this
deployment does not offer it yet (a catalog row adds it): ask them to
connect it in the shell's Settings, under Connections. A 403 `forbidden`
means they kept this agent from it.

## Workflow

1. `viewer`, `teams`, and `workflow-states` to establish current context
2. `issues`, `issue`, or `issue-search` to inspect work
3. `create-issue`, `update-state`, `assign`, `set-priority`, or `add-comment` to change state
4. `request` only when a needed GraphQL operation is not already covered

## Commands

Current user:

```bash
python3 ${SKILL_DIR}/scripts/linear_api.py viewer
```

Teams and workflow states:

```bash
python3 ${SKILL_DIR}/scripts/linear_api.py teams

python3 ${SKILL_DIR}/scripts/linear_api.py workflow-states \
  --team-key ENG
```

Issue lists:

```bash
python3 ${SKILL_DIR}/scripts/linear_api.py issues \
  --limit 20

python3 ${SKILL_DIR}/scripts/linear_api.py issues \
  --team-key ENG \
  --state-type started \
  --limit 20

python3 ${SKILL_DIR}/scripts/linear_api.py issue-search \
  --query "bug login" \
  --limit 10
```

Single issue:

```bash
python3 ${SKILL_DIR}/scripts/linear_api.py issue \
  --issue-id ENG-123
```

Projects, users, labels:

```bash
python3 ${SKILL_DIR}/scripts/linear_api.py projects
python3 ${SKILL_DIR}/scripts/linear_api.py users
python3 ${SKILL_DIR}/scripts/linear_api.py labels
```

Mutations:

```bash
python3 ${SKILL_DIR}/scripts/linear_api.py create-issue \
  --team-id TEAM_UUID \
  --title "Fix login bug" \
  --description "Users cannot login with SSO" \
  --priority 2

python3 ${SKILL_DIR}/scripts/linear_api.py update-state \
  --issue-id ENG-123 \
  --state-id STATE_UUID

python3 ${SKILL_DIR}/scripts/linear_api.py assign \
  --issue-id ENG-123 \
  --assignee-id USER_UUID

python3 ${SKILL_DIR}/scripts/linear_api.py set-priority \
  --issue-id ENG-123 \
  --priority 1

python3 ${SKILL_DIR}/scripts/linear_api.py add-comment \
  --issue-id ISSUE_UUID \
  --body "Investigated. Root cause is X."
```

Raw GraphQL escape hatch:

```bash
python3 ${SKILL_DIR}/scripts/linear_api.py request \
  --query 'query { viewer { id name } }'
```

Machine-readable output:

```bash
python3 ${SKILL_DIR}/scripts/linear_api.py issues \
  --team-key ENG \
  --json
```

## Notes

- Workflow states are team-specific; fetch `workflow-states` before changing status.
- Priorities: `0=none`, `1=urgent`, `2=high`, `3=medium`, `4=low`.
- Use the script’s output directly; do not pipe raw GraphQL responses into another interpreter just to pretty-print them.
