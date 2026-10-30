"""The shared CI's gate step: nix evaluates test flakes for real, in a store of
its own, with stand-ins for `nix build` and `nix flake check`. With eval-apart,
each derivation gets a nix process of its own and stays a GC root until it's
built, only the checks are built, every output is checked as nix flake check
checks it, a failed evaluation or build fails the gate, and a flake with an
output eval-apart doesn't know, or with a nixConfig, is refused rather than
half checked. Without it, the checks are built first, by derivation, kept as
GC roots, once the one nix process that evaluated them has exited, so no
evaluation stays in memory beside the builds; nix flake check then finds them
built. A flake with a nixConfig is left to nix flake check alone.

  python3 -I tests/gate.py .github/workflows/flake-check.yml
  nix build .#checks.x86_64-linux.gate
"""

import glob
import os
import shutil
import subprocess
import sys
import tempfile

import yaml

job = yaml.safe_load(open(sys.argv[1]))["jobs"]["check"]
step = next(s for s in job["steps"] if s.get("id") == "gate")
assert step["shell"] == "bash"
assert step["env"]["APART"] == "${{ inputs.eval-apart }}", step["env"]
assert step["env"]["SKIP"] == "${{ inputs.build-skip }}", step["env"]

tmp = tempfile.mkdtemp()
script = os.path.join(tmp, "gate.sh")
open(script, "w").write(step["run"])

# Real nix for evaluation, in a store under tmp, so this runs in the build
# sandbox; builds are only logged, and fail on request.
real_nix = shutil.which("nix")
real_bin = os.path.dirname(real_nix)
nix_env = {
    "HOME": tmp,
    "XDG_CACHE_HOME": os.path.join(tmp, "cache"),
    "NIX_CONFIG": "\n".join([
        "experimental-features = nix-command flakes",
        "flake-registry =",
        f"store = {os.path.join(tmp, 'store')}",
    ]),
}
bin_dir = os.path.join(tmp, "bin")
os.mkdir(bin_dir)
bash = shutil.which("bash")


def stub(name, body):
    path = os.path.join(bin_dir, name)
    open(path, "w").write(f"#!{bash}\n{body}")
    os.chmod(path, 0o755)


# A build collects garbage first, as a GC on the runner could between the
# evaluations and the build, and every derivation it's given must survive it.
# A flake check evaluates as nix's does, and builds nothing.
stub("nix", f"""case $1 in
  build)
    echo "$*" >> "$NIX_LOG"
    {real_bin}/nix-store --gc > /dev/null 2>&1
    for a in "$@"; do
      case $a in /nix/store/*) {real_bin}/nix-store --check-validity "${{a%^*}}" || exit 1 ;; esac
    done
    [ -z "$BUILD_FAIL" ] || case "$*" in *"$BUILD_FAIL"*) exit 1 ;; esac
    exit 0 ;;
  flake) [ "$2" = check ] && {{ echo "$*" >> "$NIX_LOG"; exec {real_nix} flake check --no-build "${{@:3}}"; }} ;;
esac
exec {real_nix} "$@"
""")
# Each evaluation, logged as the plan or the entry it evaluates.
stub("nix-instantiate", f"""args=("$@")
what=plan
while [ $# -gt 0 ]; do
  case $1 in
    --arg) [ "$2" = entry ] && what="entry $3"; shift 3 ;;
    --argstr | --option) shift 3 ;;
    --add-root) case $2 in */check) what=checks ;; esac; shift 2 ;;
    --expr) shift 2 ;;
    *) shift ;;
  esac
done
echo "nix-instantiate $what" >> "$NIX_LOG"
exec {real_bin}/nix-instantiate "${{args[@]}}"
""")

system = subprocess.run([real_nix, "config", "show", "system"],
                        env={**os.environ, **nix_env}, capture_output=True, text=True, check=True).stdout.strip()


def flake(outputs, top=""):
    """A test flake, committed to git as a checkout is."""
    d = tempfile.mkdtemp(dir=tmp)
    open(os.path.join(d, "flake.nix"), "w").write(f"""{{
  {top}
  outputs = {{ self }}:
    let
      system = "{system}";
      drv = name: derivation {{ inherit name system; builder = "/bin/sh"; args = [ "-c" "echo > $out" ]; }};
    in
    {outputs};
}}
""")
    git = ["git", "-c", "user.name=gate", "-c", "user.email=gate@example.org", "-c", "init.defaultBranch=main"]
    for cmd in (["init", "-q"], ["add", "flake.nix"], ["commit", "-qm", "flake"]):
        subprocess.run(git + cmd, cwd=d, check=True, env={**os.environ, "HOME": tmp})
    return d


