# Rules for working on this project

These rules apply to every contributor, human or AI (Claude Code, Copilot, Hermes or any other
agent).

## Document every change in the knowledge base (`rag/`)

`rag/` explains how every feature works, one feature per file (see `rag/README.md`). It is the
primary documentation for this project. **Code comments are not enough.** Every change has to be
explained in `rag/` as well.

For **every** change to code, migrations, config, prompts, Docker or data:

1. **Update the feature file(s)** in `rag/` that describe what you changed, so they match the code
   afterwards. Change the "How it works", "Rules and why", "Configuration", "Tests" and "Gotchas"
   sections as needed, and set `last_reviewed` to today.
2. **New feature?** Create a new `rag/<feature>.md` using the same frontmatter and sections, and add
   it to the index table in `rag/README.md`.
3. **Removed feature?** Delete or rewrite its file and its index entry. Don't leave docs describing
   code that no longer exists.
4. **Add an entry to `rag/CHANGELOG.md`** (newest first) explaining:
   - **What** changed
   - **Why** it changed (the problem or request behind it)
   - **How** it works now, in plain words
   - **Files**: the code paths touched
   - **Docs**: the `rag/` files updated
5. **Bugs and gaps:** if you find one, add it to `rag/known-issues.md`. When you fix one, remove it
   there and mention the fix in the changelog.
6. **Uncommitted or experimental work** is marked `status: uncommitted` in its feature file until
   it's committed.

A change is not finished until steps 1–4 are done. When you report your work, say which `rag/` files
you updated.

## Writing style for `rag/`

- Explain the mechanism (what calls what, which table, which constraint), not just the intent.
- Give exact names: functions, tables, env vars, routes, file paths.
- Write in plain English. Short sentences. Tables for lists of things.
- Don't paste large code blocks. Point to `file:function` instead.

## Project basics

- The core rule: **the model never writes facts.** Prices, cars, times and contact details come from
  the database, through tools. Don't weaken this. See `rag/overview.md`.
- Build, run and test: see `rag/running.md` and `rag/testing.md`.
