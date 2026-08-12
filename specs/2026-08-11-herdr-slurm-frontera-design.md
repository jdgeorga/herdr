# herdr on TACC Frontera — design

Branch: `herdr-slurm-frontera`, cut from `feat/slurm-jobs-sidebar` (12 commits ahead of
`master`, 0 behind, rebased 2026-08-11).

Frontera-specific by intent. This branch is not meant to be portable to other clusters and
does not try to be.

## Problem

`build.rs` shells out to `zig build` for the vendored `libghostty-vt`. Zig 0.15.x cannot run
on Frontera at all: it calls `statx(2)` (Linux 4.11+) with no fallback, and Frontera is
CentOS 7 on kernel 3.10, so every invocation dies with `error: unable to load package
manifest ... Unexpected`. This is not a filesystem, quota, or process-cap issue — it
reproduces on local ext4, on Lustre, and on a compute node with `ulimit -u 4096`. `LD_PRELOAD`
cannot shim it (the Zig binary is static and issues raw syscalls) and a container cannot help
(Apptainer shares the host kernel).

The Rust half is fine. With the Zig step stubbed out, the entire crate graph compiles on
Frontera with the pinned Rust 1.96.1 and fails only at the final link, on the missing
`ghostty_*` symbols.

## Goal

1. `cargo build --release` works on a Frontera compute node, so the SLURM sidebar code can be
   iterated on in place.
2. The SLURM jobs sidebar actually populates on Frontera.
3. The branch stays rebaseable onto upstream `master` indefinitely.

## Non-goals

- Making `just check` / `just test` pass. Their `windows-lint` leg cross-compiles to
  `x86_64-pc-windows-msvc`, which needs a working Zig regardless. Frontera is scoped to
  `cargo build --release` and, optionally, `cargo nextest run` on the gnu target.
- Building Zig from source on Frontera, or patching its std.
- Upstreaming any of this.

## Established facts

Every claim below was verified on this host on 2026-08-11, not assumed.

| Fact | Evidence |
|---|---|
| `build.rs` already honours a `ZIG` env var | `build.rs:63` — `env::var("ZIG").unwrap_or_else(\|_\| "zig".into())`, then `Command::new(zig)`, then `assert!(status.success())` |
| A shim satisfying that var gets cargo past the build script | Probe A: `cargo build --release` with a stub `ZIG` compiled the full graph and failed at link with "some `extern` functions couldn't be found". Prior attempts died in the build script at 16s. |
| `vendor/libghostty-vt/zig-out/` is gitignored | `git check-ignore -v` → `vendor/libghostty-vt/.gitignore:12:zig-out/`. Tree stays clean. |
| The vendor-tree digest is fast and discriminating | 1238 files / 19 MB in 0.32 s; stable across runs; a one-line edit changes it. |
| `libghostty-vt.a` is self-contained | `GhosttyLibVt.zig:217-227` sets `bundle_compiler_rt`, `bundle_ubsan_rt`, `pic`; `CombineArchivesStep` merges simdutf + highway via `zig ar -M`. No libc++ (`-DSIMDUTF_NO_LIBCXX`, `-DHWY_NO_LIBCXX`). |
| The glibc pin is load-bearing | Zig's default for an unversioned `linux-gnu` triple is **2.28** (`Target.zig:430`); Frontera has 2.17. The `.2.17` suffix parses (`Query.zig:240-252` + its round-trip test). |
| AVX-512 SIGILL risk is structurally dead | An explicit `-Dtarget` resolves cpu to `Target.Cpu.baseline` (`system.zig:345-350`); Highway separately disables every AVX3/AVX10 dispatch target (`SharedDeps.zig:812-814`). |
| XALT hijacks every link | `COMPILER_PATH=/opt/apps/xalt/xalt/bin` forces `collect2` to XALT's `ld`, a **bash script** that rewrites the link argv and, with `XALT_FUNCTION_TRACKING` on, runs a second full link per invocation. Both `/usr/bin/cc` and gcc-8.3.0 resolve `-print-prog-name=ld` to it. Clean bypass at `xalt/bin/ld:198-203`. |
| Bare `cc` is GCC 4.8.5 / binutils 2.27 | No gcc module is loaded; `/opt/apps/gcc/8.3.0/bin` is in `PATH` only incidentally. |
| The SLURM provider runs unmodified on Frontera | `herdr-jobs.py --mode live` and `--mode history` both emit correct JSON against Slurm **23.11.11**, despite being written for NERSC's 25.11. |
| It needs Python ≥ 3.7 | `/usr/bin/python3` is 3.6.8 and fails on `from __future__ import annotations`. `python3/3.9.2` compiles and runs it. |
| Intel python is broken by a local libssp shadow | `~/.julia/juliaup/.../lib/julia/libssp.so.0` precedes `/opt/apps/gcc/8.3.0/lib64` in `LD_LIBRARY_PATH` and lacks `__vsnprintf_chk@LIBSSP_1.0`. Not a TACC defect. |
| Fork Actions are not yet usable | `actions/permissions` → `enabled:true`, but `actions/workflows` → `total_count:0` and zero runs, with all 8 YAML files present. Needs a one-time manual enable in the browser. |

