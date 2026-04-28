# Identity is scoped to one machine, not one person

Each machine running cc-connect generates its own Ed25519 keypair on first run; we do not synchronise keys across a person's devices. A user who runs cc-connect on a laptop and a desktop appears as two distinct Pubkeys; client-local Nickname mapping bridges the human-readable gap.

We picked this over a one-person-many-machines model to keep v0.1 simple: no key transport UX, no password-derived keys, no shared-secret bootstrap. The threat surface stays "whatever has access to the machine has access to the Identity," which is the same as the underlying SSH-key model.

If a v0.2+ feature is needed, the upgrade path is additive — a signed `i_am_also` Message kind lets one Identity claim another and renders them as one person in clients that understand the claim. v0.1 protocol does not need to change.
