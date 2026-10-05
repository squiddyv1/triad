---
name: triad-engagement
description: "Use when running an authorized pentest with the triad stack (Strix + Cairn + Hermes)."
version: 1.0.0
metadata:
  hermes:
    tags: [pentest, triad, cairn, strix, roe]
    category: security
---

# Triad engagement loop

Discovery scans the target. Cairn searches the exploitation state space.
Hermes is the control plane: it decides, gates, records and reports. Never let
one layer do another layer's job; that separation is the whole point.

## Before anything

1. Confirm written authorization and a scope list. No scope file, no run.
2. Fill `contracts/roe-instructions.md` for this engagement (in scope, out of
   scope, rate limit, forbidden actions, stop conditions).
3. Set the kill switch up front so you can use it: `cairn_status(project_id, "stopped")`.

## The loop

1. **Open the engagement.** `cairn_create(title, origin="target <url>",
   goal="<objective>", hints=[<roe lines>, <credentials>, <focus>])`.
   Keep `bootstrap=false` when you want Strix to lead discovery first.
2. **Discover.** `strix_scan(target, instruction_file=<roe>, scan_mode="quick"|"deep",
   max_turns=N, workdir=<engagement dir>)`. Non-blocking; a scan is tens of minutes.
   Always set `max_turns`; `--max-budget` does not trip on unpriced models.
3. **Read honestly.** `strix_findings(workdir)`. `findings` are validated vulns;
   `coverage_gaps` are *unexamined*, never "no issues found". Report both.
4. **Hand off.** `triad_feed(project_id, workdir)` posts findings as hints and
   exploitation intents onto the Cairn graph.
5. **Let Cairn work.** Poll `cairn_graph(project_id)` for new facts, open intents,
   dead ends. Add `cairn_hint` when you have context the workers lack
   (new credentials, a dead end, a scanner lead). Do not micromanage.
6. **Validate before you claim success.** A Cairn `completed` status is a claim,
   not proof. Read `cairn_graph(project_id, format="path")` and check the chain
   end to end. If it does not hold, `cairn_reopen(project_id, "<why>")`.
7. **Close out.** Report the path (origin -> ... -> goal), the evidence per fact,
   plus the coverage gaps and anything left unexamined.

## Hard rules

- **Read vs write authority.** Strix is read-biased discovery and runs with a
  non-destructive ROE. Cairn is the write-capable layer and should be the only
  thing holding target credentials, behind a scope-enforcing egress proxy.
  Never give Strix credentials that Cairn needs.
- **Approval gates are software, not prompts.** Before any step that mutates a
  target (creates accounts, uploads files, writes data, drops anything), stop
  and get a human approval through the gateway. `cairn_status(..., "stopped")`
  is the freeze; resume only after the human says go.
- **No destructive cleanup skills.** Anything that deletes or overwrites on a
  target is out of scope for automation. Wipe-after-extraction style skills are
  how the referenced campaign destroyed a victim's backups; do not carry them.
- **Budget per workflow, not per token.** Track Strix `run.json` -> `llm_usage.cost`
  and Cairn `runtime.max_workers` / `max_running_projects`. Stop the run when the
  cost per completed objective stops making sense.
- **Everything is audited.** Cairn's `export?format=timeline` is the immutable
  graph history; the plugin's `post_tool_call` hook is the tool audit trail.
  Keep both with the report.
