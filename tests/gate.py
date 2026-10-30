"""The shared CI's gate step, eval-apart: nix evaluates test flakes for real, in
a store of its own, with stand-ins for `nix build` and `nix flake check`. Each
derivation gets a nix process of its own and stays a GC root until it's built,
only the checks are built, every output is checked as nix flake check checks
it, a failed evaluation or build fails the gate, every evaluation reads one
snapshot of the checkout, and a flake with an output eval-apart doesn't check,
or with a nixConfig, is refused rather than half checked.

  python3 -I tests/gate.py .github/workflows/flake-check.yml
  nix build .#checks.x86_64-linux.gate
"""

import glob
import os
import re
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
# sandbox; builds and flake checks are only logged, and fail on request.
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
stub("nix", f"""case $1 in
  build)
    echo "$*" >> "$NIX_LOG"
    {real_bin}/nix-store --gc > /dev/null 2>&1
    for a in "$@"; do
      case $a in /nix/store/*) {real_bin}/nix-store --check-validity "${{a%^*}}" || exit 1 ;; esac
    done
    [ -z "$BUILD_FAIL" ] || case "$*" in *"$BUILD_FAIL"*) exit 1 ;; esac
    exit 0 ;;
  flake) [ "$2" = check ] && {{ echo "$*" >> "$NIX_LOG"; exit 0; }} ;;
  # Each evaluation, logged as the plan or the entry it evaluates.
  eval)
    what=plan
    for a in "$@"; do
      case $a in *"; entry = "*) n=${{a##*entry = }}; what="entry ${{n%%;*}}" ;; esac
    done
    echo "eval $what" >> "$NIX_LOG"
    # The checkout edited once the plan is made, if asked.
    [ -z "$EDIT" ] || [ "$what" != "entry 0" ] || cp -- "$EDIT" "$EDIT_TO" ;;
esac
exec {real_nix} "$@"
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
    assert not glob.glob(os.path.join(tmp, "eval-apart.*")), "GC roots or the snapshot left behind"
    return p.returncode, p.stdout + p.stderr, calls


def entries(calls):
    return [c for c in calls if c.startswith("eval entry ")]


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
    assert calls[0] == "eval plan", calls
    assert entries(calls) == [f"eval entry {i}" for i in range(6)], calls
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
def refused_outputs():
    "an output nix flake check checks some other way is refused, before anything is evaluated"
    rc, out, calls = run(flake("""{
      checks.${system}.a = drv "a";
      overlays.o = final: prev: { };
      apps.${system}.x = { type = "app"; program = "/bin/sh"; };
      lib.f = x: x;
    }"""))
    assert rc != 0 and 'this flake has ["apps","overlays"]' in out, out
    assert not entries(calls) and not builds(calls), calls


@case
def other_outputs():
    "modules, templates, lib and unknown outputs are checked as nix flake check checks them"
    rc, out, calls = run(flake("""{
      checks.${system}.a = drv "a";
      nixosModules = { m = { }; f = { config, ... }: throw "not applied"; };
      templates.t = { path = ./.; description = "a template"; welcomeText = "hi"; };
      lib.deep = throw "not forced";
      foo = { };
      "" = { };
    }"""))
    assert rc == 0, out
    assert "warning: unknown flake output 'foo'" in out and "warning: unknown flake output ''" in out, out
    assert "warning: unknown flake output 'lib'" not in out, out
    assert len(entries(calls)) == 1 and builds(calls), calls
    for outputs, error in (('nixosModules.bad = throw "bad module";', "bad module"),
                           ("nixosModules = null;", "flake output 'nixosModules' is not an attribute set"),
                           ('lib = throw "bad lib";', "bad lib"),
                           ('foo = throw "bad foo";', "bad foo"),
                           ("templates = null;", "flake output 'templates' is not an attribute set"),
                           ("templates.t = null;", "flake output 'templates.t' is not an attribute set"),
                           ('templates.t.description = "d";', "template 'templates.t' lacks attribute 'path'"),
                           ('templates.t = { path = ./nowhere; description = "d"; };', "refers to a non-existent path"),
                           ("templates.t.path = ./.;", "template 'templates.t' lacks attribute 'description'"),
                           ("templates.t = { path = ./.; description = 1; };", "is not a string without context"),
                           ('templates.t = { path = ./.; description = "d"; extra = 1; };', "has unsupported attribute 'extra'")):
        rc, out, calls = run(flake(f"{{ checks.${{system}}.a = drv \"a\"; {outputs} }}"))
        assert rc != 0 and error in out, (outputs, out)
        assert not entries(calls), (outputs, calls)


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
            ('checks.${system} = { ok = drv "ok"; fake = { type = "derivation"; name = "x"; drvPath = "/nix/store/00000000000000000000000000000000-x.drv"; }; };',
             "", "is not valid"),
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
def invalid_derivations():
    "a derivation nix flake check wouldn't read fails it, as a check built or skipped, a package, a dev shell or a formatter"
    for value, error in (
            ('drv "x" // { name = throw "bad name"; }', "bad name"),
            ('removeAttrs (drv "x") [ "name" ]', "derivation name missing"),
            ('drv "x" // { name = 1; }', "is not a string without context"),
            ('drv "x" // { name = "${drv "y"}"; }', "is not a string without context"),
            ('drv "x" // { drvPath = throw "bad drvPath"; }', "bad drvPath"),
            ('drv "x" // { drvPath = ""; }', "is not a store derivation's path"),
            ('drv "x" // { drvPath = (drv "x").drvPath + "\\n"; }', "is not a store derivation's path"),
            ('drv "x" // { drvPath = " " + (drv "x").drvPath; }', "is not a store derivation's path"),
            ('drv "x" // { drvPath = (drv "x").outPath; }', "is not a store derivation's path"),
            ('drv "x" // { drvPath = "/elsewhere/00000000000000000000000000000000-x.drv"; }', "is not a store derivation's path"),
            ('drv "x" // { drvPath = 1; }', "cannot coerce"),
            ('drv "x" // { drvPath = { outPath = "/nix/store/x.drv"; }; }', "is not a store derivation's path"),
            ('drv "x" // { drvPath = ./flake.nix; }', "is not a store derivation's path"),
            ('drv "x" // { drvPath = "/nix/store/00000000000000000000000000000000-${builtins.concatStringsSep "" (builtins.genList (_: "x") 208)}.drv"; }',
             "is not a store derivation's path")):
        ok = 'checks.${system}.ok = drv "ok";'
        for where, skip in ((f'checks.${{system}} = {{ ok = drv "ok"; fake = {value}; }};', ""),
                            (f'checks.${{system}} = {{ ok = drv "ok"; fake = {value}; }};', "fake"),
                            (f"{ok} packages.${{system}}.fake = {value};", ""),
                            (f"{ok} devShells.${{system}}.fake = {value};", ""),
                            (f"{ok} formatter.${{system}} = {value};", "")):
            # A NixOS system alongside, whose own result is no derivation path.
            rc, out, calls = run(flake(f"""{{
              nixosConfigurations.box.config.system.build.toplevel = drv "box";
              {where}
            }}"""), skip=skip)
            assert rc != 0 and error in out, (value, where, skip, out)
            assert any("-ok.drv^*" in b for b in builds(calls)), (value, where, out, calls)


@case
def valid_derivations():
    "a derivation nix flake check reads passes, its name dotted or its drvPath a path or an attribute set, wherever it is"
    for value in ('drv ".check"', 'drv "x" // { drvPath = { outPath = (drv "x").drvPath; }; }',
                  'drv "x" // { drvPath = { outPath = /. + builtins.unsafeDiscardStringContext (drv "x").drvPath; }; }',
                  'drv "x" // { drvPath = /. + builtins.unsafeDiscardStringContext (drv "x").drvPath; }'):
        for where, skip in ((f'checks.${{system}}.fake = {value};', ""), (f'checks.${{system}}.fake = {value};', "fake"),
                            (f"packages.${{system}}.fake = {value};", ""), (f"devShells.${{system}}.fake = {value};", ""),
                            (f"formatter.${{system}} = {value};", "")):
            rc, out, calls = run(flake(f"{{ {where} }}"), skip=skip)
            assert rc == 0, (value, where, skip, out)


def git(d, *args):
    subprocess.run(["git", "-c", "user.name=gate", "-c", "user.email=gate@example.org",
                    "-c", "protocol.file.allow=always", *args], cwd=d, check=True, capture_output=True,
                   env={**os.environ, "HOME": tmp})


@case
def repository_settings():
    "the repository's own settings hold in the snapshot: a mode git is told to ignore leaves the checkout clean"
    d = flake("""{
      checks.${system}.meta = if self ? rev then drv "clean" else throw "seen as dirty";
    }""")
    git(d, "config", "core.filemode", "false")
    os.chmod(os.path.join(d, "flake.nix"), 0o755)
    rc, out, calls = run(d)
    assert rc == 0, out
    [b] = builds(calls)
    assert "-clean.drv^*" in b, b


@case
def submodules():
    "a dirty checkout's submodules are in the snapshot as nix reads them, checked out, uncommitted changes and all"
    sub = tempfile.mkdtemp(dir=tmp)
    open(os.path.join(sub, "file"), "w").write("committed")
    git(sub, "init", "-q")
    git(sub, "add", "file")
    git(sub, "commit", "-qm", "sub")
    d = flake("""{
      checks.${system}.sub = drv (builtins.readFile ./sub/file);
    }""", top="inputs.self.submodules = true;")
    git(d, "submodule", "add", "-q", sub, "sub")
    git(d, "commit", "-qm", "submodule")
    open(os.path.join(d, "sub", "file"), "w").write("changed")
    with open(os.path.join(d, "flake.nix"), "a") as f:
        f.write("# changed\n")
    rc, out, calls = run(d)
    assert rc == 0, out
    [b] = builds(calls)
    assert "-changed.drv^*" in b, b


@case
def snapshot():
    "an edit to the checkout while the gate runs changes nothing it evaluates, and nothing writes the checkout"
    d = flake("""{ checks.${system} = { a = drv "a"; b = drv "b"; }; }""")
    edit = os.path.join(tmp, "edited.nix")
    open(edit, "w").write(f"""{{
  outputs = {{ self }}: {{
    checks.{system} = {{
      b = derivation {{ name = "b2"; system = "{system}"; builder = "/bin/sh"; args = [ "-c" "echo > $out" ]; }};
      c = derivation {{ name = "c"; system = "{system}"; builder = "/bin/sh"; args = [ "-c" "echo > $out" ]; }};
    }};
  }};
}}
""")
    rc, out, calls = run(d, skip="a", env={"EDIT": edit, "EDIT_TO": os.path.join(d, "flake.nix")})
    assert rc == 0, out
    assert open(os.path.join(d, "flake.nix")).read() == open(edit).read(), "the edit wasn't made"
    [b] = builds(calls)
    assert "-b.drv^*" in b and "-a.drv" not in b and "-b2.drv" not in b and "-c.drv" not in b, b
    # A lock file that needs changing fails it, and isn't written.
    dep = flake("{ }")
    rc, out, calls = run(flake(f"""{{ checks.${{system}}.a = drv "a"; }}""", top=f'inputs.dep.url = "path:{dep}";'))
    assert rc != 0 and not builds(calls), out
    assert "lock file" in out, out


@case
def committed():
    "a clean checkout is evaluated with its commit as self's"
    d = flake("""{
      checks.${system}.meta = if self ? rev && self ? revCount && self.lastModified > 0
        then drv "meta-${self.shortRev}" else throw "lost the commit";
    }""")
    rev = subprocess.run(["git", "rev-parse", "--short=7", "HEAD"], cwd=d, capture_output=True, text=True, check=True).stdout.strip()
    rc, out, calls = run(d)
    assert rc == 0, out
    [b] = builds(calls)
    assert f"-meta-{rev}.drv^*" in b, (rev, b)


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
    rc, out, calls = run(d, skip="a")
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
def uncommitted():
    "a checkout with changes not yet committed is evaluated as it is, purely, with the source's own metadata"
    d = flake("""{ checks.${system}.a = drv "a"; }""")
    open(os.path.join(d, "flake.nix"), "w").write(f"""{{
  outputs = {{ self }}: {{
    checks.{system}.changed = derivation {{ name = "changed"; system = "{system}"; builder = "/bin/sh"; args = [ "-c" "echo > $out" ]; }};
    checks.{system}.pure = if builtins.getEnv "GATE_ENV" == "" then throw "only pure" else null;
    # What nix flake check gives self for a checkout with changes.
    checks.{system}.meta = if self ? dirtyShortRev && self.lastModified > 0
      then derivation {{ name = "meta-${{builtins.replaceStrings [ "-" ] [ "" ] self.dirtyShortRev}}"; system = "{system}"; builder = "/bin/sh"; args = [ "-c" "echo > $out" ]; }}
      else throw "lost the source's metadata";
  }};
}}
""")
    rc, out, calls = run(d, env={"GATE_ENV": "set"})
    assert rc != 0 and "only pure" in out, out
    [b] = builds(calls)
    assert "-changed.drv^*" in b, b
    assert re.search(r"-meta-[0-9a-f]+dirty\.drv\^\*", b), b


@case
def no_checks():
    "no checks, no build"
    rc, out, calls = run(flake("""{ packages.${system}.p = drv "p"; }"""))
    assert rc == 0 and len(entries(calls)) == 1 and not builds(calls), (out, calls)


@case
def whole():
    "without eval-apart, nix flake check"
    rc, _, calls = run(every, apart=False)
    assert rc == 0 and calls == ["flake check -L --keep-going"], calls


if failures:
    sys.exit(f"failed: {', '.join(failures)}")
