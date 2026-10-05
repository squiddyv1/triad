#!/usr/bin/env bash
#
# Triad installer: Docker + Strix (discovery) + Cairn (exploitation).
#
# The normal flow is Strix -> Cairn and needs no agent framework. Hermes is an
# optional extra layer and is never installed unless you ask for it.
#
#   ./install.sh                 install Docker, Strix and Cairn (Hermes only if present)
#   ./install.sh --with-hermes   also install Hermes if it is missing
#   ./install.sh --no-docker     never install Docker, only report it
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
PLUGIN_SRC="$TRIAD_HOME/plugin"
PLUGIN_DST="$HERMES_HOME/plugins/triad"

# Official installers, used only as a fallback when no package manager is present.
STRIX_INSTALL_URL="https://strix.ai/install"
HERMES_INSTALL_URL="https://hermes-agent.nousresearch.com/install.sh"
# Docker publishes this convenience script for the same purpose. Any override
# here is env-only, so a corporate mirror can be dropped in.
DOCKER_INSTALL_URL="${DOCKER_INSTALL_URL:-https://get.docker.com}"
DOCKER_BIN="${DOCKER_BIN:-docker}"

MODE="install"
# Installing the missing layers is the default; --detect-only turns it off.
INSTALL_METHOD="${TRIAD_INSTALL_METHOD:-official}"
# Hermes is optional: only installed when explicitly requested.
WITH_HERMES=0
# Docker is installed by default, because Strix's sandbox and Cairn's container
# mode both need it; --no-docker (or DOCKER_INSTALL_METHOD=none) opts out.
DOCKER_INSTALL_METHOD="${DOCKER_INSTALL_METHOD:-}"
for arg in "$@"; do
  case "$arg" in
    --check)        MODE="check" ;;
    --uninstall)    MODE="uninstall" ;;
    --detect-only|--no-deps)
                    INSTALL_METHOD="none" ;;
    --no-docker)
                    DOCKER_INSTALL_METHOD="none" ;;
    --with-hermes|--all)
                    WITH_HERMES=1 ;;
    --with-strix)
                    : ;;   # Strix is part of the normal flow and installs anyway
    -h|--help)      sed -n '2,16p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *)              echo "unknown option: $arg (try --help)" >&2; exit 2 ;;
  esac
done

ok()   { printf '  \033[32m✓\033[0m %s\n' "$*"; }
warn() { printf '  \033[33m!\033[0m %s\n' "$*"; }
err()  { printf '  \033[31m✗\033[0m %s\n' "$*" >&2; }
hdr()  { printf '\n\033[1m%s\033[0m\n' "$*"; }
have() { command -v "$1" >/dev/null 2>&1; }
strix_present()  { have strix  || [ -x "$HOME/.strix/bin/strix" ]; }
hermes_present() { have hermes || [ -x "$HOME/.local/bin/hermes" ]; }
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

# Docker. The one prerequisite the harness cannot work around: Strix runs its
# agent in a sandbox container, and Cairn's default mode is compose. Installed by
# default; --no-docker or DOCKER_INSTALL_METHOD=none only reports it.
docker_present()   { have "$DOCKER_BIN"; }
docker_daemon_up() { docker_present && "$DOCKER_BIN" info >/dev/null 2>&1; }

# Root directly, else sudo. Non-interactive runs fail fast rather than hanging.
as_root() {
  if [ "$(id -u)" = 0 ]; then "$@"
  elif have sudo; then sudo "$@"
  else err "need root for '$1', and sudo is not available"; return 127
  fi
}

# Docker publishes packages only for the distros below. On a derivative (Kali,
# Parrot, Mint, Pop) get.docker.com takes its "*)" branch: it maps the distro to
# debian but keeps VERSION_ID as the version, so it asks for `debian kali-rolling`,
# which has no Release file. That also leaves an apt source behind which breaks
# every later apt call, so the route is picked up front.
DOCKER_OFFICIAL_IDS="debian ubuntu raspbian fedora centos rhel rocky almalinux amzn sles opensuse-leap opensuse-tumbleweed"
DOCKER_APT_LIST="${DOCKER_APT_LIST:-/etc/apt/sources.list.d/docker.list}"

os_release_id() {
  local f="${OS_RELEASE_FILE:-/etc/os-release}"
  if [ ! -r "$f" ]; then return 1; fi
  ( . "$f" 2>/dev/null; echo "${ID:-}" )
}

