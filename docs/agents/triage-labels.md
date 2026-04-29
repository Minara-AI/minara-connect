# Triage labels

The `triage` skill moves an issue through a five-state machine. Each canonical role maps to a real GitHub label string applied via `gh issue edit --add-label`:

| Role               | Label              | Meaning                                                        |
| ------------------ | ------------------ | -------------------------------------------------------------- |
| `needs-triage`     | `needs-triage`     | Maintainer hasn't evaluated yet. Default for incoming issues.  |
| `needs-info`       | `needs-info`       | Waiting on the reporter for clarification. Will auto-close.    |
| `ready-for-agent`  | `ready-for-agent`  | Fully specified — an AFK agent (Claude) can pick it up cold.   |
| `ready-for-human`  | `ready-for-human`  | Spec is clear but the work needs a human (design call, infra). |
| `wontfix`          | `wontfix`          | Won't be actioned. Add a one-line reason in a closing comment. |

Rules:

1. Every open issue carries exactly one of these labels.
2. `triage` removes the prior role label when applying a new one.
3. `ready-for-agent` requires: reproducible repro, clear acceptance criteria, the right `area:*` label, no architectural decisions left open.
4. `wontfix` is closed, not left open.

The labels are created by `scripts/init-repo.sh` (one-shot, see CONTRIBUTING.md). If they don't exist yet on the remote, run that script before triaging.