def run(directory, apart=True, skip="", build_fail="", env=None):
    log = os.path.join(tmp, "nix.log")
    if os.path.exists(log):
        os.remove(log)
    full = {**nix_env, "PATH": bin_dir + os.pathsep + os.environ["PATH"], "NIX_LOG": log,
            "APART": "true" if apart else "false", "SKIP": skip, "BUILD_FAIL": build_fail,
            "RUNNER_TEMP": tmp, **(env or {})}
    # GitHub runs `shell: bash` as this.
    p = subprocess.run([bash, "--noprofile", "--norc", "-eo", "pipefail", script],
                       cwd=directory, env=full, capture_output=True, text=True)
    calls = open(log).read().splitlines() if os.path.exists(log) else []
    # The roots last only as long as the gate.
    assert not glob.glob(os.path.join(tmp, "roots.*")), "GC roots left behind"
    return p.returncode, p.stdout + p.stderr, calls


def entries(calls):
    return [c for c in calls if c.startswith("nix-instantiate entry ")]


def builds(calls):
    return [c for c in calls if c.startswith("build ")]


failures = []


def case(f):
    """Each case runs, and reports, whether or not the others pass."""
    try:
        f()
        print(f"{f.__doc__}: ok")
    except AssertionError as e:
        print(f"{f.__doc__}: FAILED {e}")
        failures.append(f.__doc__)


every = flake("""{
      checks.${system} = { a = drv "a"; vm = drv "vm"; };
      packages.${system}.p = drv "p";
      devShells.${system}.default = drv "shell";
      formatter.${system} = drv "fmt";
      nixosConfigurations.box.config.system.build.toplevel = drv "box";
      # The gate never evaluates legacyPackages; nor does nix flake check.
      legacyPackages.${system}.huge = throw "evaluated legacyPackages";
      # Nor any other system's outputs.
      checks.other-system.broken = throw "evaluated another system";
    }""")


@case
def each_apart():
    "each derivation on its own, kept until built, then only the checks built"
    rc, out, calls = run(every, skip="vm")
    assert rc == 0, out
    # One plan, then one evaluation per derivation.
    assert calls[0] == "nix-instantiate plan", calls
    assert entries(calls) == [f"nix-instantiate entry {i}" for i in range(6)], calls
    for what in (f"checks.{system}.a", f"checks.{system}.vm", f"packages.{system}.p",
                 f"devShells.{system}.default", f"formatter.{system}", "nixosConfigurations.box"):
        assert f"evaluating {what}\n" in out, (what, out)
    # What it builds is the derivation the evaluation gave, outputs and all.
    [b] = builds(calls)
    assert b.startswith("build -L --no-link --keep-going /nix/store/") and b.endswith("-a.drv^*"), b
    assert "-vm.drv" not in b and "-p.drv" not in b, b
    assert f"not building checks.{system}.vm" in out, out


@case
def failing_build():
    "a failing check fails it"
    rc, out, calls = run(every, build_fail="-a.drv")
    assert rc != 0 and builds(calls), out


@case
def failing_eval():
    "a check that doesn't evaluate fails it, once the rest are evaluated and built"
    rc, out, calls = run(flake("""{
      checks.${system} = { a = drv "a"; broken = throw "broken check"; };
      packages.${system}.p = drv "p";
    }"""))
    assert rc != 0 and "broken check" in out, out
    assert len(entries(calls)) == 3, calls
    [b] = builds(calls)
    assert "-a.drv^*" in b, b


@case
def unknown_outputs():
    "an output it doesn't know is refused, before anything is evaluated"
    rc, out, calls = run(flake("""{
      checks.${system}.a = drv "a";
      nixosModules.m = { };
      overlays.o = final: prev: { };
      "" = { };
    }"""))
    assert rc != 0 and 'this flake also has ["","nixosModules","overlays"]' in out, out
    assert not entries(calls) and not builds(calls), calls


