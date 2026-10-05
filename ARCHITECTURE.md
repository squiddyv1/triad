# Triad: architecture and implementation plan

Deep-research notes for building one deployable system out of **Strix** (discovery),
**Cairn** (exploitation) and **Hermes** (orchestration), following the reference
architecture described in the Gambit campaign write-up.

Everything below was verified against the actual software: Cairn was cloned and its
server run locally, its REST lifecycle exercised end to end, Strix's installed CLI
and its artifact layout checked on this machine.

---

## 1. What the source material actually says

The Medium piece is a re-read of Gambit Security's disclosure of an operator who
chained three open-source agent frameworks against retailers. The facts worth
keeping, and which the design leans on:

| Claim | Why it matters for us |
|---|---|
| Strix: discovery. 146 deep-mode scans, 138 hosts, 633 scanner-hours in 195 clock-hours (23–31 Aug) | Discovery must be parallel and shaped as *structured opportunities*, not prose |
| Cairn: exploitation. Given a domain + objective, runs unattended for hours until success/timeout/stop | Execution is a separate trust domain and needs durable state |
| Hermes: orchestration. Persistent memory, self-editing skills (121 total, 78 attack), scheduled jobs, web console; 1,951 human prompts over 260 sessions | The control plane is where governance, budget and audit live, not the prompt |
| Model routing: Strix on GLM-5.2 → DeepSeek V4 Pro; Cairn on DeepSeek V4.1 Flash; Hermes on Claude Opus 4.6 | Cheap models on high-volume loops, strong models on decision points |
| ~$25.46 mean per completed scan; $12k–$18k total; $3.13–$79.31 range | Cost accounting per **workflow**, not per token |
| Correction to the popular retelling: 27 companies compromised in the 10–15 Sep burst; 600k+ card records came from **two** companies, not 27; skimmer counts are inconsistent across the report body and summary (19 vs 5 vs 119) | Do not repeat inflated numbers; report what the evidence supports |

**The architectural lesson**, which is the one we're implementing: *discovery
produces structured opportunities, execution is bounded and stateful, orchestration
is the control plane.* Everything else is a distribution detail.

**What we do differently.** The campaign's autonomy was unfiltered; ours is gated.
Three specific paper cuts from that campaign map directly onto controls we build in:

1. *"A skill whose purpose is to remove the content security filters of the harness
   itself."* → skills are reviewed artifacts in a repo, not agent-authored at runtime.
2. *A "Database Wipe After Extraction" skill whose table-name matching also dropped
   180 tables including the victim's own backups.* → destructive actions are
   denylisted in software and require human approval; nothing auto-deletes on a target.
3. *Only 1,951 prompts total, tens of companies per day, remediation measured in
   weeks.* → the defender's lever is the kill switch and per-workflow budget, both
   first-class here.

---

## 2. The three components as they actually are (verified)

### Strix: `usestrix/strix`, Apache-2.0, PyPI `strix-agent`

- Installed locally as a **standalone aarch64 binary**, `strix 1.6.2` at `~/.strix/bin/strix`.
- Headless: `strix -n --target <t> --scan-mode quick --max-turns 60 --instruction-file <roe>`.
- Exit codes: `0` clean, `1` fatal, `2` vulnerabilities found.
- Artifacts in `strix_runs/<run>/`: `run.json`, `vulnerabilities.json`,
  `vulnerabilities/*.md`, `penetration_test_report.md`, `findings.sarif`,
  `coverage.json`, `.state/`.
- Needs a running Docker daemon; sandbox image `ghcr.io/usestrix/strix-sandbox:1.3.0`
  (~5.8 GB), **multi-arch including arm64**.
- Is itself an MCP **client** (`~/.strix/mcp-servers.json`, stdio or http), which is
  how we let the scanning agent read and write the Cairn blackboard mid-scan.
- `--max-budget` silently fails on unpriced models; **`--max-turns` is the guardrail**.

### Cairn: `oritera/Cairn`, AGPL-3.0, `cairn` v0.2.1

- Two processes: **server** (FastAPI + SQLite, graph consistency only) and
  **dispatcher** (schedules tasks, manages worker containers, sole writer to the protocol).
- Three primitives: **Fact** (immutable confirmed finding), **Intent** (declared
  exploration direction, claimable), **Hint** (out-of-graph human/external input).
- Workers run an OODA loop with no fixed roles; tasks are `bootstrap` / `reason` /
  `explore`. Backends: **Claude Code, Codex, Pi**.
- Container mode: `ghcr.io/oritera/cairn-worker-container:latest`, **linux/amd64 only**.
  Local mode (`runtime.execution: local`) runs the CLIs on the host, no Docker, no sandbox.
