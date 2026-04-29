# Issue tracker

Issues for this repo live at **GitHub Issues** on `Minara-AI/cc-connect`.

Skills (`to-issues`, `to-prd`, `triage`, `qa`) interact with it via the `gh` CLI:

- `gh issue list` — read issues
- `gh issue view <num>` — read full body + comments
- `gh issue create --title … --body …` — file new issues
- `gh issue edit <num> --add-label …` — apply triage labels
- `gh api repos/Minara-AI/cc-connect/issues/<num>/comments` — read/write comments

When `to-issues` produces vertical slices, file each as a separate issue and link parent → children with `Blocks: #N` / `Blocked by: #N` lines in the body. Use the labels in `triage-labels.md`.

External labels we also use beyond triage roles:

- `area:protocol` `area:hook` `area:mcp` `area:tui` `area:chat-ui` `area:install` `area:security` `area:docs`
- `kind:bug` `kind:feature` `kind:chore` `kind:rfc`
- `good-first-issue` — must include enough context an outside contributor can pick it up cold
