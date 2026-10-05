# Triad — one deployable system over Strix + Cairn + Hermes

Discovery, exploitation and orchestration as three layers with clean handoffs,
run by a single agent control plane.

```
        ┌───────────────────────────────────────────────────────────┐
        │  HERMES  — control plane (orchestration, memory, skills,   │
        │           cron, approvals, audit, kill switch)             │
        └───────────┬──────────────────────────────┬────────────────┘
        tools: strix_*                         tools: cairn_*
                    │                              │
        ┌───────────▼──────────┐      ┌────────────▼──────────────────┐
        │  STRIX               │      │  CAIRN                        │
        │  discovery           │─────▶│  exploitation / state search  │
        │  Docker sandbox      │ feed │  server :8000 + dispatcher    │
        │  strix_runs/<run>/   │      │  Fact / Intent / Hint graph   │
        └──────────────────────┘      └───────────────────────────────┘
             read authority                  write authority
```

- **Strix** — autonomous pentest agent, Docker sandbox, validated findings.
  Produces `strix_runs/<run>/{vulnerabilities.json, findings.sarif, run.json}`.
- **Cairn** ([oritera/Cairn](https://github.com/oritera/Cairn)) — blackboard
  Fact/Intent state-space search engine with a REST API. Give it `origin` + `goal`
  and its workers explore toward the goal. This is the layer that touches the target.
- **Hermes** — the control plane: the loop, the policy, the budget, the approvals,
  the audit trail, plus skills and memory so an engagement improves every run.

Reasoning, verified API notes, failure modes and the build plan:
**[ARCHITECTURE.md](ARCHITECTURE.md)**.

---

## Install

```bash
git clone https://github.com/<you>/triad.git
cd triad
./install.sh
```

`install.sh` is idempotent and does five things:

1. checks prerequisites and reports exactly what is missing;
2. clones Cairn and applies `patches/0001-opencode-worker-backend.patch`
   (upstream Cairn is never vendored into this repo);
3. creates `.env` from `.env.example` and the engagement directory;
4. symlinks the Hermes plugin into `$HERMES_HOME/plugins/triad` and validates it
   with `hermes plugins doctor`;
5. installs a `triad` wrapper into `~/.local/bin` so the CLI works from anywhere.

Verify, repair, or remove at any time:

```bash
./install.sh --check        # report install health, change nothing
./install.sh                # re-run to repair
./install.sh --uninstall    # remove the symlinks it created
```

Every path is overridable, so nothing is machine-specific:

| Variable | Default | Meaning |
|---|---|---|
| `TRIAD_HOME` | script directory | where the harness lives |
| `CAIRN_DIR` | `$TRIAD_HOME/cairn` | where Cairn is cloned |
| `HERMES_HOME` | `~/.hermes` | which Hermes profile to extend |
| `BIN_DIR` | `~/.local/bin` | where the `triad` CLI goes |
| `ENGAGEMENTS` | `~/engagements` | engagements, scans, reports |

### Prerequisites

| Component | Needs |
|---|---|
| Python | ≥ 3.9 for the CLI (the installer checks) |
| Strix | Docker running; `pipx install strix-agent` or `curl -sSL https://strix.ai/install \| bash`; an LLM key |
| Cairn | `uv` (the installer checks) + Docker **and the `docker compose` v2 plugin** for container mode, **or** local mode reusing a host worker CLI |
| Hermes | a Hermes install; `mcp<2` for the MCP bridge (`uv run --with 'mcp<2'`) |

---

## Run it

```bash
$EDITOR .env                       # LLM keys
make bootstrap                     # pulls the worker image, checks the stack
make up                            # Cairn server: http://127.0.0.1:8000

cd cairn && uv run --project cairn cairn dispatch --config ../dispatch.local.yaml
```

The server and the dispatcher are **separate processes**, and a project will not
move until the dispatcher is running.

Then either talk to Hermes (the plugin exposes `cairn_*` / `strix_*` tools), or
drive it headlessly:

```bash
triad engage --title ACME --target https://app.example \
             --goal "conclude or rule out every finding in scope" \
             --roe contracts/roe-instructions.md
triad scan   --target https://app.example --workdir ~/engagements/acme \
             --roe contracts/roe-instructions.md --mode quick --max-turns 50
triad feed   --project proj_001 --workdir ~/engagements/acme
triad watch  --project proj_001 --timeout 1800
triad report --project proj_001 --workdir ~/engagements/acme -o report.md
```

Emergency stop for every project: `make stop-all`, or
`cairn_status(project_id, "stopped")` one message away in the gateway.

---

## Layout

```
triad/
├── install.sh                 the installer (idempotent, path-overridable)
├── triad.py                   headless driver (engage/scan/feed/watch/report)
├── patches/                   the one change this repo makes to Cairn
├── docker-compose.yaml        Cairn server + dispatcher (+ optional egress proxy)
├── dispatch.yaml              worker pool, model routing, concurrency caps
├── dispatch.local.yaml        no-Docker / arm64 fallback (uses the opencode backend)
├── contracts/
│   ├── finding.schema.json    the layer-to-layer handoff object
│   └── roe-instructions.md    rules-of-engagement template
├── hermes/plugin-triad/       Hermes plugin: strix_* + cairn_* tools + the loop skill
├── hermes/config-snippets.yaml  MCP + cron + gateway wiring
└── mcp/cairn_mcp.py           Cairn as an MCP server (Hermes *and* Strix can use it)
```

`cairn/` appears after install and is gitignored.

---

## The `opencode` worker backend (this repo's patch to Cairn)

Upstream Cairn ships four worker backends: `claudecode`, `codex`, `pi`, `mock`.
If none of those CLIs is installed, the exploitation layer cannot run at all.
This repo adds a fifth: `opencode`.

- `patches/0001-opencode-worker-backend.patch` — the driver plus its registration
  in `workers/adapters/__init__.py`, `workers/registry.py`, and `WorkerType` /
  `WORKER_ENV_KEYS` in `dispatcher/config.py`.
- Parses opencode's `--format json` event stream for reply text and session id, so
  the conclude phase continues the same session.
- Worker env: `OPENCODE_MODEL`, `OPENCODE_AGENT`, `OPENCODE_AUTO` (default on),
  plus `OPENCODE_BASE_URL` / `OPENCODE_API_KEY` / `OPENCODE_EXTRA_HEADERS` for the
  health check. In local mode no keys are injected — it reuses the host config.
- 9 tests; all 107 pass (98 upstream + 9 new). Verified end to end against a live
  target: bash tool use, the exact `{"accepted": true, "data": {...}}` reply
  contract, and cost telemetry.

---

## Known constraints

- **The `goal` string is the autonomy boundary.** A goal one finding can satisfy buys
  you one finding: the reason step completes the project and the remaining intents sit
  stranded. Scope it up front ("conclude or rule out every module") rather than
  reopening later — `reopen` records a correction but does not change the completion
  criterion, so a literally-satisfied goal re-completes within one pass.
- **Evidence is written to `/tmp/cairn-prompts/<phase>-<hash>/`**, not the engagement
  workspace, and `/tmp` is volatile. Copy it into the engagement directory at close-out.
- **Cairn's worker image is `linux/amd64` only.** On arm64 use `dispatch.local.yaml`
  or add `platform: linux/amd64` (qemu emulation, slow). Strix's sandbox image is
  multi-arch and runs natively on arm64.
- **Cairn is AGPL-3.0.** Free for personal/educational use; commercial use needs a
  commercial license from its author. See [LICENSE](LICENSE).
- **`--max-budget` in Strix silently no-ops on models LiteLLM cannot price.** Use
  `--max-turns`; the plugin sets it by default.
- **A host reboot leaves `run.json` at `status: running`** even with complete
  artifacts. Judge a run by its artifacts, not that field.
- **The dispatcher does not restart itself.** The compose server does
  (`restart: unless-stopped`); `cairn dispatch` is a plain process.
- **Cairn pins the Aliyun PyPI mirror.** If that is slow from your network, override
  with `UV_DEFAULT_INDEX=https://pypi.org/simple` before building.

---

## Authorization

These are offensive tools. Point them only at systems you own, or that you hold
explicit written permission to test, within the window that permission covers.
Filling in `contracts/roe-instructions.md` before a run is not optional.
