#!/bin/bash
# Install the login-node wrappers and config for the herdr SLURM fork.
#
# Everything is symlinked, so the repo checkout stays the single source of truth
# and a live file can never silently drift from the tracked one. The two
# exceptions are called out below: session-template.json is seeded once because
# it is per-user data, and site.env is never touched because it is per-cluster
# override data.
#
# This script does NOT build the binary, edit your shell rc, or start anything.
# It links files, seeds the template, and reports what it resolved.
#
#   ./site/install.sh              install or repair
#   ./site/install.sh --dry-run    show what would change
#   ./site/install.sh --uninstall  remove links, restore any backups
#   ./site/install.sh --link-herdr also point `herdr` at the fork

set -euo pipefail

REPO="$(cd -- "$(dirname -- "$(readlink -f -- "${BASH_SOURCE[0]}")")/.." && pwd -P)"
BIN_DIR="$HOME/.local/bin"
CONF_DIR="$HOME/.config/herdr"
SLURM_CONF_DIR="$HOME/.config/herdr-slurm"
BACKUP_SUFFIX='.bak.pre-site-install'

dry_run=0
uninstall=0
link_herdr=0
force=0

usage() {
  sed -n '2,18p' "$(readlink -f -- "${BASH_SOURCE[0]}")" | sed 's/^# \{0,1\}//'
}

while (( $# > 0 )); do
  case "$1" in
    --dry-run) dry_run=1 ;;
    --uninstall) uninstall=1 ;;
    --link-herdr) link_herdr=1 ;;
    --force) force=1 ;;
    --help|-h) usage; exit 0 ;;
    *) echo "install.sh: unknown argument: $1" >&2; usage >&2; exit 2 ;;
  esac
  shift
done

say() { printf '  %-8s %s\n' "$1" "$2"; }

# ---------------------------------------------------------------------------
# The link set. "source-relative-to-repo|absolute destination".
# ---------------------------------------------------------------------------
links=(
  "site/bin/herdr-slurm|$BIN_DIR/herdr-slurm"
  "site/bin/herdr-stock|$BIN_DIR/herdr-stock"
  "site/bin/herdr-health|$BIN_DIR/herdr-health"
  "site/bin/herdr-ls|$BIN_DIR/herdr-ls"
  "site/bin/herdr-cleanup|$BIN_DIR/herdr-cleanup"
  "site/bin/herdr-reap|$BIN_DIR/herdr-reap"
  "site/bin/herdr-slurm-cleanup|$BIN_DIR/herdr-slurm-cleanup"
  "scripts/herdr-sync.sh|$BIN_DIR/herdr-sync"
  "site/config/scripts/herdr-jobs|$CONF_DIR/scripts/herdr-jobs"
  "scripts/herdr-jobs.py|$CONF_DIR/scripts/herdr-jobs.py"
  "site/config/scripts/herdr-spinner|$CONF_DIR/scripts/herdr-spinner"
  "site/config/scripts/herdr-spinner.py|$CONF_DIR/scripts/herdr-spinner.py"
  "site/config/scripts/orphan-scan.py|$CONF_DIR/scripts/orphan-scan.py"
  "site/config/config.toml|$SLURM_CONF_DIR/config.toml"
)

if (( link_herdr )); then
  links+=("site/bin/herdr-slurm|$BIN_DIR/herdr")
fi

seed_src="$REPO/site/config/session-template.json"
seed_dest="$SLURM_CONF_DIR/session-template.json"