docker_official_ok() {
  local id
  id="$(os_release_id)" || return 1
  if [ -z "$id" ]; then return 1; fi
  case " $DOCKER_OFFICIAL_IDS " in
    *" $id "*) return 0 ;;
    *)         return 1 ;;
  esac
}

# An existing docker.list whose suite Docker never published breaks apt with
# "does not have a Release file". Remove exactly that entry, and only that.
docker_apt_repair() {
  if [ ! -r "$DOCKER_APT_LIST" ]; then return 0; fi
  local suite
  suite="$(awk '{for(i=1;i<=NF;i++) if ($i ~ /^https?:/) {print $(i+1); exit}}' "$DOCKER_APT_LIST")"
  case "$suite" in
    *kali*|*parrot*|*mint*|*pop*|*elementary*)
      warn "removing an unusable Docker apt source: $DOCKER_APT_LIST (suite '$suite' does not exist)"
      as_root rm -f "$DOCKER_APT_LIST" || return 1
      as_root apt-get update -qq >/dev/null 2>&1 || true
      ok "apt works again; using the distro's own packages instead"
      ;;
  esac
  return 0
}

# Distro route, which is the documented one on derivatives. Package names differ
# per family, so each case is explicit. docker.io is the engine on Debian-likes:
# Kali's own `docker` package is unrelated to containers, so it is never used.
pkg_docker() {
  if have apt-get; then
    as_root apt-get update -qq
    echo "    apt-get install docker.io"
    as_root apt-get install -y docker.io || return 1
    # compose v2, whatever this release calls it. Optional here: ensure_compose
    # retries, and Cairn's local mode works without it.
    as_root apt-get install -y docker-compose-v2 >/dev/null 2>&1 \
      || as_root apt-get install -y docker-compose-plugin >/dev/null 2>&1 \
      || as_root apt-get install -y docker-compose >/dev/null 2>&1 || true
    return 0
  fi
  if have dnf; then
    echo "    dnf install moby-engine docker-compose-plugin"
    as_root dnf install -y moby-engine docker-compose-plugin || return 1
    return 0
  fi
  if have yum; then
    echo "    yum install docker docker-compose-plugin"
    as_root yum install -y docker docker-compose-plugin || return 1
    return 0
  fi
  if have zypper; then
    echo "    zypper install docker docker-compose"
    as_root zypper --non-interactive install docker docker-compose || return 1
    return 0
  fi
  if have pacman; then
    echo "    pacman -S docker docker-compose"
    as_root pacman -S --noconfirm docker docker-compose || return 1
    return 0
  fi
  return 127
}

# The vendor script runs as root. Fetched to a temp file and hashed first, same
# as every other layer, so nothing is piped blindly into a shell.
docker_script_install() {
  local tmp
  tmp="$(mktemp -t triad-docker.XXXXXX.sh)"
  curl -fsSL "$DOCKER_INSTALL_URL" -o "$tmp" \
    || { err "could not fetch $DOCKER_INSTALL_URL"; return 1; }
  echo "    fetched $DOCKER_INSTALL_URL"
  echo "    -> $tmp  ($(wc -c <"$tmp") bytes, sha256 $(sha256sum "$tmp" | cut -c1-32)…)"
  as_root bash "$tmp" || { err "the Docker installer exited non-zero (script kept at $tmp)"; return 1; }
  rm -f "$tmp"
}

start_docker_daemon() {
  if have systemctl; then
    as_root systemctl enable --now docker >/dev/null 2>&1 || return 1
  elif have service; then
    as_root service docker start >/dev/null 2>&1 || return 1
  else
    return 1
  fi
  local i
  for i in 1 2 3 4 5 6 7 8 9 10; do
    docker_daemon_up && return 0
    sleep 1
  done
  return 1
}

# cairn-server is a compose service, so the v2 plugin is part of a working install.
ensure_compose() {
  if "$DOCKER_BIN" compose version >/dev/null 2>&1; then
    ok "docker compose v2 present"
    return 0
  fi
  warn "the docker compose v2 plugin is missing; cairn-server needs it"
  if have apt-get; then
    as_root apt-get install -y docker-compose-v2 >/dev/null 2>&1 \
      || as_root apt-get install -y docker-compose-plugin >/dev/null 2>&1 || true
  elif have dnf; then
    as_root dnf install -y docker-compose-plugin >/dev/null 2>&1 || true
  elif have yum; then
    as_root yum install -y docker-compose-plugin >/dev/null 2>&1 || true
  elif have zypper; then
    as_root zypper --non-interactive install docker-compose >/dev/null 2>&1 || true
  elif have pacman; then
    as_root pacman -S --noconfirm docker-compose >/dev/null 2>&1 || true
  fi
  if "$DOCKER_BIN" compose version >/dev/null 2>&1; then
    ok "docker compose v2 installed"
  else
    warn "install the compose plugin yourself: https://docs.docker.com/compose/install/"
    return 1
  fi
}

