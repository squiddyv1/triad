![Triad](assets/banner.svg)

<p align="center">
  <b>Discovery, then exploitation, from one command.</b><br>
  <sub>Strix finds. Cairn proves. <code>triad</code> connects the two. Hermes is optional.</sub>
</p>

<p align="center">
  <img alt="MIT license" src="https://img.shields.io/badge/license-MIT-3fb950">
  <img alt="Python 3.9 or later" src="https://img.shields.io/badge/python-3.9%2B-3776ab">
  <img alt="no agent framework required" src="https://img.shields.io/badge/agent%20framework-not%20required-8b949e">
</p>

---

## The idea

Two layers with a clean handoff between them, driven by one CLI.

| Layer | Job |
|---|---|
| **Strix** ([usestrix/strix](https://github.com/usestrix/strix)) | Autonomous pentest agent in a Docker sandbox. Writes `strix_runs/<run>/{vulnerabilities.json, findings.sarif, run.json}`. |
| **Cairn** ([oritera/Cairn](https://github.com/oritera/Cairn)) | Blackboard search over a Fact/Intent graph. Give it an origin and a goal, and its workers explore toward it. This is the layer that touches the target. |
| **`triad`** (this repo) | The driver. Seeds the Cairn graph from Strix output, reads the graph back, writes the report. |
| **Hermes** (optional) | Orchestration on top: the loop, policy, budget, approvals, audit trail, plus skills and memory so an engagement improves between runs. |

```text
  STRIX  discovery, sandboxed, read authority  ──feed──▶  CAIRN  exploitation, credentialed, write authority
```

Strix never holds target credentials; Cairn never discovers. `triad engage` walks the
whole path in one command: create the project, run the scan, post every finding into
the graph as a hint and every actionable one as an intent, then let Cairn's dispatcher
work the leads.

Reasoning, API notes and failure modes: **[ARCHITECTURE.md](ARCHITECTURE.md)**.

## Install

```bash
git clone https://github.com/squiddyv1/triad.git && cd triad
./install.sh
```

Idempotent, and it installs the toolchain with **the exact commands each project
publishes**, fetched to a temp file whose size and sha256 are printed before it runs
rather than piped blind:

```text
uv        curl -LsSf https://astral.sh/uv/install.sh | sh                    -> ~/.local/bin/uv
opencode  curl -fsSL https://opencode.ai/install | bash                    -> ~/.opencode/bin
Docker    curl -fsSL https://get.docker.com | sh                            (needs root)
Strix     curl -sSL https://strix.ai/install | bash                         -> ~/.strix/bin
Hermes    curl -fsSL https://hermes-agent.nousresearch.com/install.sh | bash   (--with-hermes only)
```

It then creates `.env`, clones and patches Cairn, and puts a `triad` wrapper in
`~/.local/bin`.

| Flag | Effect |
|---|---|
| `--with-hermes` | also install Hermes and wire the plugin |
| `--no-docker`, `--no-uv`, `--no-worker` | only report that layer, do not install it |
| `--check` | verify install health, change nothing |
| `--detect-only` | report what is present, install nothing |
| `--uninstall` | remove the symlinks it made (leaves Strix and Hermes alone) |

**On Kali, Parrot, Mint or Pop:** Docker publishes packages for Debian and Ubuntu
only, so its installer asks for a suite that does not exist (`debian kali-rolling`)
and apt fails. The installer spots the derivative and uses the distro's own
`docker.io` instead, which is what Kali documents, and it removes a broken
`docker.list` left behind by a failed attempt so apt keeps working.

Docker and uv are both installed when missing, since Cairn needs one of them.
`UV_INSTALL_METHOD=none` and `DOCKER_INSTALL_METHOD=none` (or bare `--no-docker`,
`--no-uv`) downgrade either to a report. An existing install is never touched.

<details>
<summary>Install method and path overrides</summary>

| `TRIAD_INSTALL_METHOD` | Behaviour |
|---|---|
| `official` (default) | the vendor script above; for Docker it is skipped on distros Docker does not publish for |
| `pkg` | `uv tool install strix-agent` / `hermes-agent`, else `pipx`, falling back to the vendor script; for Docker it is the distro's own packages |
| `none` | detect only, never install |

`STRIX_INSTALL_METHOD` and `HERMES_INSTALL_METHOD` override it per layer. Prefer
`official`: PyPI lags the vendor channel (`hermes-agent` there is 0.19.0, the
official script tracks 0.21.x).

| Variable | Default | Meaning |
|---|---|---|
| `TRIAD_HOME` | script directory | where the harness lives |
| `CAIRN_DIR` | `$TRIAD_HOME/cairn` | where Cairn is cloned |
| `HERMES_HOME` | `~/.hermes` | which Hermes profile to extend |
| `BIN_DIR` | `~/.local/bin` | where the `triad` CLI goes |
| `ENGAGEMENTS` | `~/engagements` | engagements, scans, reports |

</details>

### Prerequisites

| Component | Needs |
|---|---|
| Python | 3.9+ for the CLI (checked by the installer) |
| Docker | installed if missing: daemon started, invoking user added to the `docker` group |
| Worker CLI | opencode is installed if none of opencode, claude, codex, pi is present |
| Strix | Docker running and an LLM key |
| Cairn | `uv`, plus Docker and the compose v2 plugin for container mode, or local mode reusing a host worker CLI |
| Hermes (optional) | only for the control plane |

> Added to the `docker` group? Group membership only reaches **new** logins. Run
> `newgrp docker`, or log out and back in, before anything Docker-based will work.

## Run it

```bash
triad setup
```

One question: which provider should **Strix and the Cairn worker** share. A single key
covers both, and it then shows that provider's **real model list** so no id is ever
typed from memory.

If the two sides should differ (a cheap model on the high-volume worker, a strong one
on Strix, or a worker on a subscription while Strix is billed):

```bash
triad configure      # a model per side, preselected from what each is using
triad models         # just look at what a provider serves
```

```bash
triad up             # start the Cairn server and the dispatcher
triad down           # stop them (data is kept); --keep-server stops the dispatcher only
triad status         # list projects, or one graph with --project
triad auth           # (re)write the worker CLI's key from .env; --show to inspect
```

The worker CLI never needs `opencode auth login`: `setup` writes the key straight into
opencode's own credentials file, `~/.local/share/opencode/auth.json`, mode 600,
merging rather than replacing. If a chosen worker model ever shows as a git change,
that is a bug: `configure` writes machine-specific settings to `.triad/` (gitignored)
and leaves `dispatch.local.yaml` as the template.

### An engagement

```bash
triad engage --title ACME --target https://app.example \
             --goal "conclude or rule out every finding in scope" \
             --roe contracts/roe-instructions.md
triad watch  --project proj_001
triad report --project proj_001 --workdir ~/engagements/acme -o report.md
```

- `engage` takes the dispatcher down for the duration of the scan and starts it once
  the findings are in, so Cairn cannot bootstrap a graph holding none of Strix's
  input. `--no-pause` allows the overlap; `--hold` leaves Cairn idle afterwards.
- `--mode` and `--max-turns` size the scan, `--scan-timeout` extends the one-hour
  default, `--json` prints a scriptable summary.
- The steps work alone when a scan is already running, or was run elsewhere:
  `triad scan`, `triad findings`, `triad feed`. Re-feeding is safe, because hints and
  intents are additive.
- **Scope the goal carefully.** It is the autonomy boundary: a goal one finding can
  satisfy buys you one finding, and the rest of the work sits stranded. Scope it up
  front ("conclude or rule out every module") rather than reopening later: `reopen`
  records a correction but does not change the completion criterion, so a
  literally-satisfied goal re-completes within one pass.
- With the dispatcher stopped, nothing in Cairn advances (bootstrap included), so the
  server is safe to leave up.
- **Emergency stop for every project:** `make stop-all`. Through the Hermes gateway,
  `cairn_status(project_id, "stopped")` is one message away.

## Layout

```text
triad/
├── assets/                    the banner above, plus the mark on its own (logo.svg)
├── install.sh                 the installer, idempotent and path-overridable
├── triad.py                   the driver: setup, configure, engage, watch, report
├── patches/                   the one change this repo makes to Cairn
├── docker-compose.yaml        Cairn server + dispatcher (+ optional egress proxy)
├── dispatch.yaml              worker pool, model routing, concurrency caps
├── dispatch.local.yaml        no-Docker / arm64 fallback on the opencode backend
├── contracts/
│   ├── finding.schema.json    the layer-to-layer handoff object
│   └── roe-instructions.md    rules-of-engagement template
├── plugin/                    strix_* / cairn_* tools plus the engagement skill
├── integrations/hermes/       MCP, cron and gateway wiring (inert without Hermes)
└── mcp/cairn_mcp.py           Cairn as an MCP server, for Strix or for Hermes
```

`plugin/` is a plain Python package: `triad.py` imports it directly, and Hermes loads
the same directory as a plugin when it is installed, exposing the same calls as
`strix_*` and `cairn_*` tools so a chat session drives exactly what the CLI does.
`cairn/` appears after install and is gitignored.

## The opencode worker backend

Upstream Cairn ships four worker backends (claudecode, codex, pi, mock). With none of
those CLIs installed the exploitation layer cannot run at all, so this repo adds a
fifth: `opencode`. Shipped as a patch
(`patches/0001-opencode-worker-backend.patch`), never as a vendored Cairn. It registers
the backend in `workers/adapters/__init__.py`, `workers/registry.py` and
`WorkerType` / `WORKER_ENV_KEYS` in `dispatcher/config.py`, and parses opencode's
`--format json` event stream for reply text and session id, so the conclude phase
continues the same session.

Its env keys are `OPENCODE_MODEL`, `OPENCODE_AGENT` and `OPENCODE_AUTO`, plus
`OPENCODE_BASE_URL` / `OPENCODE_API_KEY` / `OPENCODE_EXTRA_HEADERS` for the health
check. In local mode no keys are injected; it reuses the host config. Verified against
a live target, including the exact `{"accepted": true, "data": {...}}` reply contract,
bash tool use and cost telemetry. 9 new tests; all 107 pass.

## Known constraints

- **The optional Hermes layer wants several GB.** Its installer unpacks Python, Node,
  npm, ripgrep and FFmpeg and clones the agent; a fresh install lands near 7 GB.
  `install.sh` checks for 4 GB free first and refuses early with a reason instead of
  dying halfway. This is the main reason it is opt-in.
- **Evidence lands in `/tmp/cairn-prompts/<phase>-<hash>/`**, not the engagement
  workspace, and `/tmp` is volatile. Copy anything you need at close-out.
- **Cairn's worker image is `linux/amd64` only.** On arm64 use `dispatch.local.yaml`,
  or add `platform: linux/amd64` and accept emulation. Strix's sandbox image is
  multi-arch and runs natively.
- **`--max-budget` in Strix silently no-ops on models LiteLLM cannot price.** Bound a
  run with `--max-turns`; the plugin sets it by default.
- **A host reboot leaves `run.json` at `status: running`** even with complete
  artifacts. Judge a run by its artifacts, not that field.
- **The dispatcher does not restart itself.** The compose server does; `cairn
  dispatch` is a plain process.
- **Cairn pins the Aliyun PyPI mirror.** If it is slow from your network, build with
  `UV_DEFAULT_INDEX=https://pypi.org/simple`.

## Authorization

These are offensive tools. Point them only at systems you own, or hold explicit
written permission to test, within the window that permission covers. Filling in
`contracts/roe-instructions.md` before a run is not optional.

## License

MIT, see [LICENSE](LICENSE). Cairn is cloned separately under its own **AGPL-3.0**,
which is why it is never vendored here: free for personal and educational use, but
commercial use needs a commercial licence from its author.
