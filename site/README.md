# Login-node wrappers and config

This directory holds everything needed to reproduce the fork's working setup on
a SLURM cluster: the wrapper commands, the herdr config that enables the jobs
sidebar, and an installer that links them into place.

Nothing here is required to build or run the fork. It is the operational layer
that makes the fork pleasant to use on a shared login node.

## Install

```bash
git clone https://github.com/jdgeorga/herdr
cd herdr
./site/install.sh              # link wrappers + config, seed template, report
source ./build-env.sh && cargo build --release -j 8
```

`install.sh` only creates symlinks and prints what it resolved. It does not
build, does not touch your shell rc, and does not start any daemon. Re-running it
is safe and repairs a broken or half-installed state.

| Flag | Effect |
| --- | --- |
| `--dry-run` | Print the plan, change nothing |
| `--uninstall` | Remove links this repo owns, restore any backups |
| `--link-herdr` | Also point `herdr` at the fork, not just `herdr-slurm` |
| `--force` | Install even if the checkout looks node-local |

`herdr` is left alone by default so the installer cannot hijack an existing
stock herdr install. Everything is symlinked, so editing a wrapper in the repo
takes effect immediately and `git status` is the truth about what you changed.
Two files are deliberately not symlinked:

- `~/.config/herdr-slurm/session-template.json` — seeded once from
  `site/config/session-template.json`, never overwritten. It contains working
  directories, so it is your data.
- `~/.config/herdr-slurm/site.env` — never created or touched. It is your
  cluster's overrides.

If a real file already sits where a link belongs, it is moved to
`<name>.bak.pre-site-install` first and `--uninstall` puts it back.

## Commands

All of these work through `herdr`, `herdr-slurm`, and `herdr-stock`, and act on
both herdr variants at once.

| Command | What it does |
| --- | --- |
| `herdr site` | Show every resolved cluster path and where it came from |
| `herdr ls [host ...]` | List running herdr sessions across your login nodes |
| `herdr health [host ...]` | Cgroup memory, task counts, OOM kills, repeated process groups, large Claude `/tmp` dirs |
| `herdr cleanup [--dry-run]` | Stop both variants on remembered nodes; saved sessions are preserved |
| `herdr reap [--kill] [--delete-claude-tmp]` | Find runaway process groups. Dry-run unless a flag is given |
| `herdr sync` | Rebase the fork onto upstream and rebuild |

With no host arguments, the multi-node commands discover nodes from your saved
sessions and per-node spinner records, then fan out over SSH concurrently, so
one wedged node costs one timeout total rather than one per node.

`herdr update` is disabled on the fork: upstream's updater swaps in a prebuilt
binary from herdr.dev, which would silently revert you to stock herdr and drop
the jobs sidebar. Use `herdr sync` instead.

`herdr reap` never takes action without an explicit flag, and it revalidates
every target on the node immediately before signalling or deleting.

## Site resolution

Nothing in `site/bin` hardcodes a cluster. `site/lib/herdr-site.sh` resolves
each value with the first rule that produces an answer:

1. the variable is already set in your environment
2. `~/.config/herdr-slurm/site.env`
3. a runtime probe
4. a built-in default

Run `herdr site` to see the outcome and the rule that produced each value:

```
VALUE                  RESOLVED                          FROM
REPO                   /pscratch/sd/j/jdgeorga/herdr-src probe: library location
FORK_BIN               /pscratch/.../release/herdr       probe: CARGO_TARGET_DIR
LOGIN_PREFIX           login                             probe: hostname login03
PYTHON                 /usr/bin/python3.11               probe: python 3.11.9
CGROUP_MODE            v2                                probe: cgroup.controllers present
```

| Variable | Meaning | Probe |
| --- | --- | --- |
| `HERDR_SITE_REPO` | Repo checkout | Walks up from the library, so it survives the symlinks |
| `HERDR_SITE_BIN_DIR` | Where the wrappers live | Same |
| `HERDR_SITE_FORK_BIN` | The fork build | `$CARGO_TARGET_DIR/release/herdr`, else `$REPO/target/release/herdr` |
| `HERDR_SITE_STOCK_BIN` | Upstream binary | `~/.local/bin/herdr-bin` |
| `HERDR_SITE_LOGIN_PREFIX` | Login node name stem | Trailing digits stripped from `hostname -s` |
| `HERDR_SITE_LOGIN_PATTERN` | Regex for a valid host argument | Derived from the prefix |
| `HERDR_SITE_PYTHON` | Jobs provider interpreter | First `python3.11`+ found, version-gated |
| `HERDR_SITE_CGROUP_MODE` | `v1` or `v2` | `/sys/fs/cgroup/cgroup.controllers` |
| `HERDR_SITE_CGROUP_DIR` | Your user slice | Derived from mode and uid |

To override anything, write `~/.config/herdr-slurm/site.env`:

```bash
HERDR_SITE_LOGIN_PREFIX=ln
HERDR_SITE_PYTHON=/opt/python/3.12/bin/python3
```

`HERDR_SITE_LOGIN_PREFIX` is the one to set on a cluster whose login nodes are
not named `<stem><digits>`; the pattern and the session-discovery globs are both
derived from it. The interpreter gate is >= 3.11 and is load-bearing: bare
`python3` is 3.6 on Perlmutter login nodes, and an ungated fallback would fail
inside the jobs provider instead of at resolution time with a usable message.

## Portability

Tested on NERSC Perlmutter. The cluster-specific assumptions are all funnelled
through `site/lib/herdr-site.sh`, and the non-Perlmutter paths through it are
covered by unit tests with faked probe inputs — but they have not been exercised
on a second real cluster. On a new site, run `herdr site` first and check the
resolved values and warnings before trusting `herdr health` numbers.

Known limits:

- Login nodes are not SLURM compute nodes, so there is nothing authoritative to
  query for their names; the pattern is inferred from the current hostname.
- cgroup v1 has no cumulative OOM-kill counter, so `herdr health` reports 0
  there rather than inventing a number.
- `herdr-spinner` intentionally invokes bare `python3`: its daemon is
  3.6-compatible and does not need the >= 3.11 interpreter the jobs provider
  requires.

## Layout

```
site/
  install.sh                    link + verify; no build, no rc edits
  lib/herdr-site.sh             site resolution + shared host/format helpers
  bin/herdr-slurm               fork launcher and subcommand dispatcher
  bin/herdr-stock               stock herdr in a per-node session
  bin/herdr-{ls,health,cleanup,reap}
  bin/herdr-slurm-cleanup       compat shim for the old name
  config/config.toml            tracked, portable, symlinked into place
  config/session-template.json  starter, seeded once
  config/scripts/herdr-jobs     interpreter shim for the jobs provider
  config/scripts/herdr-spinner{,.py}
  config/scripts/orphan-scan.py
```

The SLURM logic itself lives in `scripts/herdr-jobs.py`, not here; display
changes need no rebuild. `site/` touches no upstream-owned file, so it costs
nothing at rebase time.
