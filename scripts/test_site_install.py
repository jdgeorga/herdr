"""Tests for site/lib/herdr-site.sh and site/install.sh.

These cover the parts of site resolution that cannot be exercised on the machine
the tests run on: a cluster whose login nodes are not named `loginNN`, a host
with no numeric suffix, and cgroup v1. Probe inputs are faked (a stub `hostname`
on PATH, a temporary cgroup tree) so the non-Perlmutter branches are actually
executed rather than merely reviewed.

Kept to unittest and 3.6-compatible syntax: `just test` runs these with the
system `python3`, which is 3.6 on NERSC login nodes.
"""

import os
import shutil
import stat
import subprocess
import tempfile
import unittest

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
LIB = os.path.join(REPO, "site", "lib", "herdr-site.sh")
INSTALL = os.path.join(REPO, "site", "install.sh")

# The same >= 3.11 check the library applies when probing for an interpreter.
GATE = "-c 'import sys; raise SystemExit(0 if sys.version_info >= (3, 11) else 1)'"

WRAPPERS = [
    "herdr-slurm",
    "herdr-stock",
    "herdr-health",
    "herdr-ls",
    "herdr-cleanup",
    "herdr-reap",
    "herdr-slurm-cleanup",
]

# Everything install.sh is expected to link, relative to a fake $HOME.
LINK_DESTS = (
    [os.path.join(".local", "bin", name) for name in WRAPPERS]
    + [
        os.path.join(".local", "bin", "herdr-sync"),
        os.path.join(".config", "herdr", "scripts", "herdr-jobs"),
        os.path.join(".config", "herdr", "scripts", "herdr-jobs.py"),
        os.path.join(".config", "herdr", "scripts", "herdr-spinner"),
        os.path.join(".config", "herdr", "scripts", "herdr-spinner.py"),
        os.path.join(".config", "herdr", "scripts", "orphan-scan.py"),
        os.path.join(".config", "herdr-slurm", "config.toml"),
    ]
)


def run_bash(script, env=None, cwd=None):
    """Run a bash snippet and return (returncode, stdout, stderr)."""
    full_env = dict(os.environ)
    # Keep the caller's real site.env out of every test.
    full_env["HERDR_SITE_ENV"] = "/nonexistent/site.env"
    if env:
        full_env.update(env)
    proc = subprocess.run(
        ["bash", "-c", script],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        universal_newlines=True,
        env=full_env,
        cwd=cwd or REPO,
    )
    return proc.returncode, proc.stdout, proc.stderr


def source_and_print(varnames, env=None):
    """Source the library and print the named variables, one per line."""
    body = ['. "%s"' % LIB]
    for name in varnames:
        body.append('printf "%%s\\n" "${%s:-}"' % name)
    code, out, err = run_bash("set -u\n" + "\n".join(body), env=env)
    if code != 0:
        raise AssertionError("sourcing failed: %s%s" % (out, err))
    return out.rstrip("\n").split("\n")


def make_stub_hostname(directory, name):
    """Put a `hostname` on PATH that reports `name`, to fake the login probe."""
    path = os.path.join(directory, "hostname")
    with open(path, "w") as handle:
        handle.write('#!/bin/bash\nprintf "%s\\n"\n' % name)
    os.chmod(path, os.stat(path).st_mode | stat.S_IEXEC)
    return directory


class SiteResolutionTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp(prefix="herdr-site-test.")
        self.addCleanup(shutil.rmtree, self.tmp, True)

    def write_site_env(self, contents):
        path = os.path.join(self.tmp, "site.env")
        with open(path, "w") as handle:
            handle.write(contents)
        return path

    def test_repo_is_found_by_walking_up_from_the_library(self):
        values = source_and_print(["HERDR_SITE_REPO", "HERDR_SITE_BIN_DIR"])
        self.assertEqual(values[0], REPO)
        self.assertEqual(values[1], os.path.join(REPO, "site", "bin"))

    def test_functions_are_defined_even_when_resolution_already_happened(self):
        # A wrapper that dispatches to a sibling exports HERDR_SITE_RESOLVED.
        # The sibling must still get the function definitions; an early return
        # on that flag left every helper undefined.
        script = 'set -u\n. "%s"\n' % LIB
        for name in (
            "herdr_site_wrapper",
            "herdr_site_hosts",
            "herdr_site_fanout",
            "herdr_site_add_host",
            "herdr_site_is_login_host",
            "herdr_site_human_bytes",
            "herdr_site_report",
        ):
            script += 'declare -F %s >/dev/null || { echo "missing %s"; exit 1; }\n' % (
                name,
                name,
            )
        # The arrays the wrappers index into must exist too.
        script += 'echo "${#HERDR_SITE_UNAVAILABLE[@]}"\n'
        code, out, err = run_bash(script, env={"HERDR_SITE_RESOLVED": "1"})
        self.assertEqual(code, 0, out + err)
        self.assertEqual(out.strip(), "0")

    def test_site_env_supplies_a_value_when_the_environment_does_not(self):
        env_file = self.write_site_env("HERDR_SITE_PYTHON=/from/site-env\n")
        values = source_and_print(
            ["HERDR_SITE_PYTHON"], env={"HERDR_SITE_ENV": env_file}
        )
        self.assertEqual(values[0], "/from/site-env")

    def test_environment_wins_over_site_env(self):
        env_file = self.write_site_env("HERDR_SITE_PYTHON=/from/site-env\n")
        values = source_and_print(
            ["HERDR_SITE_PYTHON"],
            env={"HERDR_SITE_ENV": env_file, "HERDR_SITE_PYTHON": "/from/env"},
        )
        self.assertEqual(values[0], "/from/env")

    def test_site_env_wins_over_the_probe(self):
        env_file = self.write_site_env("HERDR_SITE_LOGIN_PREFIX=zzz\n")
        values = source_and_print(
            ["HERDR_SITE_LOGIN_PREFIX"], env={"HERDR_SITE_ENV": env_file}
        )
        self.assertEqual(values[0], "zzz")

    def test_provenance_is_reported_for_each_rule(self):
        env_file = self.write_site_env("HERDR_SITE_PYTHON=/from/site-env\n")
        code, out, err = run_bash(
            'set -u\n. "%s"\nherdr_site_report' % LIB,
            env={"HERDR_SITE_ENV": env_file, "HERDR_SITE_LOGIN_PREFIX": "fromenv"},
        )
        self.assertEqual(code, 0, err)
        self.assertIn("site.env", out)
        # The value set in the caller's environment must be attributed to env.
        for line in out.split("\n"):
            if line.startswith("LOGIN_PREFIX"):
                self.assertTrue(line.rstrip().endswith("env"), line)
                break
        else:
            self.fail("LOGIN_PREFIX missing from report:\n" + out)

    def test_login_pattern_is_derived_from_a_non_perlmutter_hostname(self):
        stub = make_stub_hostname(self.tmp, "ln07")
        values = source_and_print(
            ["HERDR_SITE_LOGIN_PREFIX", "HERDR_SITE_LOGIN_PATTERN"],
            env={"PATH": stub + os.pathsep + os.environ["PATH"]},
        )
        self.assertEqual(values[0], "ln")
        self.assertEqual(values[1], "^ln[0-9]+$")

    def test_hostname_without_digits_matches_only_itself(self):
        stub = make_stub_hostname(self.tmp, "bigiron")
        values = source_and_print(
            ["HERDR_SITE_LOGIN_PREFIX", "HERDR_SITE_LOGIN_PATTERN"],
            env={"PATH": stub + os.pathsep + os.environ["PATH"]},
        )
        self.assertEqual(values[0], "bigiron")
        self.assertEqual(values[1], "^bigiron$")

    def test_host_arguments_are_matched_against_the_resolved_pattern(self):
        script = 'set -u\n. "%s"\n' % LIB
        script += 'herdr_site_is_login_host ln07 && echo yes || echo no\n'
        script += 'herdr_site_is_login_host login03 && echo yes || echo no\n'
        code, out, err = run_bash(
            script, env={"HERDR_SITE_LOGIN_PATTERN": "^ln[0-9]+$"}
        )
        self.assertEqual(code, 0, err)
        self.assertEqual(out.split(), ["yes", "no"])

    def test_resolved_python_satisfies_the_version_gate(self):
        resolved = source_and_print(["HERDR_SITE_PYTHON"])[0]
        if not resolved:
            self.skipTest("no python >= 3.11 on this machine")
        code, _, err = run_bash('"%s" %s' % (resolved, GATE))
        self.assertEqual(code, 0, "%s failed the gate: %s" % (resolved, err))

    def test_an_older_interpreter_is_rejected_and_not_selected(self):
        # PATH cannot be used to hide candidates, because the probe also tries
        # absolute /usr/bin/python3.1x paths. So prove the gate itself rejects a
        # real older interpreter, and that resolution did not pick it.
        old = shutil.which("python3")
        if old is None:
            self.skipTest("no python3 on PATH")
        code, _, _ = run_bash('"%s" %s' % (old, GATE))
        if code == 0:
            self.skipTest("python3 on this machine is already >= 3.11")
        resolved = source_and_print(["HERDR_SITE_PYTHON"])[0]
        self.assertNotEqual(
            os.path.realpath(resolved) if resolved else "",
            os.path.realpath(old),
            "resolution selected an interpreter older than 3.11",
        )

    def test_cgroup_v1_and_v2_read_different_files(self):
        for mode, filename in (("v2", "memory.current"), ("v1", "memory.usage_in_bytes")):
            cgdir = os.path.join(self.tmp, "cgroup-" + mode)
            os.makedirs(cgdir)
            with open(os.path.join(cgdir, filename), "w") as handle:
                handle.write("4096\n")
            values = source_and_print(
                ["_"],
                env={"HERDR_SITE_CGROUP_MODE": mode, "HERDR_SITE_CGROUP_DIR": cgdir},
            )
            del values
            code, out, err = run_bash(
                'set -u\n. "%s"\nherdr_site_cgroup_read memory' % LIB,
                env={"HERDR_SITE_CGROUP_MODE": mode, "HERDR_SITE_CGROUP_DIR": cgdir},
            )
            self.assertEqual(code, 0, err)
            self.assertEqual(out.strip(), "4096", "mode " + mode)

    def test_missing_cgroup_metrics_degrade_instead_of_failing(self):
        code, out, err = run_bash(
            'set -u\n. "%s"\n'
            'herdr_site_cgroup_read memory\n'
            'herdr_site_cgroup_read memory_max\n'
            'herdr_site_cgroup_oom_kills\n' % LIB,
            env={
                "HERDR_SITE_CGROUP_MODE": "v1",
                "HERDR_SITE_CGROUP_DIR": os.path.join(self.tmp, "absent"),
            },
        )
        self.assertEqual(code, 0, err)
        # cgroup v1 has no cumulative oom_kill counter, so it must report 0.
        self.assertEqual(out.split(), ["0", "max", "0"])

    def test_human_bytes_formatting(self):
        code, out, err = run_bash(
            'set -u\n. "%s"\n'
            'herdr_site_human_bytes 2147483648; echo\n'
            'herdr_site_human_bytes 5242880; echo\n'
            'herdr_site_human_bytes max; echo\n' % LIB
        )
        self.assertEqual(code, 0, err)
        self.assertEqual(out.split(), ["2.0G", "5M", "unlimited"])


