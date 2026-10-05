#!/usr/bin/env bash
#
# Triad installer: Strix (discovery) + Cairn (exploitation) + Hermes (control plane).
#
#   ./install.sh                 install everything (all three layers)
#   ./install.sh --detect-only   report what is present, install nothing
#   ./install.sh --check         verify an existing install, change nothing
#   ./install.sh --uninstall     remove the symlinks this script created
#
# Env vars, install methods and the layer table are in README.md.
# Every path is overridable, so nothing here is machine-specific.

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

# Official installers, used only as a fallback when no package manager is present.
STRIX_INSTALL_URL="https://strix.ai/install"
HERMES_INSTALL_URL="https://hermes-agent.nousresearch.com/install.sh"

MODE="install"
# Installing the missing layers is the default; --detect-only turns it off.
INSTALL_METHOD="${TRIAD_INSTALL_METHOD:-official}"
for arg in "$@"; do
  case "$arg" in
    --check)        MODE="check" ;;
    --uninstall)    MODE="uninstall" ;;
    --detect-only|--no-deps)
                    INSTALL_METHOD="none" ;;
    --with-strix|--with-hermes|--all)
                    : ;;   # now the default; accepted for compatibility
    -h|--help)      sed -n '2,11p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *)              echo "unknown option: $arg (try --help)" >&2; exit 2 ;;
  esac
done

ok()   { printf '  \033[32m✓\033[0m %s\n' "$*"; }
warn() { printf '  \033[33m!\033[0m %s\n' "$*"; }
err()  { printf '  \033[31m✗\033[0m %s\n' "$*" >&2; }
hdr()  { printf '\n\033[1m%s\033[0m\n' "$*"; }
have() { command -v "$1" >/dev/null 2>&1; }
# Default is `official`: each project's own installer, fetched to a temp file and
# hashed rather than piped blindly. See README.md for the URLs and `pkg`.

# The vendor installers drop launchers in places this shell may not have on PATH
# yet (they tell you to `source ~/.bashrc`). Pick them up immediately.
refresh_path() {
  local d
  for d in "$HOME/.local/bin" "$HOME/.hermes/bin" "$HOME/.strix/bin"; do
    [ -d "$d" ] || continue
    case ":$PATH:" in
      *":$d:"*) ;;
      *) PATH="$d:$PATH" ;;
    esac
  done
  export PATH
  hash -r 2>/dev/null || true
}

pkg_install() {  # name-on-pypi
  local pkg="$1"
  if have uv; then
    echo "    uv tool install $pkg"
    uv tool install "$pkg"
    return $?
  fi
  if have pipx; then
    echo "    pipx install $pkg"
    pipx install "$pkg"
    return $?
  fi
  return 127   # no package manager available
}

script_install() {  # url
  local url="$1" tmp
  tmp="$(mktemp -t triad-installer.XXXXXX.sh)"
  curl -fsSL "$url" -o "$tmp"
  echo "    fetched $url"
  echo "    -> $tmp  ($(wc -c <"$tmp") bytes, sha256 $(sha256sum "$tmp" | cut -c1-32)…)"
  bash "$tmp" || { err "the installer exited non-zero (script kept at $tmp)"; return 1; }
  rm -f "$tmp"
}

# install_layer <pypi-name> <script-url> <method> <command-to-verify> <label>
install_layer() {
  local pkg="$1" url="$2" method="$3" cmd="$4" label="$5"
  case "$method" in
    official)
      echo "  installing $label using the command its repository documents"
      script_install "$url" || { err "$label install failed"; return 1; }
      ;;
    pkg)
      echo "  installing $label from a package manager (PyPI: $pkg)"
      if ! pkg_install "$pkg"; then
        warn "no package manager, or it failed; falling back to the vendor script"
        script_install "$url" || { err "$label install failed"; return 1; }
      fi
      ;;
    none)
      warn "$label is missing and installation is disabled (TRIAD_INSTALL_METHOD=none)"
      return 0
      ;;
    *) err "unknown install method '$method' (use official, pkg or none)"; return 2 ;;
  esac
  refresh_path
  if have "$cmd"; then
    ok "$label installed ($("$cmd" --version 2>/dev/null | head -1))"
  else
    warn "$label installed, but '$cmd' is not on PATH in this shell yet"
    warn "  open a new shell, or: source ~/.bashrc"
  fi
}

ensure_strix() {
  if have strix || [ -x "$HOME/.strix/bin/strix" ]; then ok "strix already installed"; return 0; fi
  local rc=0
  install_layer strix-agent "$STRIX_INSTALL_URL" \
                "${STRIX_INSTALL_METHOD:-$INSTALL_METHOD}" strix Strix || rc=$?
  echo "    note: Strix needs Docker; its sandbox image is pulled on the first scan"
  return "$rc"   # must not be clobbered by the echo above
}

