# Domain docs layout

Single context. No `CONTEXT-MAP.md`.

```
/
├── CONTEXT.md            ← Ubiquitous Language: every domain term
├── PROTOCOL.md           ← wire spec (RFC 2119); breaking changes bump `v`
├── SECURITY.md           ← threat model
├── TODOS.md              ← v0.1 → v1.0 gap list
└── docs/
    └── adr/
        ├── 0001-machine-scoped-identity.md
        ├── 0002-backfill-via-custom-rpc-not-iroh-docs.md
        ├── 0003-pid-based-active-rooms-discovery.md
        └── 0004-hook-budget-and-graceful-overflow.md
```

## Consumer rules (for `grill-with-docs`, `improve-codebase-architecture`)

- **Read CONTEXT.md before challenging a plan's terminology.** If a user uses a term that conflicts with the glossary, surface it before the conversation drifts.
- **Update CONTEXT.md inline.** When a new term is resolved during a grilling session, write it to `CONTEXT.md` in the same turn. Don't batch.
- **Read the relevant ADR before suggesting refactors in its area.** ADRs are decisions, not opinions. If a refactor contradicts one, mark it explicitly: _"contradicts ADR-NNNN — but worth reopening because…"_ and only when the friction is real.
- **Wire-format invariants live in PROTOCOL.md, not in code comments.** Anything that must hold across implementations belongs there. The reference implementation must follow the spec, not the other way round.
- **Threat assumptions live in SECURITY.md.** If a refactor changes what's protected (e.g. file permissions, rate limits, sensitive-path blocklists), update SECURITY.md in the same change.

## Writing a new ADR

Use the format in `.claude/skills/grill-with-docs/ADR-FORMAT.md`. File at `docs/adr/NNNN-kebab-title.md`. Increment `NNNN` to the next free four-digit number. The first paragraph is the **decision** (one sentence); the rest is **context** and **consequences**.

Only write an ADR when:

1. The decision is **hard to reverse** (cost to change later is meaningful).
2. It is **surprising without context** (a future reader will ask "why did they do it this way?").
3. It is the result of a **real trade-off** (genuine alternatives existed).

If any of those is missing, skip the ADR.
