#!/bin/bash
# shellcheck shell=bash
#
# Site resolution for the herdr SLURM fork's login-node wrappers.
#
# Every wrapper in site/bin sources this file and then uses the HERDR_SITE_*
# values below instead of hardcoding a cluster's layout. Nothing here is
# Perlmutter-specific; Perlmutter is simply what the probes find when they run
# on Perlmutter.
#
# Resolution order, applied per value, first hit wins:
#
#   1. the variable is already set in the environment
#   2. ~/.config/herdr-slurm/site.env  (untracked, optional)
#   3. a runtime probe
#   4. a built-in default
#
# `herdr-slurm site` prints every resolved value together with which of those
# four rules produced it, so a bad probe is visible rather than silent.

# The knobs a site may need to set. Order is the display order of `herdr site`.
HERDR_SITE_VARS=(
  HERDR_SITE_REPO
  HERDR_SITE_BIN_DIR
  HERDR_SITE_FORK_BIN
  HERDR_SITE_STOCK_BIN
  HERDR_SITE_LOGIN_PREFIX
  HERDR_SITE_LOGIN_PATTERN
  HERDR_SITE_PYTHON
  HERDR_SITE_CGROUP_MODE
  HERDR_SITE_CGROUP_DIR
)

declare -gA HERDR_SITE_SOURCE=()

herdr_site_env_file() {
  printf '%s\n' "${HERDR_SITE_ENV:-$HOME/.config/herdr-slurm/site.env}"
}

# Record a probed or defaulted value without clobbering anything an earlier
# (higher precedence) rule already supplied.
_herdr_site_set() {
  local var="$1" value="$2" source="$3"
  [[ -n "${!var:-}" ]] && return 0
  printf -v "$var" '%s' "$value"
  export "${var?}"
  HERDR_SITE_SOURCE["$var"]="$source"
}