@case
def nix_config():
    "a flake with a nixConfig is refused, before anything is evaluated"
    d = flake("""{ checks.${system}.a = drv "a"; }""", top="nixConfig.allow-import-from-derivation = false;")
    rc, out, calls = run(d, env={"NIX_CONFIG": nix_env["NIX_CONFIG"] + "\naccept-flake-config = true"})
    assert rc != 0 and "can't carry this flake's nixConfig" in out, out
    assert not entries(calls) and not builds(calls), calls


@case
def malformed_outputs():
    "a malformed output fails it, as nix flake check fails it"
    for outputs, error in (("checks = null;", "flake output 'checks' is not an attribute set"),
                           ("formatter = null;", "flake output 'formatter' is not an attribute set"),
                           ("packages = 1;", "flake output 'packages' is not an attribute set"),
                           ("devShells.${system} = null;", f"flake output 'devShells.{system}' is not an attribute set"),
                           ("nixosConfigurations = null;", "flake output 'nixosConfigurations' is not an attribute set"),
                           ("legacyPackages = null;", "flake output 'legacyPackages' is not an attribute set"),
                           ("legacyPackages.nodash = { };", "'nodash' in flake output 'legacyPackages' is not a valid system type"),
                           ("packages.nodash = { };", "'nodash' in flake output 'packages' is not a valid system type"),
                           ('devShells.nodash = throw "evaluated it";', "'nodash' in flake output 'devShells' is not a valid system type")):
        base = "checks.${system}.ok = drv \"ok\";" if outputs.startswith("packages") else "packages.${system}.p = drv \"p\";"
        rc, out, calls = run(flake(f"{{ {base} {outputs} }}"))
        assert rc != 0 and error in out, (outputs, out)
        assert not entries(calls), (outputs, calls)


@case
def not_derivations():
    "anything but a derivation fails it, built or not"
    fake = f"flake attribute 'checks.{system}.fake' is not a derivation"
    for outputs, skip, error in (
            ('checks.${system} = { ok = drv "ok"; fake = { drvPath = ""; }; };', "", fake),
            ('checks.${system} = { ok = drv "ok"; fake = { drvPath = ""; }; };', "fake", fake),
            ('checks.${system} = { ok = drv "ok"; fake = { type = "derivation"; name = "x"; }; };', "", "drvPath"),
            ('checks.${system} = { ok = drv "ok"; fake = { type = "derivation"; name = "x"; drvPath = "/nix/store/x"; }; };',
             "", "is not in the Nix store"),
            ('checks.${system} = { ok = drv "ok"; fake = { type = "derivation"; name = "x"; drvPath = "/nix/store/00000000000000000000000000000000-x"; }; };',
             "", "is not a valid derivation path"),
            ('checks.${system}.ok = drv "ok"; packages.${system}.fake = "/nix/store/x";', "",
             f"flake attribute 'packages.{system}.fake' is not a derivation"),
            ('checks.${system}.ok = drv "ok"; devShells.${system}.fake = { };', "",
             f"flake attribute 'devShells.{system}.fake' is not a derivation"),
            ('checks.${system}.ok = drv "ok"; formatter.${system} = 1;', "",
             f"flake attribute 'formatter.{system}' is not a derivation"),
            ('checks.${system}.ok = drv "ok"; nixosConfigurations.box.config.system.build.toplevel = { };', "",
             "'config.system.build.toplevel' of nixosConfigurations.box is not a derivation")):
        rc, out, calls = run(flake(f"{{ {outputs} }}"), skip=skip)
        assert rc != 0 and error in out, (outputs, out)
        # The rest are still built.
        assert any("-ok.drv^*" in b for b in builds(calls)), (outputs, calls)


@case
def pure():
    "the plan is as pure as the evaluations: the runner's environment hides no check"
    rc, out, _ = run(flake("""{
      checks.${system} = if builtins.getEnv "GATE_ENV" == "" then { a = drv "a"; pure = throw "only pure"; } else { a = drv "a"; };
    }"""), env={"GATE_ENV": "set"})
    assert rc != 0 and "only pure" in out, out