class ConfigPortabilityTest(unittest.TestCase):
    """config.toml is tracked and symlinked, so it must name no cluster path."""

    def test_config_has_no_absolute_interpreter_or_home_path(self):
        path = os.path.join(REPO, "site", "config", "config.toml")
        with open(path) as handle:
            lines = handle.read().split("\n")
        offenders = []
        for number, line in enumerate(lines, 1):
            if line.lstrip().startswith("#"):
                continue
            for needle in ("/usr/bin/python", "/pscratch", "/global/homes", "jdgeorga"):
                if needle in line:
                    offenders.append("%d: %s" % (number, line.strip()))
        self.assertEqual(offenders, [], "cluster-specific paths in config.toml")

    def test_session_template_is_tokenised(self):
        path = os.path.join(REPO, "site", "config", "session-template.json")
        with open(path) as handle:
            body = handle.read()
        self.assertIn("@HERDR_SITE_WORKDIR@", body)
        for needle in ("jdgeorga", "/pscratch"):
            self.assertNotIn(needle, body)


class ShellSyntaxTest(unittest.TestCase):
    def test_all_shell_files_parse(self):
        targets = [LIB, INSTALL]
        targets += [os.path.join(REPO, "site", "bin", name) for name in WRAPPERS]
        targets.append(os.path.join(REPO, "site", "config", "scripts", "herdr-jobs"))
        targets.append(os.path.join(REPO, "site", "config", "scripts", "herdr-spinner"))
        for target in targets:
            code, out, err = run_bash('bash -n "%s"' % target)
            self.assertEqual(code, 0, "%s: %s" % (target, err))

    def test_wrappers_are_executable(self):
        for name in WRAPPERS:
            path = os.path.join(REPO, "site", "bin", name)
            self.assertTrue(os.access(path, os.X_OK), path)
        for name in ("install.sh",):
            self.assertTrue(os.access(os.path.join(REPO, "site", name), os.X_OK))


