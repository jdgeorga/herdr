# Building and running herdr on TACC Frontera

Frontera-specific by intent. Nothing here is meant to work on another cluster.

Full reasoning: [`specs/2026-08-11-herdr-slurm-frontera-design.md`](../../specs/2026-08-11-herdr-slurm-frontera-design.md).

## The problem in one paragraph

`build.rs` shells out to `zig build` for the vendored `libghostty-vt`. Zig 0.15.x calls
`statx(2)`, added in Linux 4.11. Frontera is CentOS 7 on kernel **3.10**, so every zig
invocation — even `zig init` — dies with `error: unable to load package manifest ...
Unexpected`. `LD_PRELOAD` cannot shim it (static binary, raw syscalls) and a container cannot
help (Apptainer shares the host kernel). The Rust half is completely fine.

So: build the Zig archive in CI, and give `build.rs` something that verifies and stages it.
`build.rs` already reads a `ZIG` environment variable, so this needs **zero changes to any
upstream-owned file**.

## First-time setup

```bash
# 0. one-time, needs a browser: enable Actions on the fork
#    https://github.com/jdgeorga/herdr/actions
gh workflow list --repo jdgeorga/herdr      # must list workflows, not nothing

# 1. push the branch; CI builds libghostty-vt.a and publishes it as a release asset
git push origin herdr-slurm-frontera

# 2. record what CI produced (deliberate, reviewed commit -- never automate this)
gh release download frontera-libghostty-vt --repo jdgeorga/herdr \
   --pattern provenance.json --dir /tmp
cp /tmp/provenance.json scripts/frontera/provenance.json
git commit -am 'chore(frontera): refresh libghostty-vt provenance'

# 3. stage it on /work2
scripts/frontera/fetch-artifact.sh

# 4. prove it links BEFORE spending an hour on a 600-crate build
scripts/frontera/link-probe.sh
```

## Building

```bash
sbatch scripts/frontera/build.slurm       # normal queue, 3h, runs link-probe first
```

or interactively on a compute node:

```bash
source scripts/frontera/env.sh
cargo build --release --locked -j 24
```

Never on a login node: `ulimit -u` there is 300 and counts **threads**, so parallel rustc
trips it and reports misleading errors (`WouldBlock`, `EAGAIN`).

## The SLURM sidebar

`scripts/herdr-jobs.py` was written for NERSC but runs on Frontera **unmodified** — verified
against Slurm 23.11 in both `live` and `history` modes, and `--selftest` passes. It only needs
the right interpreter, which is what `herdr-jobs-frontera.sh` provides.

```bash
cp scripts/frontera/config.frontera.toml ~/.config/herdr/config.toml
scripts/frontera/herdr-jobs-frontera.sh --mode live      # should print one JSON object
```

Two Frontera facts drive the wrapper:

- **Python.** The provider needs >= 3.7 (`from __future__ import annotations`).
  `/usr/bin/python3` is 3.6.8 and cannot parse it. `python3/3.9.2` works.
- **libssp.** Juliaup ships its own `libssp.so.0`, `.bashrc` puts it first on
  `LD_LIBRARY_PATH`, and it lacks `__vsnprintf_chk@LIBSSP_1.0` — so every Intel-built python
  dies with a relocation error. gcc 8.3.0's `lib64` has the working copy and must come first.

## Files

| File | Role |
|---|---|
| `zig-shim.sh` | Stands in for `zig build`. **Verifies, then exits 0.** Never a no-op. |
| `env.sh` | Toolchain paths, `ZIG`, XALT bypass, linker pin, libssp fix. |
| `fetch-artifact.sh` | Downloads the CI archive to `/work2`, rejects it if provenance disagrees. |
| `link-probe.sh` | 30-second go/no-go gate. Run before any full build. |
| `provenance.json` | The committed expectation the shim checks against. |
| `herdr-jobs-frontera.sh` | Interpreter wrapper for the SLURM sidebar. |
| `config.frontera.toml` | Sidebar config pointing at the wrapper. |
| `build.slurm` | sbatch wrapper (`normal` queue, not `development`). |

## When a build suddenly refuses to start

```
frontera zig-shim: FATAL: vendored libghostty-vt has changed since the archive was built.
```

Working as designed. You rebased onto an upstream that re-vendored `libghostty-vt`, so the
pinned archive no longer matches the source. Redo steps 1–3 above.

**Do not "fix" this by relaxing the check.** `src/ghostty/bindings.rs` is committed bindgen
output with 64 `#[repr(C)]` types and *zero* layout tests. A stale archive exports identical
symbol names with different struct layouts, so it links cleanly, boots, and reads garbage.
Every other failure mode in this setup is loud; this is the only silent one, and that digest
is the only thing standing in front of it.

Its regression test — which must keep failing:

```bash
printf '\n// staleness probe\n' >> vendor/libghostty-vt/src/lib_vt.zig
cargo build --release          # MUST fail with a provenance mismatch
git checkout -- vendor/libghostty-vt/src/lib_vt.zig
```

Note it must be a **content** change. `touch` alone does not trip the gate — the digest hashes
file contents, not mtimes — which is the desired behaviour but makes `touch` useless as a test.
(Verified both ways on 2026-08-11.)

If that ever *succeeds*, the guard has gone fail-open (most likely `build.rs` stopped honouring
`ZIG`) and the archive is no longer being checked at all.

## Not supported here

`just check` / `just test`. Their `windows-lint` leg cross-compiles to
`x86_64-pc-windows-msvc`, which needs a real zig regardless of this shim. The shim
deliberately **refuses** any target other than `x86_64-unknown-linux-gnu` rather than handing
a linux archive to a windows build and letting you believe the leg passed. Frontera is scoped
to `cargo build --release`.