# ---------------------------------------------------------------------------
# Rules 1 and 2: environment, then site.env.
# ---------------------------------------------------------------------------
_herdr_site_load_env_file() {
  local var kv env_file
  local -a preset=()

  for var in "${HERDR_SITE_VARS[@]}"; do
    if [[ -n "${!var:-}" ]]; then
      preset+=("$var=${!var}")
      HERDR_SITE_SOURCE["$var"]='env'
    fi
  done

  env_file="$(herdr_site_env_file)"
  if [[ -r "$env_file" ]]; then
    # shellcheck disable=SC1090  # path is site-chosen by design
    . "$env_file"
    for var in "${HERDR_SITE_VARS[@]}"; do
      [[ -n "${!var:-}" && -z "${HERDR_SITE_SOURCE[$var]:-}" ]] || continue
      HERDR_SITE_SOURCE["$var"]='site.env'
      export "${var?}"
    done
  fi

  # site.env must not be able to override a value the caller set explicitly on
  # the command line or in its own environment; restore those afterwards.
  # bash < 4.4 (CentOS 7 ships 4.2) treats "${arr[@]}" on an EMPTY array as an
  # unbound variable under `set -u`, so this needs the same count guard used
  # elsewhere in this file. Without it every site/bin command aborts at startup.
  if (( ${#preset[@]} > 0 )); then
    for kv in "${preset[@]}"; do
      export "${kv?}"
    done
  fi
}

# ---------------------------------------------------------------------------
# Rule 3: probes.
# ---------------------------------------------------------------------------

# Repo root, found by walking up from this library rather than by being told.
# Works through the ~/.local/bin symlinks the installer creates, because
# BASH_SOURCE is resolved with `pwd -P`.
_herdr_site_probe_repo() {
  local lib_dir
  lib_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)" || return 0
  _herdr_site_set HERDR_SITE_REPO "$(cd -- "$lib_dir/../.." && pwd -P)" \
    'probe: library location'
  _herdr_site_set HERDR_SITE_BIN_DIR "$lib_dir/../bin" 'probe: library location'
  # Normalise the bin dir so remote invocations get a clean absolute path.
  if [[ -d "$HERDR_SITE_BIN_DIR" ]]; then
    HERDR_SITE_BIN_DIR="$(cd -- "$HERDR_SITE_BIN_DIR" && pwd -P)"
    export HERDR_SITE_BIN_DIR
  fi
}

# The fork build. build-env.sh redirects CARGO_TARGET_DIR to $PSCRATCH because
# $HOME is quota-bound here, so prefer that; a site with no redirect gets
# cargo's in-repo default instead.
_herdr_site_probe_fork_bin() {
  if [[ -n "${HERDR_SLURM_BIN:-}" ]]; then
    _herdr_site_set HERDR_SITE_FORK_BIN "$HERDR_SLURM_BIN" 'env: HERDR_SLURM_BIN'
    return 0
  fi
  if [[ -n "${CARGO_TARGET_DIR:-}" && -x "$CARGO_TARGET_DIR/release/herdr" ]]; then
    _herdr_site_set HERDR_SITE_FORK_BIN "$CARGO_TARGET_DIR/release/herdr" \
      'probe: CARGO_TARGET_DIR'
    return 0
  fi
  if [[ -n "${PSCRATCH:-}" && -x "$PSCRATCH/rust/target/release/herdr" ]]; then
    _herdr_site_set HERDR_SITE_FORK_BIN "$PSCRATCH/rust/target/release/herdr" \
      'probe: PSCRATCH build dir'
    return 0
  fi
  _herdr_site_set HERDR_SITE_FORK_BIN \
    "${HERDR_SITE_REPO:-.}/target/release/herdr" 'default: in-repo target dir'
}

_herdr_site_probe_stock_bin() {
  if [[ -n "${HERDR_BIN:-}" ]]; then
    _herdr_site_set HERDR_SITE_STOCK_BIN "$HERDR_BIN" 'env: HERDR_BIN'
    return 0
  fi
  _herdr_site_set HERDR_SITE_STOCK_BIN "$HOME/.local/bin/herdr-bin" \
    'default: ~/.local/bin/herdr-bin'
}

# Login-node naming. Login nodes are not SLURM compute nodes, so scontrol and
# sinfo cannot be asked; the current hostname is the only reliable evidence.
# login03 -> prefix "login"; ln01 -> prefix "ln"; a digitless hostname yields
# itself, which keeps single-host machines working without a special case.
_herdr_site_probe_login_prefix() {
  local host prefix
  host="$(hostname -s 2>/dev/null || printf 'localhost')"
  prefix="${host%%[0-9]*}"
  if [[ -z "$prefix" || "$prefix" == "$host" ]]; then
    _herdr_site_set HERDR_SITE_LOGIN_PREFIX "$host" \
      "probe: hostname $host (no numeric suffix)"
    _herdr_site_set HERDR_SITE_LOGIN_PATTERN "^${host}\$" \
      "probe: hostname $host (no numeric suffix)"
    return 0
  fi
  _herdr_site_set HERDR_SITE_LOGIN_PREFIX "$prefix" "probe: hostname $host"
  _herdr_site_set HERDR_SITE_LOGIN_PATTERN "^${prefix}[0-9]+\$" \
    "probe: hostname $host"
}

# The jobs provider needs >= 3.11: bare python3 is 3.6 on Perlmutter login
# nodes, and an ungated fallback would fail deep inside the provider instead of
# here, where the error can name the actual problem.
_herdr_site_probe_python() {
  local candidate found
  for candidate in \
    /usr/bin/python3.13 /usr/bin/python3.12 /usr/bin/python3.11 \
    python3.13 python3.12 python3.11 python3
  do
    found="$(command -v "$candidate" 2>/dev/null)" || continue
    [[ -x "$found" ]] || continue
    "$found" -c 'import sys; raise SystemExit(0 if sys.version_info >= (3, 11) else 1)' \
      >/dev/null 2>&1 || continue
    _herdr_site_set HERDR_SITE_PYTHON "$found" \
      "probe: $("$found" -c 'import sys; print("python %d.%d.%d" % sys.version_info[:3])' 2>/dev/null)"
    return 0
  done
  _herdr_site_set HERDR_SITE_PYTHON '' 'unresolved: no python >= 3.11 found'
}

# cgroup v2 exposes memory.current/pids.current under a unified hierarchy;
# v1 splits controllers into per-subsystem trees with different filenames.
_herdr_site_probe_cgroup() {
  local uid
  uid="$(id -u)"
  if [[ -e /sys/fs/cgroup/cgroup.controllers ]]; then
    _herdr_site_set HERDR_SITE_CGROUP_MODE v2 'probe: cgroup.controllers present'
    _herdr_site_set HERDR_SITE_CGROUP_DIR \
      "/sys/fs/cgroup/user.slice/user-${uid}.slice" 'probe: cgroup v2 layout'
    return 0
  fi
  _herdr_site_set HERDR_SITE_CGROUP_MODE v1 'probe: no cgroup.controllers'
  _herdr_site_set HERDR_SITE_CGROUP_DIR \
    "/sys/fs/cgroup/memory/user.slice/user-${uid}.slice" 'probe: cgroup v1 layout'
}

herdr_site_resolve() {
  _herdr_site_load_env_file
  _herdr_site_probe_repo
  _herdr_site_probe_fork_bin
  _herdr_site_probe_stock_bin
  _herdr_site_probe_login_prefix
  _herdr_site_probe_python
  _herdr_site_probe_cgroup
  HERDR_SITE_RESOLVED=1
  export HERDR_SITE_RESOLVED
}

# ---------------------------------------------------------------------------
# Host helpers, shared by ls / health / cleanup / reap.
# ---------------------------------------------------------------------------

herdr_site_is_login_host() {
  [[ "${1:-}" =~ ${HERDR_SITE_LOGIN_PATTERN} ]]
}

# Accept a hostname argument if it looks like a login node of this cluster.
# Silently ignoring a non-matching name preserves the old add_host contract.
herdr_site_add_host() {
  local host="${1:-}"
  herdr_site_is_login_host "$host" || return 0
  HERDR_SITE_SEEN["$host"]=1
}

# Nodes this user has actually used, from saved sessions and spinner records.
# The current node is always included so a first run on a fresh cluster still
# reports something.
herdr_site_discover_hosts() {
  local path base name prefix
  prefix="$HERDR_SITE_LOGIN_PREFIX"
  for path in "$HOME/.config/herdr/sessions"/herdr-"$prefix"* \
              "$HOME/.config/herdr/sessions"/slurm-"$prefix"*; do
    [[ -e "$path" ]] || continue
    base="${path##*/}"
    herdr_site_add_host "${base#*-}"
  done
  for path in "$HOME/.config/herdr"/herdr-spinner."$prefix"*.pid \
              "$HOME/.config/herdr"/herdr-spinner."$prefix"*.log; do
    [[ -e "$path" ]] || continue
    name="${path##*/herdr-spinner.}"
    herdr_site_add_host "${name%%.*}"
  done
  herdr_site_add_host "$(hostname -s)"
}

# Sorted host list from HERDR_SITE_SEEN into the HERDR_SITE_HOSTS array.
herdr_site_hosts() {
  if (( ${#HERDR_SITE_SEEN[@]} == 0 )); then
    herdr_site_discover_hosts
  fi
  # Same bash < 4.4 caveat, plus: printf '%s\n' with no argument still emits one
  # blank line, which mapfile turns into a phantom empty host and pids[""] then
  # fails with "bad array subscript". Guard on the count instead.
  if (( ${#HERDR_SITE_SEEN[@]} > 0 )); then
    mapfile -t HERDR_SITE_HOSTS < <(printf '%s\n' "${!HERDR_SITE_SEEN[@]}" | sort -V)
  else
    HERDR_SITE_HOSTS=()
  fi
}

# Absolute path to a sibling wrapper. Used both for local dispatch and for the
# remote half of a fan-out, which is why it must not go through ~/.local/bin:
# the repo copy is the one guaranteed to match this script's own version.
herdr_site_wrapper() {
  printf '%s\n' "$HERDR_SITE_BIN_DIR/herdr-${1:?wrapper name required}"
}

# Run `<wrapper> --local <extra args>` on every host in HERDR_SITE_HOSTS at the
# same time, leaving stdout per host in $result_dir/<host>.out and the hosts
# that failed in HERDR_SITE_UNAVAILABLE.
#
# Concurrency is the point: one wedged node used to cost one SSH timeout per
# node because the fan-out was sequential.
herdr_site_fanout() {
  local timeout_s="$1" result_dir="$2" wrapper="$3"
  shift 3
  local host remote
  local -A pids=()

  remote="$(herdr_site_wrapper "$wrapper") --local"
  if (( $# > 0 )); then
    remote+=" $*"
  fi

  HERDR_SITE_UNAVAILABLE=()
  for host in "${HERDR_SITE_HOSTS[@]}"; do
    (
      timeout "$timeout_s" ssh -o BatchMode=yes -o ConnectTimeout=3 "$host" \
        "$remote" >"$result_dir/$host.out" 2>/dev/null
    ) &
    pids["$host"]=$!
  done
  for host in "${HERDR_SITE_HOSTS[@]}"; do
    wait "${pids[$host]}" || HERDR_SITE_UNAVAILABLE+=("$host")
  done
}

# ---------------------------------------------------------------------------
# Formatting and cgroup reads, shared by health.
# ---------------------------------------------------------------------------

herdr_site_human_bytes() {
  local bytes="${1:-0}"
  if [[ "$bytes" == "max" || ! "$bytes" =~ ^[0-9]+$ ]]; then
    printf 'unlimited'
  elif (( bytes >= 1073741824 )); then
    awk -v n="$bytes" 'BEGIN {printf "%.1fG", n/1073741824}'
  elif (( bytes >= 1048576 )); then
    awk -v n="$bytes" 'BEGIN {printf "%.0fM", n/1048576}'
  else
    awk -v n="$bytes" 'BEGIN {printf "%.0fK", n/1024}'
  fi
}

# Read one logical cgroup metric, hiding the v1/v2 filename differences.
# Unreadable metrics report the neutral value rather than failing: a site whose
# cgroup layout we guessed wrong should still get the rest of the health table.
herdr_site_cgroup_read() {
  local what="$1" dir="$HERDR_SITE_CGROUP_DIR" file fallback
  case "$what:$HERDR_SITE_CGROUP_MODE" in
    memory:v2)     file=memory.current            ; fallback=0 ;;
    memory:v1)     file=memory.usage_in_bytes      ; fallback=0 ;;
    memory_max:v2) file=memory.max                 ; fallback=max ;;
    memory_max:v1) file=memory.limit_in_bytes      ; fallback=max ;;
    tasks:v2)      file=pids.current               ; fallback=0 ;;
    tasks:v1)      file=pids.current               ; fallback=0 ;;
    tasks_max:v2)  file=pids.max                   ; fallback=max ;;
    tasks_max:v1)  file=pids.max                   ; fallback=max ;;
    *) printf '0\n'; return 0 ;;
  esac
  cat "$dir/$file" 2>/dev/null || printf '%s\n' "$fallback"
}

# Cumulative OOM kills. v2 keeps a counter in memory.events; v1 has no
# equivalent counter, so report 0 rather than inventing a number.
herdr_site_cgroup_oom_kills() {
  if [[ "$HERDR_SITE_CGROUP_MODE" != v2 ]]; then
    printf '0\n'
    return 0
  fi
  awk '$1 == "oom_kill" {found=1; print $2} END {if (!found) print 0}' \
    "$HERDR_SITE_CGROUP_DIR/memory.events" 2>/dev/null || printf '0\n'
}

# ---------------------------------------------------------------------------
# `herdr site` report.
# ---------------------------------------------------------------------------

herdr_site_report() {
  local var value source env_file
  env_file="$(herdr_site_env_file)"

  printf '%-22s %-52s %s\n' VALUE RESOLVED FROM
  for var in "${HERDR_SITE_VARS[@]}"; do
    value="${!var:-}"
    source="${HERDR_SITE_SOURCE[$var]:-default}"
    printf '%-22s %-52s %s\n' "${var#HERDR_SITE_}" "${value:-<unset>}" "$source"
  done

  printf '\n'
  if [[ -r "$env_file" ]]; then
    printf 'site.env  %s (in use)\n' "$env_file"
  else
    printf 'site.env  %s (absent; create it to override any value above)\n' "$env_file"
  fi

  local -a warnings=()
  [[ -n "${HERDR_SITE_PYTHON:-}" ]] || \
    warnings+=('no python >= 3.11 found; the SLURM jobs sidebar will not run')
  [[ -x "${HERDR_SITE_FORK_BIN:-}" ]] || \
    warnings+=("fork binary missing at $HERDR_SITE_FORK_BIN; build it with 'source ./build-env.sh && cargo build --release'")
  [[ -d "${HERDR_SITE_CGROUP_DIR:-}" ]] || \
    warnings+=("cgroup dir $HERDR_SITE_CGROUP_DIR not readable; 'herdr health' will show zeroed memory")
  command -v squeue >/dev/null 2>&1 || \
    warnings+=('squeue not on PATH; the jobs sidebar has no SLURM to query')

  if (( ${#warnings[@]} > 0 )); then
    printf '\n'
    printf 'warning: %s\n' "${warnings[@]}"
  fi
}

declare -gA HERDR_SITE_SEEN=()
declare -ga HERDR_SITE_HOSTS=()
declare -ga HERDR_SITE_UNAVAILABLE=()

# Resolve once per process. A wrapper that dispatches to a sibling exports its
# resolved values, so the sibling skips the probes but must still get the
# function definitions and array declarations above -- which is why this guard
# sits here and not at the top of the file.
if [[ -z "${HERDR_SITE_RESOLVED:-}" ]]; then
  herdr_site_resolve
fi