# ---------------------------------------------------------------------------
# Uninstall.
# ---------------------------------------------------------------------------
if (( uninstall )); then
  echo "Removing herdr site links (repo: $REPO)"
  for entry in "${links[@]}"; do
    dest="${entry#*|}"
    if [[ -L "$dest" ]]; then
      target="$(readlink -f -- "$dest" 2>/dev/null || true)"
      # Only ever remove a link this repo owns; never a user's own symlink.
      if [[ "$target" == "$REPO"/* ]]; then
        (( dry_run )) || rm -f "$dest"
        say unlink "$dest"
      else
        say skip "$dest (points outside the repo: $target)"
      fi
    elif [[ -e "$dest" ]]; then
      say skip "$dest (not a symlink)"
    fi
    if [[ -e "$dest$BACKUP_SUFFIX" ]]; then
      (( dry_run )) || mv "$dest$BACKUP_SUFFIX" "$dest"
      say restore "$dest (from $BACKUP_SUFFIX)"
    fi
  done
  say keep "$seed_dest (your session template; delete it yourself if unwanted)"
  say keep "$SLURM_CONF_DIR/site.env (your cluster overrides, if any)"
  (( dry_run )) && echo "(dry run: nothing was changed)"
  exit 0
fi

# ---------------------------------------------------------------------------
# Preflight: the repo must live somewhere every login node can see, because the
# wrappers ssh to other nodes and run the repo copy of themselves there.
# ---------------------------------------------------------------------------
fs_type="$(stat -f -c %T "$REPO" 2>/dev/null || echo unknown)"
case "$fs_type" in
  tmpfs|ramfs|devtmpfs)
    echo "install.sh: $REPO is on $fs_type, which is node-local." >&2
    echo "  The multi-node wrappers ssh to other login nodes and run the repo copy" >&2
    echo "  of themselves there, so the checkout must be on shared storage." >&2
    (( force )) || { echo "  Re-run with --force to install anyway." >&2; exit 1; }
    ;;
  lustre|gpfs|nfs|nfs4|beegfs|panfs|cvfs|fuseblk)
    ;;
  *)
    echo "install.sh: note: $REPO is on '$fs_type'; assuming it is shared across" >&2
    echo "  login nodes. If it is not, multi-node commands will fail on other nodes." >&2
    ;;
esac

# ---------------------------------------------------------------------------
# Install.
# ---------------------------------------------------------------------------
echo "Installing herdr site files (repo: $REPO)"
for dir in "$BIN_DIR" "$CONF_DIR/scripts" "$SLURM_CONF_DIR"; do
  (( dry_run )) || mkdir -p "$dir"
done

for entry in "${links[@]}"; do
  src="$REPO/${entry%|*}"
  dest="${entry#*|}"

  if [[ ! -e "$src" ]]; then
    echo "install.sh: missing source file: $src" >&2
    exit 1
  fi

  if [[ -L "$dest" ]]; then
    if [[ "$(readlink -f -- "$dest" 2>/dev/null || true)" == "$(readlink -f -- "$src")" ]]; then
      say ok "$dest"
      continue
    fi
    (( dry_run )) || ln -sfn "$src" "$dest"
    say relink "$dest"
    continue
  fi

  if [[ -e "$dest" ]]; then
    # A real file here is the user's pre-existing copy. Keep it; a lost custom
    # config is far more expensive than a stale backup file.
    if [[ -e "$dest$BACKUP_SUFFIX" ]]; then
      echo "install.sh: refusing to overwrite $dest" >&2
      echo "  a backup already exists at $dest$BACKUP_SUFFIX" >&2
      echo "  move or remove one of them, then re-run" >&2
      exit 1
    fi
    (( dry_run )) || mv "$dest" "$dest$BACKUP_SUFFIX"
    (( dry_run )) || ln -sfn "$src" "$dest"
    say backup "$dest -> $(basename -- "$dest")$BACKUP_SUFFIX, then linked"
    continue
  fi

  (( dry_run )) || ln -sfn "$src" "$dest"
  say link "$dest"
done

# Seed the session template once. It holds working directories, so it is user
# data, not tracked config: never overwrite an existing one.
if [[ -e "$seed_dest" ]]; then
  say ok "$seed_dest (kept)"
else
  workdir="${SCRATCH:-$HOME}"
  if (( ! dry_run )); then
    sed "s|@HERDR_SITE_WORKDIR@|$workdir|g" "$seed_src" >"$seed_dest"
  fi
  say seed "$seed_dest (working dir: $workdir)"
fi

# ---------------------------------------------------------------------------
# Report what the wrappers will actually use.
# ---------------------------------------------------------------------------
echo
# shellcheck source=lib/herdr-site.sh
. "$REPO/site/lib/herdr-site.sh"
herdr_site_report

echo
if (( dry_run )); then
  echo "(dry run: nothing was changed)"
  exit 0
fi

if [[ ! -x "$HERDR_SITE_FORK_BIN" ]]; then
  echo "next: build the fork"
  echo "  cd $REPO && source ./build-env.sh && cargo build --release -j 8"
else
  echo "next: run 'herdr-slurm' (or 'herdr' if you installed with --link-herdr)"
fi
if (( ! link_herdr )) && [[ ! -e "$BIN_DIR/herdr" ]]; then
  echo "note: 'herdr' is not installed; re-run with --link-herdr to point it at the fork"
fi