ensure_hermes() {
  if have hermes || [ -x "$HOME/.local/bin/hermes" ]; then ok "hermes already installed"; return 0; fi
  # The vendor installer unpacks Python, Node, npm, ripgrep and FFmpeg into
  # $HERMES_HOME/tools: several GB, so check first and fail fast instead.
  local need_mb=4096 avail_mb
  avail_mb="$(df -Pm "$HOME" 2>/dev/null | awk 'NR==2{print $4}')"
  if [ -n "${avail_mb:-}" ] && [ "$avail_mb" -lt "$need_mb" ]; then
    err "Hermes needs roughly ${need_mb} MB free under $HOME; only ${avail_mb} MB available"
    err "free some space, or install Hermes yourself: $HERMES_INSTALL_URL"
    return 1
  fi
  local rc=0
  install_layer hermes-agent "$HERMES_INSTALL_URL" \
                "${HERMES_INSTALL_METHOD:-$INSTALL_METHOD}" hermes Hermes || rc=$?
  if [ "$rc" = 0 ]; then
    warn "the Hermes installer may have prompted for sudo (Node/libatomic)"
  fi
  return "$rc"   # must not be clobbered by the warn above
}
# Uninstall.
if [ "$MODE" = "uninstall" ]; then
  hdr "Uninstalling"
  [ -L "$PLUGIN_DST" ] && rm -f "$PLUGIN_DST" && ok "removed plugin symlink $PLUGIN_DST"
  [ -f "$BIN_DIR/triad" ] && rm -f "$BIN_DIR/triad" && ok "removed $BIN_DIR/triad"
  warn "kept: $CAIRN_DIR, $TRIAD_HOME/.env, $ENGAGEMENTS (delete manually if you want them gone)"
  warn "Strix and Hermes are separate installs; this does not touch them"
  exit 0
fi
# Prerequisites.
hdr "Checking prerequisites"
FAIL=0

if have python3; then
  PYV=$(python3 -c 'import sys;print("%d.%d"%sys.version_info[:2])')
  if python3 -c 'import sys; sys.exit(0 if sys.version_info>=(3,9) else 1)'; then
    ok "python3 $PYV"
  else
    err "python3 $PYV, need >= 3.9"; FAIL=1
  fi
else
  err "python3 not found"; FAIL=1
fi

if have docker; then
  if docker info >/dev/null 2>&1; then ok "docker (daemon reachable)"
  else warn "docker installed but the daemon is not reachable; needed by Strix and by Cairn container mode"; fi
else
  warn "docker not found; Strix requires it, Cairn local mode does not"
fi

if have uv; then ok "uv $(uv --version 2>/dev/null | awk '{print $2}')"
elif have pipx; then ok "pipx (no uv)"
else warn "neither uv nor pipx; Cairn needs uv: https://docs.astral.sh/uv/getting-started/installation/"; fi

if [ "$MODE" = "check" ]; then
  refresh_path
  if have strix || [ -x "$HOME/.strix/bin/strix" ]; then ok "strix present"; else warn "strix not found"; fi
  if have hermes || [ -x "$HOME/.local/bin/hermes" ]; then ok "hermes present"; else warn "hermes not found"; fi
else
  # Reported here; installed a few lines below, once the hard failures clear.
  if have strix || [ -x "$HOME/.strix/bin/strix" ]; then ok "strix present"
  else warn "strix missing; will install it ($INSTALL_METHOD)"; fi
  if have hermes || [ -x "$HOME/.local/bin/hermes" ]; then ok "hermes present"
  else warn "hermes missing; will install it ($INSTALL_METHOD)"; fi
  if [ -d "$HERMES_HOME" ]; then ok "Hermes home: $HERMES_HOME"
  elif [ "$INSTALL_METHOD" = "none" ]; then warn "no Hermes home at $HERMES_HOME; --detect-only, so nothing was installed"; fi
fi

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
# Strix + Hermes: install whatever is missing.
hdr "Strix and Hermes"
ensure_strix   || warn "Strix is not installed; the discovery layer will be unavailable"
ensure_hermes  || warn "Hermes is not installed; the control plane will be unavailable"
# Cairn checkout + backend patch.
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
# Config + directories.
hdr "Configuration"
mkdir -p "$ENGAGEMENTS"
if [ -f "$TRIAD_HOME/.env" ]; then
  ok ".env exists (left untouched)"
else
  cp "$TRIAD_HOME/.env.example" "$TRIAD_HOME/.env"
  chmod 600 "$TRIAD_HOME/.env"
  warn "created .env; fill in your API keys before running a scan"
fi
mkdir -p "$TRIAD_HOME/datas/cairn"
ok "engagements: $ENGAGEMENTS"
# Hermes plugin.
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
  warn "could not validate the plugin with \`hermes plugins doctor\` ; run it yourself once Hermes is on PATH"
fi
# CLI wrapper.
hdr "CLI"
mkdir -p "$BIN_DIR"
cat > "$BIN_DIR/triad" <<EOF
#!/usr/bin/env bash
# Installed by triad install.sh, a thin wrapper so the CLI works from anywhere.
export TRIAD_HOME="\${TRIAD_HOME:-$TRIAD_HOME}"
export CAIRN_BASE_URL="\${CAIRN_BASE_URL:-http://127.0.0.1:8000}"
export TRIAD_WORKDIR="\${TRIAD_WORKDIR:-$ENGAGEMENTS}"
exec python3 "\$TRIAD_HOME/triad.py" "\$@"
EOF
chmod +x "$BIN_DIR/triad"
ok "$BIN_DIR/triad"

case ":$PATH:" in
  *":$BIN_DIR:"*) ok "$BIN_DIR is on PATH" ;;
  *) warn "$BIN_DIR is NOT on PATH; add: export PATH=\"$BIN_DIR:\$PATH\"" ;;
esac
# Done.
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