- REST API on `:8000`, verified live. Exact lifecycle:

```
POST /projects            {title, origin, goal, bootstrap_enabled, hints[]}
                          -> {project, facts, intents, hints}
POST /projects/{id}/intents {from:[fact_id], description, creator, worker|null}
                          -> intent          (worker must be null or == creator, else 422)
POST /projects/{id}/intents/{iid}/heartbeat {worker}   -> claim/renew
POST /projects/{id}/intents/{iid}/conclude  {worker, description}
                          -> {fact, intent}   (writes a new Fact, closes the edge)
POST /projects/{id}/complete {from:[fact_id], description, worker}
                          -> the completion intent (to == "goal"); project -> completed
POST /projects/{id}/reopen {description, creator}
                          -> {project, fact, intent}; adds an `external_feedback` edge, back to active
PUT  /projects/{id}/status {status:"stopped"|"active"}
                          -> project           ** the kill switch **
GET  /projects/{id}/export?format=yaml|timeline
GET  /projects, GET /projects/{id}, DELETE /projects/{id} (204)
GET|PUT /settings         {intent_timeout, reason_timeout}
```

- `stopped` is a hard stop: open intents lose their claims immediately, `project.reason`
  is cleared, and the dispatcher cancels local work and drops orphan containers. Any
  exploration write against a stopped project is rejected (`Project is stopped`).
  Hints are still accepted while stopped or completed.

### Hermes: Nous Research, the framework this runs on

- Control plane surfaces we use:
  - **Plugin system**: `ctx.register_tool(name, toolset, schema, handler)` puts tools
    in the registry; `ctx.register_hook("post_tool_call", fn)` gives the audit trail.
  - **MCP client**: `mcp_servers:` in `config.yaml`, tools surface as `mcp_<server>_<tool>`.
  - **Cron**: scheduled runs that can attach skills (the nightly re-scan).
  - **Gateway**: the human approval channel (Telegram here) and the emergency stop.
  - **Skills + memory**: the engagement loop and per-client knowledge persist and
    improve; this is the layer the campaign used for its 121 skills, run in reverse.

---

## 3. The integration design

### 3.1 One contract, three layers

The failure mode the article names is a next layer forced to re-interpret a prose
blob. So the handoff is one JSON object (`contracts/finding.schema.json`) carrying
target identity, evidence, confidence, recommended next action, prerequisites,
risk level, scope limits and expected side effects. `risk_level: destructive`
requires human approval. That is the software-enforced version of "ask before you
write", not a prompt asking the model to be careful.

### 3.2 Two integration surfaces, chosen deliberately

- **Hermes → Cairn: REST, via the `cairn_*` plugin tools.** The control plane reasons
  about the graph (open intents, dead ends, path to goal) and writes hints/intents.
  It does not need the graph as chat context; it needs typed operations.
- **Hermes/Strix → Cairn: MCP (`mcp/cairn_mcp.py`).** One bridge, two consumers. This
  is the cheapest way to make the blackboard a native tool surface for both the
  orchestrator and the scanning agent, and it lets you restrict Strix to the
  **read-only subset** (`cairn_list_projects`, `cairn_get_project`, `cairn_export`,
  `cairn_goal_path`) with `allowed_tools`, which is read/write separation enforced by
  configuration rather than by asking nicely.
- **Hermes → Strix: process supervision.** Strix is a CLI, not a service. The plugin
  launches it detached (`start_new_session=True`), returns the PID and run dir, and a
  later `strix_findings` call reads the artifacts. Scans are tens of minutes; nothing
  blocks on them.

### 3.3 The loop

```
1. engage      cairn_create(origin=target, goal=objective, hints=ROE)
               -> project_id;  bootstrap=false so discovery leads
2. discover    strix_scan(target, instruction_file=ROE, max_turns=N)     [read-biased]
3. read        strix_findings(workdir) -> findings + coverage_gaps
               coverage_gaps are UNEXAMINED, never "clean"
4. hand off    triad_feed(project_id, workdir)
               -> one Hint per finding, one Intent per critical/high/medium,
                  anchored on `origin` (or a better fact if one exists)
5. exploit     Cairn workers claim intents and push the graph toward `goal`
               Hermes only: poll cairn_graph, add cairn_hint for context the
               workers lack, and gate any write action
6. validate    cairn_graph(project_id, format="path") -> origin -> ... -> goal
               a `completed` status is a CLAIM; unsupported -> cairn_reopen
7. report      path + per-fact evidence + coverage gaps + $ cost per objective
```

