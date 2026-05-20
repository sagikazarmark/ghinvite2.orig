# Issue Tracker

Issues for this repository are tracked in GitHub Issues for `sagikazarmark/ghinvite2.orig`.

## Tooling

Use the GitHub CLI (`gh`) from the repository root.

Common commands:

- Create an issue: `gh issue create --repo sagikazarmark/ghinvite2.orig`
- View an issue: `gh issue view <number> --repo sagikazarmark/ghinvite2.orig`
- List issues: `gh issue list --repo sagikazarmark/ghinvite2.orig`
- Edit an issue: `gh issue edit <number> --repo sagikazarmark/ghinvite2.orig`

## Agent Rules

- Treat GitHub Issues as the source of truth for project issues.
- Do not write issue files under `.scratch/` unless the user explicitly asks for local markdown issues.
- Use the labels documented in `docs/agents/triage-labels.md` when triaging issues.
- If `gh` is unavailable or unauthenticated, stop and ask the user how to proceed instead of switching trackers silently.
