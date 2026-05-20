# Triage Labels

The `triage` skill maps its canonical issue states to these GitHub labels.

| Canonical role | GitHub label | Meaning |
| --- | --- | --- |
| `needs-triage` | `needs-triage` | A maintainer needs to evaluate the issue. |
| `needs-info` | `needs-info` | The issue is waiting on more information from the reporter. |
| `ready-for-agent` | `ready-for-agent` | The issue is fully specified and ready for an AFK agent. |
| `ready-for-human` | `ready-for-human` | The issue is ready, but needs human implementation. |
| `wontfix` | `wontfix` | The issue will not be actioned. |

## Agent Rules

- Apply the mapped GitHub label for the current triage state.
- Remove stale triage-state labels when moving an issue to a new triage state.
- If one of these labels is missing in GitHub, ask before creating it unless the user explicitly requested label creation.