Steps 2–4 can be fanned out across targets in parallel (that is exactly what Strix's
633-hours-in-195-hours number represents); steps 5–6 are bounded by
`runtime.max_running_projects` so the bill stays legible.

### 3.4 Model routing

Mirror the campaign's routing, with the cost discipline attached:

| Layer | Job | Model class | Why |
|---|---|---|---|
| Strix | scan / validate | GLM-5.3 (its tuned default) or DeepSeek-V4-Pro | high volume, tool-calling heavy |
| Cairn `explore` | try one intent | cheapest that can tool-call (V4.1 Flash) | highest token volume, lowest stakes per call |
| Cairn `reason`/`bootstrap` | decide the graph's next move | strong (V4 Pro / Opus-class) | these are the real decision points |
| Hermes | orchestrate, gate, report | strongest available | few calls, each high impact |

`dispatch.yaml` ships exactly this split.

---

## 4. Deployment

### 4.1 Single-host, Docker compose (the default)

`docker-compose.yaml` + `dispatch.yaml` + `make up`. The server binds to
`127.0.0.1:8000`; the dispatcher mounts the host Docker socket so worker containers
get host networking (that is Cairn's design, and it is also why the egress proxy
matters; see below). Data is a single SQLite file under `./datas/cairn/`, so backup
and "move the engagement to another box" are file copies.

Hermes stays on the host rather than in a container: it needs the gateway, the
skills, the plugin directory and the LLM credentials that are already configured
there. `make plugin` symlinks the plugin in; the MCP block goes in `config.yaml`.

### 4.2 arm64 / no-Docker fallback

`dispatch.local.yaml`: `runtime.execution: local`, no worker image, workers are host
processes reusing the already-logged-in `claude`/`codex`/`pi` CLIs. This is the
correct path on this machine (aarch64): the worker container is amd64-only, and Strix,
the layer that genuinely benefits from a sandbox, is multi-arch and runs natively.

**Local mode has no sandbox and runs with your user's permissions.** Only use it on a
host you control, on a network you control, inside an authorized engagement.

### 4.3 Failure modes to design against

| Failure | Symptom | Control |
|---|---|---|
| Strix blocked by WAF | `coverage_gaps`, low findings | carry forward as *unexamined*; never report "clean"; kill the run early (token burn is front-loaded and mostly cache reads) |
| `--max-budget` doesn't trip | cost reads $0.0000, runs forever | use `--max-turns`; read real usage from `run.json -> llm_usage` |
| Runaway exploitation | workers keep spawning containers | `runtime.max_workers`, `max_running_projects`, `max_project_workers`, `tasks.reason.max_intents` |
| Agent does something irreversible | a wipe, a drop, an overwrite | `risk_level` gate + egress proxy + `cairn_status(..., "stopped")` |
| Prompt-cache / cost surprises | bill outruns value | log cost per completed objective, not per token; stop on a threshold |
| Engagement data leaking across clients | one target's creds used on another | one Cairn project per target, one workspace per engagement, scoped proxy rules |

---

## 5. Phased build plan

**Phase 0: stand it up (half a day).** Clone Cairn, `make bootstrap`, `make up`,
`make plugin`. Verify `curl :8000/projects` and that Hermes sees `cairn_*` tools.
Everything in this repo is built to make phase 0 the whole setup.

**Phase 1: one target, tool-call driven (a day).** Pick a deliberately vulnerable
target you own (DVWA/Juice Shop). Run the loop by hand from a Hermes session:
create → scan → read → feed → watch → validate. The goal is to learn where the
handoff is lossy. Do **not** automate yet.

**Phase 2: close the loop (2–3 days).** Wrap the loop in a Hermes cron job
(`hermes/config-snippets.yaml`) that iterates engagements, re-scans stale targets,
and posts a digest. Add the approval gate in the gateway and the cost ledger.

**Phase 3: harden (ongoing).** Egress proxy with a generated allowlist from the
scope file; a denylist of destructive actions enforced in the plugin's
`post_tool_call` hook; signed audit export (Cairn `timeline` + the plugin hook log);
per-client memory namespaces so one engagement's facts never bleed into another's.

**Phase 4: scale and productize.** Multi-target fan-out, a findings sink (SARIF →
your GRC/Jira), and a report renderer that walks `cairn_graph(format="path")` into
the narrative. This is a genuinely nice property: the attack path *is* the
deliverable, already ordered and already evidenced.

---

## 6. Licensing and legal posture

- **Cairn is AGPL-3.0** for personal/educational use; commercial use requires a
  commercial license from the author. If this ends up inside a paid engagement
  deliverable or an internal product, get that resolved first. Strix is Apache-2.0.
