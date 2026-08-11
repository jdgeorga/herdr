#!/bin/bash
# Rebase this herdr fork onto upstream master, rebuild it, and optionally push.
#
# This is the fork's replacement for `herdr update`. Upstream's updater
# (src/update.rs) downloads a prebuilt binary from https://herdr.dev/latest.json
# and swaps it in place -- on this fork that silently reverts you to stock herdr
# and loses the jobs sidebar. Source stays untouched by it. Rebasing is a git
# operation, so it lives here instead.
#
# Safe to re-run. Stops before building or pushing if the rebase conflicts.

set -uo pipefail

FORK_BRANCH="${HERDR_FORK_BRANCH:-feat/slurm-jobs-sidebar}"
UPSTREAM_REMOTE="${HERDR_UPSTREAM_REMOTE:-origin}"
FORK_REMOTE="${HERDR_FORK_REMOTE:-fork}"

usage() {
  cat <<'EOF'
Usage: herdr-sync [options]
       herdr-slurm sync [options]

Rebases the fork branch onto upstream master, then rebuilds the release
binary. A backup branch is created before every rebase.

Options:
  --no-build      Rebase only; skip cargo build.
  --no-rebase     Build only; skip fetch and rebase.
  --push          Push the rebased branch to the fork remote with
                  --force-with-lease. Off by default: a rebase rewrites
                  history, so this is opt-in.
  --dry-run       Report what would happen and exit without changing anything.
  -h, --help      Show this help.

Environment:
  HERDR_FORK_BRANCH     branch carrying the fork delta (default feat/slurm-jobs-sidebar)
  HERDR_UPSTREAM_REMOTE remote tracking herdrdev/herdr (default origin)
  HERDR_FORK_REMOTE     remote tracking your own fork (default fork)

Exit status is nonzero if any requested step fails.
EOF
}

die() {
  echo "herdr-sync: $*" >&2
  exit 1
}

note() { echo "==> $*"; }

do_build=1
do_rebase=1
do_push=0
dry_run=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --no-build)  do_build=0 ;;
    --no-rebase) do_rebase=0 ;;
    --push)      do_push=1 ;;
    --dry-run)   dry_run=1 ;;
    -h|--help)   usage; exit 0 ;;
    *)           usage >&2; die "unknown option: $1" ;;
  esac
  shift
done

# The script lives in <repo>/scripts/, but is normally invoked through a symlink
# in ~/.local/bin, so resolve it before deriving the repo root.
self="$(readlink -f "${BASH_SOURCE[0]}")"
repo="$(cd "$(dirname "$self")/.." && pwd)"
cd "$repo" || die "cannot enter $repo"

git rev-parse --git-dir >/dev/null 2>&1 || die "$repo is not a git repository"
git remote get-url "$UPSTREAM_REMOTE" >/dev/null 2>&1 \
  || die "no '$UPSTREAM_REMOTE' remote; set HERDR_UPSTREAM_REMOTE"

current="$(git symbolic-ref --quiet --short HEAD || echo DETACHED)"
[[ "$current" == "$FORK_BRANCH" ]] \
  || die "on '$current', expected '$FORK_BRANCH'; check out the fork branch first"

if [[ $dry_run -eq 1 ]]; then
  note "dry run in $repo"
  git fetch --quiet "$UPSTREAM_REMOTE" || die "fetch failed"
  behind="$(git rev-list --count "HEAD..$UPSTREAM_REMOTE/master")"
  ahead="$(git rev-list --count "$UPSTREAM_REMOTE/master..HEAD")"
  echo "    branch    : $FORK_BRANCH"
  echo "    upstream  : $UPSTREAM_REMOTE/master ($behind new commits to absorb)"
  echo "    fork delta: $ahead commits"
  if [[ "$behind" -gt 0 ]]; then
    echo "    files both sides touched:"
    git diff --name-only "$UPSTREAM_REMOTE/master...HEAD" | sort -u >/tmp/.herdr-sync-fork.$$
    git diff --name-only "HEAD...$UPSTREAM_REMOTE/master" | sort -u >/tmp/.herdr-sync-up.$$
    comm -12 /tmp/.herdr-sync-fork.$$ /tmp/.herdr-sync-up.$$ | sed 's/^/      /'
    rm -f /tmp/.herdr-sync-fork.$$ /tmp/.herdr-sync-up.$$
  fi
  exit 0
fi

if [[ $do_rebase -eq 1 && -n "$(git status --porcelain)" ]]; then
  die "working tree is dirty; commit or stash before rebasing"
fi

if [[ $do_rebase -eq 1 ]]; then
  note "fetching $UPSTREAM_REMOTE"
  git fetch "$UPSTREAM_REMOTE" || die "fetch failed"

  behind="$(git rev-list --count "HEAD..$UPSTREAM_REMOTE/master")"
  if [[ "$behind" -eq 0 ]]; then
    note "already on top of $UPSTREAM_REMOTE/master; nothing to rebase"
  else
    backup="backup/pre-rebase-$(date +%Y%m%d-%H%M)"
    note "backing up $FORK_BRANCH to $backup"
    git branch -f "$backup" "$FORK_BRANCH" || die "could not create $backup"

    note "rebasing onto $UPSTREAM_REMOTE/master ($behind new upstream commits)"
    if ! git rebase "$UPSTREAM_REMOTE/master"; then
      cat >&2 <<EOF

herdr-sync: rebase stopped with conflicts. Nothing was built or pushed.

  Resolve, then:   git add <files> && git rebase --continue
  Or abandon it:   git rebase --abort   (branch is also saved at $backup)

Re-run herdr-sync after the rebase finishes to build.
EOF
      exit 1
    fi
    note "rebase clean"
  fi
fi

if [[ $do_build -eq 1 ]]; then
  [[ -f "$repo/build-env.sh" ]] || die "build-env.sh not found in $repo"
  note "building release binary"
  # shellcheck disable=SC1091
  source "$repo/build-env.sh" || die "could not source build-env.sh"
  cargo build --release || die "build failed; the binary on disk is unchanged"
  built="${CARGO_TARGET_DIR:-$repo/target}/release/herdr"
  note "built $built"
  if [[ -n "${HERDR_SOCKET_PATH:-}" ]]; then
    note "a herdr server is running from the old binary; restart it to pick this up"
  fi
fi

if [[ $do_push -eq 1 ]]; then
  git remote get-url "$FORK_REMOTE" >/dev/null 2>&1 \
    || die "no '$FORK_REMOTE' remote; set HERDR_FORK_REMOTE or add it"
  note "pushing $FORK_BRANCH to $FORK_REMOTE"
  git push --force-with-lease "$FORK_REMOTE" "$FORK_BRANCH" \
    || die "push rejected; fetch the fork remote and reconcile"
else
  echo
  note "not pushed. To publish the rebased branch:"
  echo "      git push --force-with-lease $FORK_REMOTE $FORK_BRANCH"
fi