## Architecture

Three planes, each with one job.

### 1. Artifact plane — GitHub Actions on the fork

A new workflow builds the one thing Frontera cannot: the Zig static archive.

- Trigger: `on: push: branches: [herdr-slurm-frontera]`, path-filtered to
  `vendor/libghostty-vt/**` and the workflow file itself, plus `workflow_dispatch`.
  A push trigger runs from the branch it lives on; `workflow_dispatch` alone would **not**
  work, because GitHub only surfaces dispatchable workflows that exist on the default branch
  (`master`), and putting it there would diverge the fork's master.
- Reuses `mlugg/setup-zig@d1434d08` pinned to `0.15.2`, matching `ci.yml` and `release.yml`.
- Invokes Zig with `build.rs`'s exact argv plus the Frontera-specific bits:

  ```
  zig build -Demit-lib-vt -Doptimize=ReleaseFast -Dsimd=true \
            -Dtarget=x86_64-linux-gnu.2.17 -Dcpu=baseline \
            -Dversion-string=$(cat VERSION) -Demit-xcframework=false
  ```

- Emits, as a **fork release asset** (not an Actions artifact — the repo's existing upload
  steps set `retention-days: 7`):
  - `libghostty-vt.a`
  - `libghostty-vt.undefined-symbols.txt` (`nm --undefined-only --format=posix`)
  - `provenance.json`
- Fails the job if the undefined-symbol set contains any C++ runtime symbol
  (`_Z`, `__cxa_`, `__gxx_personality`, `_Unwind_`, `_Znwm`, `_ZdlPv`).

`provenance.json` records: `vendor_tree_sha256`, `archive_sha256`, `version_string`,
`source_commit`, `rust_target`, `zig_target`, `zig_version`, the full zig argv, and the CI run
URL.

There are two copies and they play different roles. CI **produces** one and ships it beside the
archive; `scripts/frontera/provenance.json` is the **committed expectation** the shim checks
against. `fetch-artifact.sh` downloads the archive plus the CI copy into the staging directory
on `/work2` and refuses to install them unless the two agree field-for-field. So a CI rebuild
that the branch hasn't acknowledged is rejected at fetch time, and a vendored-source change the
artifact hasn't caught up to is rejected at build time. Updating the committed copy is a
deliberate, reviewable commit — never automatic.

Current expected values:

```
vendor_tree_sha256  3235075cd8eecc8845da6e8bb22afca5356e97a8cbbc4dbb6d94f9d030d92906
version_string      1.3.2-HEAD-+c5a21edfc
source_commit       c5a21edfcbc2d5b46540ad91b7980aca31f5f1f3
rust_target         x86_64-unknown-linux-gnu
zig_target          x86_64-linux-gnu.2.17
```

### 2. Gate plane — `scripts/frontera/zig-shim.sh`

`build.rs` runs this as `$ZIG build …` with cwd `vendor/libghostty-vt`. It is a **verifier that
happens to exit 0**, not a no-op. In order, every step fail-closed:

1. `$TARGET` must equal `provenance.rust_target` → else `exit 1`. This is what stops a
   `--target x86_64-pc-windows-msvc` invocation from silently consuming a linux-gnu archive.
2. Recompute the vendor-tree digest and compare to `provenance.vendor_tree_sha256` → mismatch
   is `exit 1`, printing the exact command to re-trigger CI. **This is the anti-corruption
   gate** and the single most important line in the branch.
3. `sha256sum` the archive against `provenance.archive_sha256` → catches truncation/corruption.
4. `strings libghostty-vt.a` must contain `cat VERSION` → cheap independent staleness check.
5. `mkdir -p zig-out/lib`, copy the archive in, `exit 0`.

A missing artifact or missing provenance file is an error, never a skip. There is no code path
that emits link directives without having verified all four.

**Why a shim and not a `build.rs` patch.** `build.rs` has churned 13 times in the repo's
4.5-month history, most recently 2026-08-06; a guard block inside it is a standing rebase
conflict. The shim lives entirely in `scripts/frontera/`, a directory upstream never touches,
so the branch diff is 100% additions and rebase conflicts are structurally impossible.

**Why step 2 is not optional.** Upstream re-vendors `libghostty-vt` roughly monthly (9 commits
Apr–Jul 2026). `src/ghostty/bindings.rs` is committed bindgen output with 64 `#[repr(C)]` types
and `grep -c bindgen_test_layout` returns **0** — layout tests are disabled. A stale archive
exports identical symbol names with different struct layouts, so it links cleanly, boots, and
reads garbage. Every other failure mode in this design is loud; this is the only silent one.

### 3. Runtime plane — `scripts/frontera/`