ensure_docker() {
  local method="${DOCKER_INSTALL_METHOD:-$INSTALL_METHOD}"
  if docker_daemon_up; then
    ok "docker is running ($("$DOCKER_BIN" --version 2>/dev/null | head -1))"
  elif docker_present; then
    warn "docker is installed but the daemon is not reachable; starting it"
    start_docker_daemon || warn "could not start the docker daemon automatically"
  else
    local use_official=0
    case "$method" in
      official)
        if docker_official_ok; then
          use_official=1
        else
          warn "Docker publishes no packages for '$(os_release_id)'"
          warn "  its installer would request a suite that does not exist, so the"
          warn "  distro's own docker.io is used instead (the documented route there)"
          docker_apt_repair || true
        fi
        ;;
      pkg)  : ;;
      none)
        warn "docker is missing and installation is disabled ($method)"
        warn "  install it yourself: https://docs.docker.com/engine/install/"
        return 1
        ;;
      *) err "unknown install method '$method' (use official, pkg or none)"; return 2 ;;
    esac

    if [ "$use_official" = 1 ]; then
      echo "  installing Docker using the command its docs publish"
      # Remember whether the apt source existed, so a failed attempt can clean up
      # after itself without touching a source the user already had.
      local list_before=0
      if [ -e "$DOCKER_APT_LIST" ]; then list_before=1; fi
      if ! docker_script_install; then
        if [ "$list_before" = 0 ] && [ -e "$DOCKER_APT_LIST" ]; then
          warn "removing the apt source the failed install left behind ($DOCKER_APT_LIST)"
          as_root rm -f "$DOCKER_APT_LIST" || true
          as_root apt-get update -qq >/dev/null 2>&1 || true
        fi
        warn "the official route failed; falling back to the distro's own packages"
        pkg_docker || { err "Docker install failed on both routes"; return 1; }
      fi
    else
      echo "  installing Docker from the distro package manager"
      if ! pkg_docker; then
        warn "the package manager route failed; falling back to the vendor script"
        if ! docker_script_install; then
          docker_apt_repair || true
          err "Docker install failed on both routes"
          return 1
        fi
      fi
    fi
    start_docker_daemon \
      || warn "Docker is installed but the daemon did not come up; start it before scanning"
  fi

  # Talking to the socket needs group membership for anyone who is not root.
  if [ "$(id -u)" != 0 ] && docker_present \
     && ! "$DOCKER_BIN" info >/dev/null 2>&1 \
     && ! id -nG 2>/dev/null | tr ' ' '\n' | grep -qx docker; then
    if as_root usermod -aG docker "$(id -un)" 2>/dev/null; then
      ok "added $(id -un) to the docker group"
      warn "log out and back in (or run 'newgrp docker') before docker works without sudo"
    fi
  fi

  ensure_compose || true   # optional: local mode works without it
  return 0
}

ensure_strix() {
  if strix_present; then ok "strix already installed"; return 0; fi
  local rc=0
  install_layer strix-agent "$STRIX_INSTALL_URL" \
                "${STRIX_INSTALL_METHOD:-$INSTALL_METHOD}" strix Strix || rc=$?
  echo "    note: Strix needs Docker; its sandbox image is pulled on the first scan"
  return "$rc"   # must not be clobbered by the echo above
}

