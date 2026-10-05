#!/usr/bin/env bash
#
# Triad installer — Strix (discovery) + Cairn (exploitation) + Hermes (control plane).
#
#   ./install.sh              install into this checkout
#   ./install.sh --check      verify an existing install, change nothing
#   ./install.sh --uninstall  remove the symlinks this script created
#
# Everything is configurable through environment variables:
#   TRIAD_HOME    where the harness lives        (default: this script's directory)
#   CAIRN_DIR     where Cairn is cloned          (default: $TRIAD_HOME/cairn)
#   HERMES_HOME   the Hermes profile to extend   (default: $HERMES_HOME or ~/.hermes)
#   BIN_DIR       where the `triad` CLI goes     (default: ~/.local/bin)
#   ENGAGEMENTS   where engagements/runs live    (default: ~/engagements)
#
# Nothing here is machine-specific: every path is derived or overridable, and the
# script is idempotent — run it again to repair an install.

set -euo pipefail

TRIAD_HOME="${TRIAD_HOME:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
CAIRN_DIR="${CAIRN_DIR:-$TRIAD_HOME/cairn}"
HERMES_HOME="${HERMES_HOME:-$HOME/.hermes}"
BIN_DIR="${BIN_DIR:-$HOME/.local/bin}"
ENGAGEMENTS="${ENGAGEMENTS:-$HOME/engagements}"

CAIRN_REPO="${CAIRN_REPO:-https://github.com/oritera/Cairn.git}"
PATCH="$TRIAD_HOME/patches/0001-opencode-worker-backend.patch"
PLUGIN_SRC="$TRIAD_HOME/hermes/plugin-triad"
PLUGIN_DST="$HERMES_HOME/plugins/triad"

MODE="install"
case "${1:-}" in
  --check)     MODE="check" ;;
  --uninstall) MODE="uninstall" ;;
  -h|--help)   sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
  "")          ;;
  *)           echo "unknown option: $1 (try --help)" >&2; exit 2 ;;
esac

ok()   { printf '  \033[32m✓\033[0m %s\n' "$*"; }
warn() { printf '  \033[33m!\033[0m %s\n' "$*"; }
err()  { printf '  \033[31m✗\033[0m %s\n' "$*" >&2; }
hdr()  { printf '\n\033[1m%s\033[0m\n' "$*"; }
have() { command -v "$1" >/dev/null 2>&1; }

# --------------------------------------------------------------------------- #
# uninstall
# --------------------------------------------------------------------------- #
if [ "$MODE" = "uninstall" ]; then
  hdr "Uninstalling"
  [ -L "$PLUGIN_DST" ] && rm -f "$PLUGIN_DST" && ok "removed plugin symlink $PLUGIN_DST"
  [ -f "$BIN_DIR/triad" ] && rm -f "$BIN_DIR/triad" && ok "removed $BIN_DIR/triad"
  warn "kept: $CAIRN_DIR, $TRIAD_HOME/.env, $ENGAGEMENTS (delete manually if you want them gone)"
  exit 0
fi

# --------------------------------------------------------------------------- #
# prerequisites
# --------------------------------------------------------------------------- #
hdr "Checking prerequisites"
FAIL=0

if have python3; then
  PYV=$(python3 -c 'import sys;print("%d.%d"%sys.version_info[:2])')
  if python3 -c 'import sys; sys.exit(0 if sys.version_info>=(3,9) else 1)'; then
    ok "python3 $PYV"
  else
    err "python3 $PYV — need >= 3.9"; FAIL=1
  fi
else
  err "python3 not found"; FAIL=1
fi

if have docker; then
  if docker info >/dev/null 2>&1; then ok "docker (daemon reachable)"
  else warn "docker installed but the daemon is not reachable — needed by Strix and by Cairn container mode"; fi
else
  warn "docker not found — Strix requires it; Cairn local mode does not"
fi

if have uv; then ok "uv $(uv --version 2>/dev/null | awk '{print $2}')"
else warn "uv not found — Cairn needs it: https://docs.astral.sh/uv/getting-started/installation/"; fi

if have strix || [ -x "$HOME/.strix/bin/strix" ]; then ok "strix present"
else warn "strix not found — install it (see README) or set PATH to include ~/.strix/bin"; fi

if [ -d "$HERMES_HOME" ]; then ok "Hermes home: $HERMES_HOME"
else warn "no Hermes home at $HERMES_HOME — the plugin step will create it, but Hermes must be installed to use it"; fi

if [ "$MODE" = "check" ]; then
  hdr "Install check"
  if [ -d "$CAIRN_DIR/.git" ]; then ok "Cairn checkout at $CAIRN_DIR"; else err "no Cairn checkout at $CAIRN_DIR (run ./install.sh)"; FAIL=1; fi
  if [ -d "$CAIRN_DIR/.git" ]; then
    if git -C "$CAIRN_DIR" apply --reverse --check "$PATCH" >/dev/null 2>&1; then
      ok "opencode backend patch applied"
    else
      err "opencode backend patch NOT applied"; FAIL=1
    fi
  fi
  if [ -e "$PLUGIN_DST" ]; then ok "Hermes plugin linked at $PLUGIN_DST"; else err "Hermes plugin not linked"; FAIL=1; fi
  if [ -x "$BIN_DIR/triad" ]; then ok "triad CLI at $BIN_DIR/triad"; else err "triad CLI missing at $BIN_DIR/triad"; FAIL=1; fi
  if [ -f "$TRIAD_HOME/.env" ]; then ok ".env present"; else warn ".env missing (copy .env.example and fill it in)"; fi
  if command -v python3 >/dev/null 2>&1 && "$BIN_DIR/triad" --help >/dev/null 2>&1; then
    ok "triad CLI runs"
  else
    err "triad CLI does not run"; FAIL=1
  fi
  echo
  if [ "$FAIL" = 0 ]; then ok "install looks healthy"; exit 0; else err "install is incomplete"; exit 1; fi
