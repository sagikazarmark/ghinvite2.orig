# Domain Docs

This repository uses a single-context domain-doc layout.

## Layout

- Root context file: `CONTEXT.md`
- Architectural decision records: `docs/adr/`
- No root `CONTEXT-MAP.md` is expected for this repo.

## Agent Rules

- Before domain-sensitive work, read `CONTEXT.md` if it exists.
- Before architectural changes, read relevant ADRs under `docs/adr/` if they exist.
- Treat `CONTEXT.md` as the source of project domain language and product concepts.
- Treat ADRs as the source of past architectural decisions and constraints.
- If these files do not exist yet, proceed from the codebase and existing documentation instead of inventing domain rules.
- Do not assume a multi-context layout unless `CONTEXT-MAP.md` is introduced later.