ensure_hermes() {
  if hermes_present; then ok "hermes already installed"; return 0; fi
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
  warn "Strix and Hermes are separate installs; this never touches them"
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

if docker_daemon_up; then ok "docker (daemon reachable)"
elif docker_present; then warn "docker installed but the daemon is not reachable; will try to start it"
elif [ "$MODE" = "check" ] || [ "${DOCKER_INSTALL_METHOD:-$INSTALL_METHOD}" = "none" ]; then
  warn "docker not found; Strix needs it, Cairn local mode does not"
else
  warn "docker not found; will install it (${DOCKER_INSTALL_METHOD:-$INSTALL_METHOD})"
fi

if have uv; then ok "uv $(uv --version 2>/dev/null | awk '{print $2}')"
elif have pipx; then ok "pipx (no uv)"
else warn "neither uv nor pipx; Cairn needs uv: https://docs.astral.sh/uv/getting-started/installation/"; fi

if [ "$MODE" = "check" ]; then
  refresh_path
  if strix_present; then ok "strix present"; else warn "strix not found"; fi
  # Hermes is optional: its absence is not an install problem.
  if hermes_present; then ok "hermes present (optional layer)"
  else warn "hermes not installed; the optional control plane is unavailable"; fi
else
  # Reported here; installed a few lines below, once the hard failures clear.
  if strix_present; then ok "strix present"
  else warn "strix missing; will install it ($INSTALL_METHOD)"; fi
  if hermes_present; then ok "hermes present (optional layer)"
  elif [ "$WITH_HERMES" = 1 ]; then warn "hermes missing; --with-hermes, will install it ($INSTALL_METHOD)"
  else warn "hermes not installed; optional, pass --with-hermes to add it"; fi
  if [ -d "$HERMES_HOME" ]; then ok "Hermes home: $HERMES_HOME"
  elif [ "$INSTALL_METHOD" = "none" ]; then warn "no Hermes home at $HERMES_HOME"; fi
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
  # The plugin link only matters when Hermes is installed; without it, skipping
  # the link is correct rather than a failure.
  if hermes_present; then
    if [ -e "$PLUGIN_DST" ]; then ok "Hermes plugin linked at $PLUGIN_DST"
    else err "Hermes plugin not linked (Hermes is present)"; FAIL=1; fi
  else
    ok "Hermes absent; plugin link not required"
  fi
  if [ -x "$BIN_DIR/triad" ]; then ok "triad CLI at $BIN_DIR/triad"; else err "triad CLI missing at $BIN_DIR/triad"; FAIL=1; fi
  if docker_daemon_up; then ok "docker daemon reachable"
  else warn "docker daemon not reachable (Strix needs it; Cairn local mode does not)"; fi
  if "$DOCKER_BIN" compose version >/dev/null 2>&1; then ok "docker compose v2 available"
  else warn "docker compose v2 missing (cairn-server needs it)"; fi
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
#
# Docker first: Strix's sandbox and the Cairn server both need it, so leaving it
# until last would install layers that cannot run.
hdr "Docker"
ensure_docker || warn "docker is not usable; Strix will not run until it is"
#
# Strix: install it if missing. It is the discovery layer, so the normal
# Strix -> Cairn flow depends on it.
hdr "Strix (discovery layer)"
ensure_strix   || warn "Strix is not installed; the discovery layer will be unavailable"
# Hermes: optional, and only touched when asked for. Rather than a no-op flag,
# this is the one place the control plane gets installed.
if [ "$WITH_HERMES" = 1 ] && [ "$INSTALL_METHOD" != "none" ]; then
  hdr "Hermes (optional control plane)"
  ensure_hermes || warn "Hermes is not installed; the optional control plane will be unavailable"
fi
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
# Hermes plugin, only when Hermes is actually present. Without Hermes this
# section is skipped entirely: the `triad` CLI drives the same code directly.
if hermes_present; then
  hdr "Hermes plugin (optional)"
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
else
  hdr "Hermes plugin (skipped)"
  ok "Hermes is not installed; nothing to wire. The CLI is the normal entry point."
  ok "add it later with: ./install.sh --with-hermes"
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
  1. Set it up:                   triad setup
                                  prompts for API keys, writes .env, then starts
                                  Cairn and the dispatcher and waits for health
  2. Run an engagement:           triad engage --title ACME --target https://app.example \\
                                       --goal "conclude or rule out every finding in scope" \\
                                       --roe $TRIAD_HOME/contracts/roe-instructions.md
  3. Later:                       triad status | triad up | triad down
EOF
if hermes_present; then
  cat <<EOF
  4. Optional: restart Hermes so the plugin + MCP tools load, then talk to it.
EOF
else
  cat <<EOF
  4. Hermes is not installed and is not needed. To add the optional control
     plane later: ./install.sh --with-hermes
EOF
fi
cat <<EOF

  Verify at any time:             ./install.sh --check
  Full walkthrough:               $TRIAD_HOME/README.md
  Design + operations notes:      $TRIAD_HOME/ARCHITECTURE.md
EOF