fi

[ "$FAIL" = 0 ] || { echo; err "fix the errors above, then re-run."; exit 1; }

# --------------------------------------------------------------------------- #
# Cairn checkout + backend patch
# --------------------------------------------------------------------------- #
hdr "Cairn (exploitation layer)"
if [ -d "$CAIRN_DIR/.git" ]; then
  ok "using existing checkout at $CAIRN_DIR"
else
  echo "  cloning $CAIRN_REPO"
  git clone --depth 1 "$CAIRN_REPO" "$CAIRN_DIR"
  ok "cloned to $CAIRN_DIR"
fi

if git -C "$CAIRN_DIR" apply --reverse --check "$PATCH" >/dev/null 2>&1; then
  ok "opencode backend already applied"
elif git -C "$CAIRN_DIR" apply --check "$PATCH" >/dev/null 2>&1; then
  git -C "$CAIRN_DIR" apply "$PATCH"
  ok "applied opencode worker backend"
else
  err "patch does not apply cleanly to $CAIRN_DIR"
  err "upstream Cairn has probably moved. Inspect $PATCH and report it."
  exit 1
fi

# --------------------------------------------------------------------------- #
# config + directories
# --------------------------------------------------------------------------- #
hdr "Configuration"
mkdir -p "$ENGAGEMENTS"
if [ -f "$TRIAD_HOME/.env" ]; then
  ok ".env exists (left untouched)"
else
  cp "$TRIAD_HOME/.env.example" "$TRIAD_HOME/.env"
  chmod 600 "$TRIAD_HOME/.env"
  warn "created .env — fill in your API keys before running a scan"
fi
mkdir -p "$TRIAD_HOME/datas/cairn"
ok "engagements: $ENGAGEMENTS"

# --------------------------------------------------------------------------- #
# Hermes plugin
# --------------------------------------------------------------------------- #
hdr "Hermes plugin"
mkdir -p "$HERMES_HOME/plugins"
if [ -L "$PLUGIN_DST" ] || [ -d "$PLUGIN_DST" ]; then
  rm -rf "$PLUGIN_DST"
fi
ln -s "$PLUGIN_SRC" "$PLUGIN_DST"
ok "linked $PLUGIN_DST -> $PLUGIN_SRC"

DOCTOR_OUT="$(hermes plugins doctor "$PLUGIN_SRC" 2>&1 || true)"
if printf '%s' "$DOCTOR_OUT" | grep -q "registration passed"; then
  ok "plugin validates ($(printf '%s' "$DOCTOR_OUT" | sed -n 's/.*registrations: //p'))"
else
  warn "could not validate the plugin with \`hermes plugins doctor\` — run it yourself once Hermes is on PATH"
fi

# --------------------------------------------------------------------------- #
# CLI wrapper
# --------------------------------------------------------------------------- #
hdr "CLI"
mkdir -p "$BIN_DIR"
cat > "$BIN_DIR/triad" <<EOF
#!/usr/bin/env bash
# Installed by triad install.sh — thin wrapper so the CLI works from anywhere.
export TRIAD_HOME="\${TRIAD_HOME:-$TRIAD_HOME}"
export CAIRN_BASE_URL="\${CAIRN_BASE_URL:-http://127.0.0.1:8000}"
export TRIAD_WORKDIR="\${TRIAD_WORKDIR:-$ENGAGEMENTS}"
exec python3 "\$TRIAD_HOME/triad.py" "\$@"
EOF
chmod +x "$BIN_DIR/triad"
ok "$BIN_DIR/triad"

case ":$PATH:" in
  *":$BIN_DIR:"*) ok "$BIN_DIR is on PATH" ;;
  *) warn "$BIN_DIR is NOT on PATH — add: export PATH=\"$BIN_DIR:\$PATH\"" ;;
esac

# --------------------------------------------------------------------------- #
# done
# --------------------------------------------------------------------------- #
hdr "Next steps"
cat <<EOF
  1. Fill in API keys:            \$EDITOR $TRIAD_HOME/.env
  2. Start Cairn:                 cd $TRIAD_HOME && make up        # or: docker compose up -d
  3. Start the dispatcher:        cd $CAIRN_DIR && \\
       uv run --project cairn cairn dispatch --config $TRIAD_HOME/dispatch.local.yaml
  4. Restart Hermes so the plugin + MCP tools load, then talk to it.

  Verify at any time:             ./install.sh --check
  Full walkthrough:               $TRIAD_HOME/README.md
  Design + operations notes:      $TRIAD_HOME/ARCHITECTURE.md
EOF