`env.sh`:
- `XALT_EXECUTABLE_TRACKING=no`; unset `LD_PRELOAD` and `COMPILER_PATH`.
- Pin `CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=/opt/apps/gcc/8.3.0/bin/gcc` rather than
  inheriting bare `cc` (GCC 4.8.5). `env.sh` asserts that path exists and reports 8.3.0,
  failing loudly if TACC moves it, rather than silently falling back.
- `RUSTUP_HOME` / `CARGO_HOME` / `CARGO_TARGET_DIR` on `/work2` — `/home1` is at 84.6% of its
  200k inode quota.
- Export `HERDR_FRONTERA_PREBUILT` pointing at the staged artifact directory.

`herdr-jobs-frontera.sh` — interpreter wrapper for the sidebar. Prepends
`/opt/apps/intel19/python3/3.9.2/lib` **and** `/opt/apps/gcc/8.3.0/lib64` to
`LD_LIBRARY_PATH` so the correct `libssp.so.0` wins over the Julia one, then execs
`/opt/apps/intel19/python3/3.9.2/bin/python3 -I -S scripts/herdr-jobs.py "$@"`.
`scripts/herdr-jobs.py` itself is **not modified** — it works as-is, and leaving it untouched
keeps future sidebar changes conflict-free.

`config.frontera.toml` — sidebar config pointing `[ui.sidebar.list] command` at the wrapper.

`build.slurm` — sbatch wrapper. Uses the `normal` queue, not `development`: no full release
build has ever completed on this host, and `development`'s QOS MaxWall is exactly the 2h the
old script requested.

## File inventory

All additions. No upstream-owned file is modified.

```
.github/workflows/frontera-libghostty-vt.yml
scripts/frontera/zig-shim.sh
scripts/frontera/env.sh
scripts/frontera/fetch-artifact.sh
scripts/frontera/herdr-jobs-frontera.sh
scripts/frontera/build.slurm
scripts/frontera/config.frontera.toml
scripts/frontera/provenance.json
scripts/frontera/README.md
specs/2026-08-11-herdr-slurm-frontera-design.md
```

## Acceptance gates

Ordered. Each must pass before the next is meaningful.

1. **Actions live.** `gh workflow list --repo jdgeorga/herdr` returns 8 rows. *(Blocked on a
   one-time manual enable in the browser; no API can do it.)*
2. **CI produces the archive**, and its undefined-symbol set contains no C++ runtime symbols.
3. **Archive readable by Frontera's own binutils.** `/usr/bin/nm --print-armap` and
   `/usr/bin/ar t` on the real `.a` — confirms GNU ld 2.27 can read an `llvm-ar` MRI-generated
   fat archive. Never exercised upstream, which builds on ubuntu-latest.
4. **The C probe.** A ~15-line program calling `ghostty_build_info(GHOSTTY_BUILD_INFO_VERSION_STRING, …)`,
   linked against the real archive with system `cc`. One command simultaneously falsifies
   undefined symbols, PIE-vs-PIC relocations, ar-index compatibility, and staleness (it prints
   the embedded version, which must equal `cat VERSION`). Run it with
   `XALT_EXECUTABLE_TRACKING` both on and off, to isolate XALT before it contaminates a cargo
   build. **This is the go/no-go**, and it costs 30 seconds instead of a 600-crate build.
5. **Full build.** `cargo build --release --locked` on a compute node, producing a runnable
   `herdr --version`.
6. **The guard is real.** `touch vendor/libghostty-vt/src/*.zig && cargo build --release` must
   **fail** with a provenance mismatch. If it succeeds, the guard is fail-open and the design's
   worst risk is live. This is a required test, not a nicety.
7. **Sidebar populates** under a real `idev` session.

## Maintenance

On every rebase onto upstream `master`:

- If `vendor/libghostty-vt` changed, the digest check fails loudly on the next build. Push the
  branch, let CI rebuild the archive, update `provenance.json`, re-fetch. The failure message
  contains the exact commands.
- If `build.rs` stops honouring `ZIG`, the shim silently stops being consulted. Guard: gate 6
  doubles as the detector — if `touch` + build stops failing, the mechanism has moved.

## Open risks

| Risk | Likelihood | Handling |
|---|---|---|
| `GhosttyBench.init()` isn't gated on `emit_lib_vt` and may force GTK/font probing in CI | low | Not pre-solved. It either works on first dispatch or it doesn't; if it fails, a one-line guard belongs in `vendor/patches/libghostty-vt/`. |
| CI's zig argv drifts from `build.rs`'s | medium | Full argv recorded in provenance so a human can diff it. |
| Old binutils reading a DWARF-5 `llvm-ar` archive | low | Gate 3. Failure is loud; `strip --strip-debug` is the fallback. |
| Login-node thread cap | n/a | Out of scope — herdr will be run on a compute node via `idev` + attach. |

## Deferred

- `just check` / `just test` on Frontera. Needs a working Zig for the windows-msvc clippy leg.
- Any change to `scripts/herdr-jobs.py`. It works unmodified; touching it only buys conflicts.