@case
def names():
    "every check is built under its own name, whatever the name"
    d = flake(r"""{
      checks.${system} = {
        a = drv "n0"; "a.b" = drv "n1"; "back\\slash" = drv "n2"; "quo\"te" = drv "n3";
        "sp ace" = drv "n4"; "tab\there" = drv "n5"; "new\nline" = drv "n6"; "$(touch pwned)" = drv "n7";
      };
    }""")
    for apart in (True, False):
        rc, out, calls = run(d, apart=apart, skip="a")
        assert rc == 0, out
        [b] = builds(calls)
        assert "-n0.drv" not in b, b
        for n in range(1, 8):
            assert f"-n{n}.drv^*" in b, (n, b)
        assert not os.path.exists(os.path.join(d, "pwned")), "a name ran as a command"
    rc, out, calls = run(flake(r"""{ checks.${system} = { "back\\slash" = drv "n2"; ok = drv "ok"; }; }"""),
                         build_fail="-n2.drv")
    assert rc != 0, out


@case
def no_checks():
    "no checks, no build"
    rc, out, calls = run(flake("""{ packages.${system}.p = drv "p"; }"""))
    assert rc == 0 and len(entries(calls)) == 1 and not builds(calls), (out, calls)


@case
def whole():
    "without eval-apart, the checks are built by derivation, kept until built, then nix flake check finds them built"
    rc, out, calls = run(every, apart=False)
    assert rc == 0, out
    assert calls[0] == "nix-instantiate checks" and calls[2:] == ["flake check -L --keep-going"], calls
    [b] = builds(calls)
    assert b.startswith("build -L --no-link --keep-going /nix/store/"), b
    assert "-a.drv^*" in b and "-vm.drv^*" in b and "-p.drv" not in b, b
    # With a check it doesn't build, nix flake check builds none.
    rc, out, calls = run(every, apart=False, skip="vm")
    assert rc == 0, out
    assert calls[0] == "nix-instantiate checks" and calls[2:] == ["flake check --no-build -L"], calls
    [b] = builds(calls)
    assert b.endswith("-a.drv^*") and "-vm.drv" not in b, b
    assert "not building vm" in out, out


@case
def whole_failing():
    "without eval-apart, a check that fails to build is built once, and one that doesn't evaluate leaves nix flake check the rest"
    rc, out, calls = run(every, apart=False, build_fail="-a.drv")
    assert rc != 0, out
    assert len(builds(calls)) == 1 and calls[-1] == "flake check --no-build -L --keep-going", calls
    broken = flake("""{ checks.${system} = { a = drv "a"; broken = throw "broken check"; }; }""")
    for skip in ("", "x"):
        rc, out, calls = run(broken, apart=False, skip=skip)
        assert rc != 0 and "broken check" in out, out
        assert not builds(calls) and calls[1].startswith("flake check"), calls


@case
def whole_no_checks():
    "without eval-apart, no checks, no build"
    rc, out, calls = run(flake("""{ packages.${system}.p = drv "p"; }"""), apart=False)
    assert rc == 0 and calls == ["nix-instantiate checks", "flake check -L --keep-going"], (out, calls)


@case
def whole_nix_config():
    "without eval-apart, a flake with a nixConfig is nix flake check's alone, which applies the config a runner accepts"
    d = flake("""{ checks.${system} = { a = drv "a"; vm = drv "vm"; }; }""", top='nixConfig.bash-prompt = "x";')
    rc, out, calls = run(d, apart=False)
    assert rc == 0 and calls == ["flake check -L --keep-going"], (out, calls)
    rc, out, calls = run(d, apart=False, skip="vm")
    assert rc == 0 and calls == ["flake check --no-build -L", f"build -L --no-link --keep-going .#checks.{system}.a"], (out, calls)
    rc, out, calls = run(d, apart=False, skip="vm", build_fail=".a")
    assert rc != 0, (out, calls)


@case
def outputs():
    "a check whose first output isn't out is built, every output of it"
    d = flake("""{ checks.${system}.multi = derivation {
      name = "multi"; inherit system; builder = "/bin/sh"; outputs = [ "dev" "out" ]; args = [ "-c" "echo > $dev; echo > $out" ];
    }; }""")
    for apart in (True, False):
        rc, out, calls = run(d, apart=apart)
        assert rc == 0, out
        [b] = builds(calls)
        assert b.endswith("-multi.drv^*"), b


if failures:
    sys.exit(f"failed: {', '.join(failures)}")