- **Authorization is the gate, not a formality.** Everything here is dual-use. The
  scope file is read before a scan starts, the proxy enforces it, and the kill
  switch is one message away. Ship it with those three things or don't ship it.

---

## 7. Open questions to settle before Phase 2

1. **Who owns the graph long-term?** Cairn's SQLite is per-engagement; Hermes memory
   is per-profile. Decide the split so findings survive both.
2. **Does Cairn's worker prompt set need forking?** Its `container/AGENTS.md` is CTF-shaped
   (Kali container, OOB callbacks, tmux shells). For client work you want a
   client-shaped prompt group, reviewed in git. This is also the control that makes
   lesson #1 from the campaign impossible here.
3. **Approval granularity.** Per-action approval kills throughput; per-project approval
   is too coarse. The middle ground is per-`risk_level`: read-only runs free, state
   changes notify, destructive blocks.
4. **Cost ceiling semantics.** Per engagement? Per day? Pick one and encode it, or the
   first runaway week sets the policy for you.

---

## 8. Dry run: what actually happened

Target: `https://pentest-ground.com:4280`, a public intentionally-vulnerable practice
host (DVWA). Full run, from engagement to report.

**Discovery (Strix).** `strix -n --scan-mode quick --max-turns 50` with the ROE as
`--instruction-file`. It ran ~19 minutes before the host VM rebooted mid-scan,
249 LLM requests, 18.4M tokens. It produced **12 validated findings** (2 critical, 3
high, 7 medium) and **14 coverage gaps**, each with raw request/response evidence,
including an unauthenticated RCE, UNION-based SQLi, LFI-to-RCE, path traversal
leaking DB credentials, and blind SQLi.

The ROE held. The scan's own coverage notes say things like "Exploitation
deliberately NOT performed by recon agent", "no file uploaded (ROE forbids
persisting files)", "create_db action deliberately not exercised. ROE forbids
state-changing actions", and "Not brute-forced (ROE prohibits brute force)".
Written constraints changed behaviour, not just the report.

**Handoff.** `triad.py feed` posted 26 hints and 12 exploitation intents into Cairn,
each intent carrying the original evidence verbatim.

**Exploitation search (Cairn + one opencode worker).** The dispatcher claimed two
intents, the worker reproduced the RCE **and strengthened it**: it tested all three
shell separators, then proved genuine shell evaluation rather than reflection by
computing a marker (`echo CAIRN_MARKER_$((6*7))` → `CAIRN_MARKER_42`), captured
`uid=33(www-data)`, fingerprinted the host with `uname -a`, and distinguished
baseline from injected output by Content-Length (4529 vs 4583). It wrote its
evidence to a file and referenced it from the fact, as Cairn's protocol prescribes.
It then declared the goal met with a written justification.

**Then it stopped, correctly.** The goal was "confirm at least one exploitable
vulnerability with evidence". One confirmed RCE satisfies that literally, so the
reason step completed the project with 13 intents still open. This is the single
most important operational lesson of the run:

> **The `goal` string is the autonomy boundary.** A goal that one finding can
> satisfy buys you one finding. `reopen` does not widen it: it deletes the
> completion edge, writes your correction as a plain fact, and returns the project
> to `active`, but the completion criterion is unchanged, so the reason step
> re-completed within one pass. Scope the objective up front ("conclude or rule out
> every module", not "confirm at least one issue"); reserve `reopen` for a
> completion that was *wrong*, not one that was merely *narrow*.

**Cost.** 18.4M tokens / 249 requests on the discovery side, ~$0.86 of opencode
across all 43 sessions counted by `opencode stats`. Strix reported cost `$0.0`,
the documented `--max-budget` blind spot for models LiteLLM cannot price, which is
exactly why `--max-turns` is the guardrail.

**Controls exercised live.** `stop` on a completed project → 409
`Completed projects cannot change status`; `reopen` → `active`; `stop` → `stopped`;
a write to a stopped project → 403 `Project is stopped`. The dispatcher's worker
pool honoured `max_project_workers` (claimed 2, skipped the 3rd).

**Gaps this run exposed.**

1. Worker argv must include the binary name in both execution modes. Omitting it
   fails at task time with `FileNotFoundError: 'run'`. Found and fixed; the new
   adapter has 9 tests covering it.
2. Evidence lands in `/tmp/cairn-prompts/<phase>-<hash>/`, not the engagement
   workspace, and `/tmp` is volatile. Copy-out is a required step, not a nicety.
3. A host reboot left `run.json` at `status: running` while the artifacts were
   complete, so judge a run by its artifacts, not by that field.

