# Vendoring the login-node wrappers and config into the fork

Date: 2026-08-12
Status: implemented

## Problem

The fork's day-to-day usability lived entirely outside the repo. Seven wrapper
scripts in `~/.local/bin` (~845 lines of bash), the herdr config that enables the
jobs sidebar, a session template, and three config-dir scripts existed only on
one machine, in one user's home directory. A fresh clone of the fork produced a
working binary and nothing else: no `herdr health`, no `herdr ls`, no jobs
sidebar config, no way for anyone else to reproduce the setup.

Two of those files were already tracked (`scripts/herdr-jobs.py`,
`scripts/herdr-sync.sh`), by two different mechanisms — the sync script is
symlinked from the repo, the provider is copied. The copy had not drifted, but
nothing prevented it.

The scripts were also hardcoded to NERSC Perlmutter in ways that go beyond
`$HOME`: every multi-node script gated host arguments on `^login[0-9]+$`, two
hardcoded `/pscratch/sd/j/jdgeorga/rust/target/release/herdr`, `herdr-health`
read `/sys/fs/cgroup/user.slice/user-$uid.slice` (cgroup v2 only), and the config
pinned `/usr/bin/python3.11`.

## Goals

Make a fresh clone reproduce the setup on **any SLURM cluster**, without
personal paths, and without the repo becoming a second source of truth that can
drift from what is actually running.

Non-goals: building the binary, editing shell rc files, starting daemons,
supporting non-SLURM machines, or changing what any command does.

## Design

### Layout

One new top-level `site/` tree. This placement is deliberate: the fork's rebase
cost is concentrated in files upstream also edits (`src/ui/sidebar.rs`,
`src/app/mod.rs`, `src/app/state.rs`, `src/ui.rs`, `src/app/input/mouse.rs`),
while the fork's *new* files have never conflicted. `site/` touches no
upstream-owned path, so it adds zero ongoing rebase burden.

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

### Everything is symlinked

`install.sh` symlinks wrappers into `~/.local/bin` and config-dir scripts into
`~/.config/herdr/scripts`, so the checkout is the single source of truth, edits
are live immediately, and `git status` is the truth about local changes. Drift is
structurally impossible rather than merely monitored.

Two files are deliberately not symlinked, because they are legitimately
per-machine:

- `session-template.json` — seeded once from a tokenised tracked starter,
  `@HERDR_SITE_WORKDIR@` rendered to `${SCRATCH:-$HOME}`, never overwritten. It
  holds working directories, so it is user data. It was already copy-once
  semantics; this only moves where the first copy comes from.
- `site.env` — never created or touched. It is the per-cluster override file.

A pre-existing regular file at a link destination is moved to
`<name>.bak.pre-site-install`, and `--uninstall` restores it. `--uninstall`
removes only symlinks that resolve inside the repo, so a user's own symlink at
one of those paths survives.

`herdr` itself is left alone by default (`--link-herdr` opts in), so the
installer cannot hijack an existing stock herdr install.

### config.toml becomes portable

`config.toml` had exactly one cluster-specific line: the jobs provider command
pinning `/usr/bin/python3.11`. That moves into a new `site/config/scripts/herdr-jobs`
shell shim which resolves the interpreter itself and applies the mandatory
`-I -S`. The config becomes:

```toml
command = ["~/.config/herdr/scripts/herdr-jobs", "--mode", "{mode}"]
```

This works because `run_list_poll` in `src/app/list_refresh.rs` spawns the
provider with a bare `Command::new(program)`, so the shim's shebang is honoured.
It also mirrors the existing `herdr-spinner` / `herdr-spinner.py` split. With
that line gone, `config.toml` is fully site-independent and can be tracked and
symlinked like any other file.

### Site resolution

`site/lib/herdr-site.sh` is sourced by every wrapper and resolves each value by
the first rule that answers:

1. the variable is already set in the environment
2. `~/.config/herdr-slurm/site.env`
3. a runtime probe
4. a built-in default

| Value | Probe |
| --- | --- |
| `HERDR_SITE_REPO`, `..._BIN_DIR` | Walk up from the library, so it survives the install symlinks |
| `HERDR_SITE_FORK_BIN` | `$CARGO_TARGET_DIR/release/herdr`, then `$PSCRATCH/rust/target`, then `$REPO/target/release/herdr` |
| `HERDR_SITE_STOCK_BIN` | `$HERDR_BIN`, else `~/.local/bin/herdr-bin` |
| `HERDR_SITE_LOGIN_PREFIX` | Trailing digits stripped from `hostname -s` |
| `HERDR_SITE_LOGIN_PATTERN` | Derived from the prefix |
| `HERDR_SITE_PYTHON` | First `python3.11`+ found, gated on `sys.version_info >= (3, 11)` |
| `HERDR_SITE_CGROUP_MODE`, `..._DIR` | `/sys/fs/cgroup/cgroup.controllers` presence, plus uid |