class InstallTest(unittest.TestCase):
    def setUp(self):
        self.home = tempfile.mkdtemp(prefix="herdr-site-home.")
        self.addCleanup(shutil.rmtree, self.home, True)

    def install(self, *args):
        argv = " ".join('"%s"' % a for a in args)
        return run_bash(
            '"%s" %s' % (INSTALL, argv),
            env={"HOME": self.home, "HERDR_SITE_ENV": "/nonexistent/site.env"},
        )

    def dest(self, relative):
        return os.path.join(self.home, relative)

    def test_dry_run_changes_nothing(self):
        code, out, err = self.install("--dry-run")
        self.assertEqual(code, 0, err)
        self.assertIn("dry run", out)
        for relative in LINK_DESTS:
            self.assertFalse(
                os.path.lexists(self.dest(relative)), relative + " was created"
            )

    def test_install_creates_every_link_pointing_into_the_repo(self):
        code, out, err = self.install()
        self.assertEqual(code, 0, err + out)
        for relative in LINK_DESTS:
            path = self.dest(relative)
            self.assertTrue(os.path.islink(path), relative + " is not a symlink")
            self.assertTrue(
                os.path.realpath(path).startswith(REPO),
                "%s points outside the repo" % relative,
            )
            self.assertTrue(os.path.exists(path), relative + " is a broken link")

    def test_install_is_idempotent(self):
        self.install()
        code, out, err = self.install()
        self.assertEqual(code, 0, err)
        self.assertNotIn("relink", out)
        self.assertNotIn("backup", out)

    def test_herdr_is_not_linked_unless_asked(self):
        self.install()
        self.assertFalse(os.path.lexists(self.dest(".local/bin/herdr")))
        code, out, err = self.install("--link-herdr")
        self.assertEqual(code, 0, err)
        self.assertTrue(os.path.islink(self.dest(".local/bin/herdr")))

    def test_pre_existing_file_is_backed_up_not_clobbered(self):
        target = self.dest(".local/bin/herdr-health")
        os.makedirs(os.path.dirname(target))
        with open(target, "w") as handle:
            handle.write("#!/bin/bash\n# my own version\n")
        code, out, err = self.install()
        self.assertEqual(code, 0, err)
        backup = target + ".bak.pre-site-install"
        self.assertTrue(os.path.isfile(backup))
        with open(backup) as handle:
            self.assertIn("my own version", handle.read())
        self.assertTrue(os.path.islink(target))

    def test_second_install_refuses_rather_than_overwrite_a_backup(self):
        target = self.dest(".local/bin/herdr-health")
        os.makedirs(os.path.dirname(target))
        for name in (target, target + ".bak.pre-site-install"):
            with open(name, "w") as handle:
                handle.write("x\n")
        code, out, err = self.install()
        self.assertNotEqual(code, 0)
        self.assertIn("refusing to overwrite", err)

    def test_uninstall_removes_links_and_restores_backups(self):
        target = self.dest(".local/bin/herdr-health")
        os.makedirs(os.path.dirname(target))
        with open(target, "w") as handle:
            handle.write("# my own version\n")
        self.install()
        code, out, err = self.install("--uninstall")
        self.assertEqual(code, 0, err)
        for relative in LINK_DESTS:
            path = self.dest(relative)
            if relative.endswith("herdr-health"):
                continue
            self.assertFalse(os.path.lexists(path), relative + " survived uninstall")
        self.assertTrue(os.path.isfile(target))
        self.assertFalse(os.path.islink(target))
        with open(target) as handle:
            self.assertIn("my own version", handle.read())

    def test_uninstall_leaves_a_foreign_symlink_alone(self):
        target = self.dest(".local/bin/herdr-ls")
        os.makedirs(os.path.dirname(target))
        os.symlink("/bin/true", target)
        code, out, err = self.install("--uninstall")
        self.assertEqual(code, 0, err)
        self.assertTrue(os.path.islink(target))
        self.assertEqual(os.readlink(target), "/bin/true")

    def test_session_template_is_seeded_once_and_rendered(self):
        code, out, err = self.install()
        self.assertEqual(code, 0, err)
        seeded = self.dest(".config/herdr-slurm/session-template.json")
        self.assertTrue(os.path.isfile(seeded))
        self.assertFalse(os.path.islink(seeded), "template must be a real file")
        with open(seeded) as handle:
            body = handle.read()
        self.assertNotIn("@HERDR_SITE_WORKDIR@", body)

        with open(seeded, "w") as handle:
            handle.write("{}\n")
        self.install()
        with open(seeded) as handle:
            self.assertEqual(handle.read(), "{}\n", "template was overwritten")

    def test_install_reports_resolved_site_values(self):
        code, out, err = self.install()
        self.assertEqual(code, 0, err)
        for label in ("REPO", "FORK_BIN", "LOGIN_PREFIX", "PYTHON", "CGROUP_MODE"):
            self.assertIn(label, out)


if __name__ == "__main__":
    unittest.main()
