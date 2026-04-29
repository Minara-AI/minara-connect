<!--
Thanks for the PR! A few things that make review faster:

- PR title mirrors the squashed commit subject: `type(scope): subject`.
- Link the issue this closes (`Closes #123`) if there is one.
- For wire-format / hook / security-relevant changes, please call that out
  explicitly — those need extra eyes.
-->

## Summary

<!-- 1-3 bullets. What + why. Link any issue this closes. -->

-

## Test plan

<!-- Concrete commands a reviewer can run. Be specific. -->

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --workspace --all-targets --tests -- -D warnings`
- [ ] `cargo test --workspace`
- [ ] (if chat-ui touched) `(cd chat-ui && bun test && bunx tsc --noEmit && bunx prettier --check .)`
- [ ] (if hook / wire / on-disk changes) `scripts/smoke-test.sh` passes locally

## Domain & docs

<!-- Tick whichever applies. Skip the section if none does. -->

- [ ] New or renamed domain term → updated [`CONTEXT.md`](../CONTEXT.md)
- [ ] Architectural decision → added [`docs/adr/NNNN-…`](../docs/adr/) and referenced it in the PR
- [ ] Wire-format change → bumped `v` / ALPN per [`PROTOCOL.md`](../PROTOCOL.md) §0
- [ ] Threat surface change → updated [`SECURITY.md`](../SECURITY.md)

## Notes for the reviewer

<!-- Anything you want surfaced: a tricky edge case, a follow-up issue,
     a non-obvious decision. Keep it short. -->