Login nodes are not SLURM compute nodes, so `scontrol`/`sinfo` cannot be asked
for their names; the current hostname is the only reliable evidence. Stripping
trailing digits handles `login03` → `login` and `ln01` → `ln`, and a digitless
hostname yields a pattern matching only itself, which keeps single-host machines
working without a special case.

The `>= 3.11` gate is load-bearing: bare `python3` is 3.6.15 on Perlmutter login
nodes, so an ungated fallback would fail deep inside the jobs provider instead of
at resolution time with a message that names the problem.

The accepted risk of probing is a wrong guess that is never noticed. The
mitigation is `herdr site`, which prints every resolved value *and the rule that
produced it*, plus warnings for a missing fork binary, a missing interpreter, an
unreadable cgroup dir, and absent `squeue`.

### Deduplication

All four multi-node scripts carried the same four blocks: an `add_host` with the
login regex, a session/spinner host-discovery loop, a `login[0-9]*` argument
case, and a parallel-SSH fan-out with a result directory. These are now
`herdr_site_add_host`, `herdr_site_discover_hosts`, `herdr_site_is_login_host`,
and `herdr_site_fanout`. `human_bytes` and the cgroup reads moved too, with the
cgroup helper hiding the v1/v2 filename differences.

Two incidental improvements fall out. Dispatch between wrappers now goes to the
repo sibling via `herdr_site_wrapper` rather than a hardcoded
`$HOME/.local/bin/herdr-<cmd>`, which removes the bug class that previously made
`herdr ls` double-report the fork session when the symlink layout changed. And an
unrecognised host argument now reports the pattern it failed to match instead of
being silently ignored.

## Testing

`scripts/test_site_install.py`, 28 unittest cases wired into `just test` and
`just check`. It must stay unittest-based and 3.6-clean because `just test` runs
these with the system `python3`.

Coverage: resolution precedence across all four rules and their provenance
labels; login-pattern derivation for a non-Perlmutter hostname and a digitless
one, using a stub `hostname` on `PATH`; the version gate; cgroup v1 vs v2 file
selection and graceful degradation when the cgroup tree is absent;
`config.toml` containing no cluster path; the session template staying
tokenised; `bash -n` over every shell file; and installer behaviour — dry-run
inertness, full link set pointing into the repo, idempotency, backup-not-clobber,
refusal to overwrite an existing backup, uninstall restoring backups, uninstall
leaving a foreign symlink alone, and seed-once semantics.

One live bug was found this way only after installing: `herdr-slurm` exports
`HERDR_SITE_RESOLVED` and then `exec`s a sibling, and the library's
double-source guard sat at the top of the file, so the sibling returned before
defining any function. The guard now wraps only the `herdr_site_resolve` call,
and a regression test asserts every helper is defined when the flag is preset.

Live verification on Perlmutter: `herdr site`, `herdr ls`, `herdr health` across
8 login nodes, `herdr health login03`, an invalid host argument, `herdr reap`
dry-run, `herdr cleanup --dry-run`, the compat shim, `herdr --help`, the jobs
shim emitting valid JSON with real SLURM data, and `herdr server reload-config`
applying with zero diagnostics.

## Limitations

Tested on Perlmutter only. The non-Perlmutter branches are exercised by unit
tests with faked probe inputs, not on a second real cluster; `herdr site` should
be run and read before trusting `herdr health` numbers at a new site.

`shellcheck` is not installed on these login nodes, so shell linting is limited
to `bash -n` in the test suite.

The jobs sidebar's rendered state is client-side, so there is no server API to
read it back; the config change is verified by a clean `reload-config` plus the
shim producing valid JSON under exactly the argv the config specifies, not by
observing the sidebar draw.

cgroup v1 has no cumulative OOM-kill counter, so `herdr health` reports 0 there
rather than inventing a number. `herdr-spinner` still invokes bare `python3` on
purpose: its daemon is 3.6-compatible and does not need the `>= 3.11`
interpreter the jobs provider requires.
