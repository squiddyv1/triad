# Triad: one deployable system over Strix and Cairn

Discovery and exploitation as two layers with a clean handoff, driven by one
command-line tool. Hermes is an optional third layer, never a requirement.

```
        ┌─────────────────────────────────────────────────────────────────────────┐
        │ triad  CLI: engage, scan, feed, watch, report, status                   │
        │                                                                         │
        │ Hermes is optional: control plane, skills, memory,                      │
        │ approvals, kill switch. It is never required.                           │
        └─────────────────────────────────────────────────────────────────────────┘

        ┌────────────────────────┐         ┌────────────────────────────────────┐
        │ STRIX                  │         │ CAIRN                              │
        │ discovery              │         │ exploitation / state search        │
        │ Docker sandbox         │──feed──▶│ server :8000 + dispatcher          │
        │ strix_runs/<run>/      │         │ Fact / Intent / Hint graph         │
        └────────────────────────┘         └────────────────────────────────────┘
        strix_*                            cairn_*
              read authority                          write authority
```

- **Strix**: autonomous pentest agent in a Docker sandbox. Produces
  `strix_runs/<run>/{vulnerabilities.json, findings.sarif, run.json}`.
- **Cairn** ([oritera/Cairn](https://github.com/oritera/Cairn)): blackboard
  Fact/Intent state-space search engine with a REST API. Give it `origin` + `goal`
  and its workers explore toward the goal. This is the layer that touches the target.
- **`triad`** (this repo): the driver. It seeds the Cairn graph from Strix output,
  reads the graph back, and writes the report. It loads the same `plugin/` package
  the Hermes integration uses, directly, so no agent framework is needed to run it.
- **Hermes** (optional): adds orchestration on top: the loop, policy, budget,
  approvals, audit trail, plus skills and memory so an engagement improves between
  runs. Installed only with `--with-hermes`; everything else works without it.

The normal flow is `triad.py engage` -> Strix scan -> `triad.py feed` -> Cairn
dispatcher -> `triad.py report`. Reasoning, verified API notes, failure modes and
the build plan: **[ARCHITECTURE.md](ARCHITECTURE.md)**.
---

## Install

```bash
git clone https://github.com/squiddyv1/triad.git
cd triad
./install.sh
```

`install.sh` is idempotent: it installs every layer the flow needs and wires the
optional one only if you ask.

| Layer | Default behaviour |
|---|---|
| **uv** (Cairn's runner, and what `triad up` starts things with) | install if missing |
| **Docker** (daemon + compose v2) | install if missing; `--no-docker` to only report |
| **Triad harness** (tool package, CLI, contracts) | install |
| **Cairn** (`oritera/Cairn`, cloned + patched) | install |
| **Strix** | install if missing (the flow needs it) |
| **Hermes** (optional) | install only with `--with-hermes`; if already present, the plugin is linked |

Docker, uv and Strix are installed with **the exact commands their own docs
publish**, so this follows the official route rather than inventing one. Hermes,
when you ask for it, goes through its own installer the same way:

```
Docker  curl -fsSL https://get.docker.com | sh                             -> dockerd + compose v2
uv      curl -LsSf https://astral.sh/uv/install.sh | sh                    -> ~/.local/bin/uv
Strix   curl -sSL https://strix.ai/install | bash                          -> ~/.strix/bin/strix
Hermes  curl -fsSL https://hermes-agent.nousresearch.com/install.sh | bash -> ~/.hermes
```

The only deviation is that the script is fetched to a temp file and its size and
sha256 are printed before it runs, a reported one-liner instead of a blind pipe.
Same bytes, same installer. Docker's needs root, so it runs under `sudo` (or
directly when the installer is already root).

What it does, in order:

1. checks prerequisites and reports exactly what is missing;
2. installs uv if it is absent (Cairn's runner, and what `triad up` uses);
3. installs Docker if it is absent, starts the daemon, adds you to the `docker`
   group and ensures the compose v2 plugin;
4. installs Strix if it is absent (see the method knobs below);
5. clones Cairn and applies `patches/0001-opencode-worker-backend.patch`
   (upstream Cairn is never vendored into this repo);
6. creates `.env` from `.env.example` and the engagement directory;
7. installs a `triad` wrapper into `~/.local/bin` so the CLI works from anywhere;
8. only if Hermes is present, or `--with-hermes` was passed: symlinks `plugin/`
   into `$HERMES_HOME/plugins/triad` and validates it with `hermes plugins doctor`.
   Otherwise it reports that Hermes is absent and moves on, because the CLI drives
   the same package directly.

```bash
./install.sh                 # install / repair uv + Docker + Strix + Cairn; leave Hermes alone
./install.sh --with-hermes   # also install Hermes and wire the plugin
./install.sh --no-docker     # do not install Docker, only report whether it is there
./install.sh --no-uv         # do not install uv, only report whether it is there
./install.sh --detect-only   # report what is present, install nothing
./install.sh --check         # verify install health, change nothing
./install.sh --uninstall     # remove the symlinks it created (leaves Strix/Hermes alone)
```

`uv` and Docker are both installed when missing, because Cairn cannot run without
either; `UV_INSTALL_METHOD=none` and `--no-docker` (or `DOCKER_INSTALL_METHOD=none`)
downgrade either one to a report. Both are idempotent, and an existing install is
never touched.

Docker publishes packages for Debian, Ubuntu, Raspbian, Fedora, the RHEL family,
SLES and openSUSE only. On a derivative (Kali, Parrot, Mint, Pop) the vendor script
asks for a suite that does not exist, for example `debian kali-rolling`, so the
installer uses the distro's own `docker.io` instead, which is what Kali documents.
If such an attempt left an unusable `docker.list` behind, that entry is removed so
`apt` keeps working; a Docker source you configured yourself is left alone.

### Install method

| `TRIAD_INSTALL_METHOD` | Behaviour |
|---|---|
| `official` (default) | the vendor script from each project's README (above); for Docker it is skipped on distros Docker does not publish for |
| `pkg` | `uv tool install strix-agent` / `hermes-agent`, else `pipx`, then fall back to the vendor script; for Docker it is the distro's own packages (`docker.io`, `docker-ce`, `moby-engine`) |
| `none` | detect only, never install |

`STRIX_INSTALL_METHOD` / `HERMES_INSTALL_METHOD` override it per layer. `official`
is the default because it is the route both projects support and it tracks their
current releases; `pkg` produces a pinned, uninstallable install but **PyPI lags
the vendor channel** (`hermes-agent` there is 0.19.0 while the official script
tracks 0.21.x), so prefer `official` unless you specifically want the pinned one.

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
| Docker | installed if missing by default: the daemon is started and the invoking user is added to the `docker` group; `--no-docker` to only report it |
| Strix | Docker running; `pipx install strix-agent` or `curl -sSL https://strix.ai/install \| bash`; an LLM key |
| Cairn | `uv` (installed if missing) + Docker **and the `docker compose` v2 plugin** for container mode, **or** local mode reusing a host worker CLI |
| Hermes (optional) | a Hermes install, only for the control plane; `mcp<2` for the MCP bridge (`uv run --with 'mcp<2'`) |

---

## Run it

```bash
triad setup      # wizard: API keys -> .env, then starts the stack
```

`triad setup` asks which model Strix should drive and for the key, writes `.env`
(mode 600, comments preserved), then starts the Cairn server, starts the
dispatcher, waits for the Cairn API to answer and prints what to run next. Bare
`triad` does the same on an unconfigured checkout, and otherwise reports where
things stand. Each step is also available on its own:

```bash
triad up         # start the Cairn server and the dispatcher
triad down       # stop them (data is kept); --keep-server stops only the dispatcher
triad status     # list projects, or one graph with --project
```

The server and the dispatcher are **separate processes**, and a project will not
move until the dispatcher is running. `triad up` runs the dispatcher on the host,
which is the path verified end to end here; `--container` uses the compose
dispatcher instead, which needs the amd64-only worker image. Runtime pids and logs
live in `.triad/`, so a dispatcher that refuses to start says why in its log.

Drive it headlessly. This is the normal flow, and it needs no Hermes:

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

If Hermes is installed, the plugin exposes this same package as `cairn_*` and
`strix_*` tools, so a chat session drives exactly these calls. That path is
convenience, not a dependency.

Emergency stop for every project: `make stop-all`. With the Hermes gateway,
`cairn_status(project_id, "stopped")` is one message away.

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
├── plugin/                    the tool package: strix_* / cairn_* tools + the loop skill
│   └── skills/triad-engagement/SKILL.md
├── integrations/hermes/config-snippets.yaml  MCP + cron + gateway wiring (optional)
└── mcp/cairn_mcp.py           Cairn as an MCP server (Strix *or* Hermes can use it)
```

`plugin/` is a plain Python package: `triad.py` imports it directly, and Hermes
loads the same directory as a plugin when it is installed. `integrations/` holds
the Hermes-only wiring and is inert otherwise.

`cairn/` appears after install and is gitignored.

---

## The `opencode` worker backend (this repo's patch to Cairn)

Upstream Cairn ships four worker backends: `claudecode`, `codex`, `pi`, `mock`.
If none of those CLIs is installed, the exploitation layer cannot run at all.
This repo adds a fifth: `opencode`.

- `patches/0001-opencode-worker-backend.patch`: the driver plus its registration
  in `workers/adapters/__init__.py`, `workers/registry.py`, and `WorkerType` /
  `WORKER_ENV_KEYS` in `dispatcher/config.py`.
- Parses opencode's `--format json` event stream for reply text and session id, so
  the conclude phase continues the same session.
- Worker env: `OPENCODE_MODEL`, `OPENCODE_AGENT`, `OPENCODE_AUTO` (default on),
  plus `OPENCODE_BASE_URL` / `OPENCODE_API_KEY` / `OPENCODE_EXTRA_HEADERS` for the
  health check. In local mode no keys are injected; it reuses the host config.
- 9 tests; all 107 pass (98 upstream + 9 new). Verified end to end against a live
  target: bash tool use, the exact `{"accepted": true, "data": {...}}` reply
  contract, and cost telemetry.

---

## Known constraints

- **The optional Hermes layer needs several GB.** Its installer unpacks Python, Node,
  npm, ripgrep and FFmpeg into `$HERMES_HOME/tools` and clones the agent; a fresh
  install lands around 7 GB. `install.sh` checks for 4 GB free under `$HOME` first
  and refuses early with a clear reason rather than dying halfway through the
  download. This is the main reason Hermes is opt-in: the Strix + Cairn flow needs
  a fraction of that.
- **The `goal` string is the autonomy boundary.** A goal one finding can satisfy buys
  you one finding: the reason step completes the project and the remaining intents sit
  stranded. Scope it up front ("conclude or rule out every module") rather than
  reopening later: `reopen` records a correction but does not change the completion
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
