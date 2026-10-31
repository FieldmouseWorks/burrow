"""The shared CI's gate step: nix evaluates test flakes for real, in a store of
its own, with stand-ins for `nix build` and `nix flake check`. With eval-apart,
each derivation gets a nix process of its own and stays a GC root until it's
built, only the checks are built, every output is checked as nix flake check
checks it, a failed evaluation or build fails the gate, every evaluation reads
one snapshot of the checkout, and a flake with an output eval-apart doesn't
check, or with a nixConfig, is refused rather than half checked. Without it,
the checks are built first, by derivation, kept as GC roots, once the one nix
process that evaluated them has exited, so no evaluation stays in memory beside
the builds; nix flake check then finds them built and its outputs still roots.
A flake with a nixConfig is left to nix flake check alone, and the flake is
only ever read purely. Where the machine has heavy's slots (the real heavy, on
a slot of its own), a check with a VM test to run waits for one, once the
others are built, and a runner without them takes none.

  python3 -I tests/gate.py .github/workflows/flake-check.yml modules/cpu/heavy.sh
  nix build .#checks.x86_64-linux.gate
"""

import fcntl
import glob
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time

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
# A flake check evaluates as nix's does, and builds nothing, and whatever a
# build before it kept as a root must still be one.
stub("nix", f"""case $1 in
  build)
    echo "$*" >> "$NIX_LOG"
    [ -z "$HEAVY_SLOT" ] || echo "$*" >> "$NIX_LOG.slot"
    {real_bin}/nix-store --gc > /dev/null 2>&1
    prev=
    for a in "$@"; do
      [ "$prev" = --out-link ] && echo "$a" >> "$NIX_LOG.links"
      prev=$a
      case $a in /nix/store/*) {real_bin}/nix-store --check-validity "${{a%^*}}" || exit 1 ;; esac
    done
    [ -z "$BUILD_FAIL" ] || case "$*" in *"$BUILD_FAIL"*) exit 1 ;; esac
    exit 0 ;;
  flake) [ "$2" = check ] && {{
    echo "$*" >> "$NIX_LOG"
    [ -z "$HEAVY_SLOT" ] || echo "$*" >> "$NIX_LOG.slot"
    if [ -e "$NIX_LOG.links" ]; then
      while read -r link; do [ -d "${{link%/*}}" ] || {{ echo "the built checks' roots went before nix flake check"; exit 1; }}; done < "$NIX_LOG.links"
    fi
    exec {real_nix} flake check --no-build "${{@:3}}"; }} ;;
  # An older nix's JSON, if asked: store paths for names, structured attrs in
  # the env.
  derivation)
    if [ -n "$OLD_JSON" ]; then
      {real_nix} "$@" | jq '.derivations | with_entries(.key = "/nix/store/" + .key | .value |= (
        if .structuredAttrs then .env.__json = (.structuredAttrs | tojson) | del(.structuredAttrs) else . end))'
      exit
    fi ;;
  # Each evaluation apart, logged as the plan or the entry it evaluates.
  eval)
    what=
    for a in "$@"; do
      case $a in
        *"; entry = "*) n=${{a##*entry = }}; what="entry ${{n%%;*}}" ;;
        *'{{ system = "'*) what=plan ;;
      esac
    done
    [ -z "$what" ] || echo "eval $what" >> "$NIX_LOG"
    # The checkout edited once the plan is made, if asked.
    [ -z "$EDIT" ] || [ "$what" != "entry 0" ] || cp -- "$EDIT" "$EDIT_TO" ;;
esac
exec {real_nix} "$@"
""")
# Each evaluation without eval-apart, logged as the plan or the checks: the
# ones given the flake's URL, so not eval-apart's look at its nixConfig or its
# rooting.
stub("nix-instantiate", f"""args=("$@")
what=
while [ $# -gt 0 ]; do
  case $1 in
    --arg) [ "$2" = builds ] && what=checks; shift 3 ;;
    --argstr) [ "$2" = url ] && what=${{what:-plan}}; shift 3 ;;
    --option) shift 3 ;;
    --expr | --add-root) shift 2 ;;
    *) shift ;;
  esac
done
[ -z "$what" ] || echo "nix-instantiate $what" >> "$NIX_LOG"
exec {real_bin}/nix-instantiate "${{args[@]}}"
""")

# Builds are only logged, so whether an output is built is the test's to say.
stub("nix-store", f"""if [ "$1" = --add-root ] && [ -n "$ALL_BUILT" ]; then
  echo "root $2 $5" >> "$NIX_LOG.roots"
  exit 0
fi
if [ "$1" = -q ] && [ "$2" = -R ] && [ -n "$NO_CLOSURE" ]; then exit 1; fi
exec {real_bin}/nix-store "$@"
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
    for f in (log, log + ".links", log + ".slot", log + ".roots"):
        if os.path.exists(f):
            os.remove(f)
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
    except Exception as e:
        print(f"{f.__doc__}: FAILED {e!r}")
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
            ('drv "x" // { drvPath = { __toString = _: "/nix/store/x.drv"; outPath = (drv "x").drvPath; }; }', "is not a store derivation's path"),
            ('drv "x" // { drvPath = { __toString = _: ./flake.nix; }; }', "is not a store derivation's path"),
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
                  'drv "x" // { drvPath = /. + builtins.unsafeDiscardStringContext (drv "x").drvPath; }',
                  'drv "x" // { drvPath = { __toString = _: /. + builtins.unsafeDiscardStringContext (drv "x").drvPath; }; }',
                  'drv "x" // { drvPath = { __toString = _: { outPath = { __toString = _: (drv "x").drvPath; }; }; }; }',
                  'drv "x" // { drvPath = { __toString = _: (drv "x").drvPath; outPath = "/nix/store/x.drv"; }; }'):
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
    "the settings in effect hold in the snapshot, set, included or included if: a mode git is told to ignore leaves the checkout clean"
    user = os.path.join(tmp, ".gitconfig")
    filemode = os.path.join(tmp, "filemode.inc")
    open(filemode, "w").write("[core]\n\tfilemode = false\n")
    for how in ("set", "included", "included if", "the user's, included if"):
        d = flake("""{
          checks.${system}.meta = if self ? rev then drv "clean" else throw "seen as dirty";
        }""")
        if how == "set":
            git(d, "config", "core.filemode", "false")
        elif how == "included":
            # Relative to the config file that includes it.
            shutil.copy(filemode, os.path.join(d, ".git", "filemode.inc"))
            git(d, "config", "include.path", "filemode.inc")
        elif how == "included if":
            git(d, "config", f"includeIf.gitdir:{d}/.path", filemode)
        else:
            # git init sets the repository's own, which would win.
            git(d, "config", "--unset", "core.filemode")
            open(user, "w").write(f'[includeIf "gitdir:{d}/"]\n\tpath = {filemode}\n')
        os.chmod(os.path.join(d, "flake.nix"), 0o755)
        try:
            rc, out, calls = run(d)
        finally:
            if os.path.exists(user):
                os.remove(user)
        assert rc == 0, (how, out)
        [b] = builds(calls)
        assert "-clean.drv^*" in b, (how, b)


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
def relative_submodules():
    "a clean checkout's submodule at a relative URL, with no remote, is fetched from beside the checkout, not the snapshot"
    parent = tempfile.mkdtemp(dir=tmp)
    sub = os.path.join(parent, "sub")
    os.mkdir(sub)
    open(os.path.join(sub, "file"), "w").write("beside")
    git(sub, "init", "-q")
    git(sub, "add", "file")
    git(sub, "commit", "-qm", "sub")
    d = flake("""{
      checks.${system}.sub = drv (builtins.readFile ./sub/file);
    }""", top="inputs.self.submodules = true;")
    top = os.path.join(parent, "top")
    os.rename(d, top)
    git(top, "submodule", "add", "-q", "../sub", "sub")
    git(top, "commit", "-qm", "submodule")
    rc, out, calls = run(top)
    assert rc == 0, out
    [b] = builds(calls)
    assert "-beside.drv^*" in b, b


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
    "without eval-apart, the checks are built by derivation, kept until nix flake check has found them built"
    rc, out, calls = run(every, apart=False)
    assert rc == 0, out
    assert calls[:2] == ["nix-instantiate plan", "nix-instantiate checks"], calls
    assert calls[3:] == ["flake check -L --keep-going"], calls
    [b] = builds(calls)
    assert b.startswith("build -L --out-link ") and "/roots." in b.split()[3], b
    assert "-a.drv^*" in b and "-vm.drv^*" in b and "-p.drv" not in b, b
    # With a check it doesn't build, nix flake check builds none.
    rc, out, calls = run(every, apart=False, skip="vm")
    assert rc == 0, out
    assert calls[:2] == ["nix-instantiate plan", "nix-instantiate checks"], calls
    assert calls[3:] == ["flake check --no-build -L"], calls
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
        assert not builds(calls) and calls[2].startswith("flake check"), calls


@case
def whole_no_checks():
    "without eval-apart, no checks, no build"
    rc, out, calls = run(flake("""{ packages.${system}.p = drv "p"; }"""), apart=False)
    assert rc == 0, out
    assert calls == ["nix-instantiate plan", "nix-instantiate checks", "flake check -L --keep-going"], calls


@case
def whole_nix_config():
    "without eval-apart, a flake with a nixConfig is nix flake check's alone, which applies the config a runner accepts"
    d = flake("""{ checks.${system} = { a = drv "a"; vm = drv "vm"; }; }""", top='nixConfig.bash-prompt = "x";')
    rc, out, calls = run(d, apart=False)
    assert rc == 0 and calls == ["nix-instantiate plan", "flake check -L --keep-going"], (out, calls)
    rc, out, calls = run(d, apart=False, skip="vm")
    assert rc == 0, out
    assert calls == ["nix-instantiate plan", "flake check --no-build -L",
                     f"build -L --no-link --keep-going .#checks.{system}.a"], calls
    rc, out, calls = run(d, apart=False, skip="vm", build_fail=".a")
    assert rc != 0, (out, calls)


@case
def whole_pure():
    "without eval-apart, the flake is only read purely: its flake.nix never sees the runner's environment"
    d = tempfile.mkdtemp(dir=tmp)
    open(os.path.join(d, "flake.nix"), "w").write(
        'builtins.trace "secret=${builtins.getEnv "GATE_SECRET"}" { nixConfig = { }; outputs = { self }: { }; }\n')
    git = ["git", "-c", "user.name=gate", "-c", "user.email=gate@example.org", "-c", "init.defaultBranch=main"]
    for cmd in (["init", "-q"], ["add", "flake.nix"], ["commit", "-qm", "flake"]):
        subprocess.run(git + cmd, cwd=d, check=True, env={**os.environ, "HOME": tmp})
    for skip in ("", "x"):
        rc, out, calls = run(d, apart=False, skip=skip, env={"GATE_SECRET": "hunter2"})
        assert rc != 0 and "hunter2" not in out, out


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


# heavy itself, as burrow.cpu assembles it, in a directory of one slot.
slots = os.path.join(tmp, "slots")
os.mkdir(slots)
slot = os.path.join(slots, "slot1")
open(slot, "w").close()
slotted_bin = os.path.join(tmp, "slotted")
os.mkdir(slotted_bin)
heavy = os.path.join(slotted_bin, "heavy")
open(heavy, "w").write(f"#!{bash}\nset -euo pipefail\nSLOTS=1\nGRACE=3\nDIR={slots}\n" + open(sys.argv[2]).read())
os.chmod(heavy, 0o755)
vms = flake("""{
      checks.${system} = {
        a = drv "a";
        needs-kvm = derivation { name = "needs-kvm"; inherit system; builder = "/bin/sh"; args = [ "-c" "echo > $out" ]; requiredSystemFeatures = [ "kvm" "nixos-test" ]; };
        structured = derivation { name = "structured"; inherit system; builder = "/bin/sh"; args = [ "-c" "echo > $out" ]; __structuredAttrs = true; requiredSystemFeatures = [ "kvm" ]; };
        # A check around a VM test builds it too.
        around = derivation { name = "around"; inherit system; builder = "/bin/sh"; args = [ "-c" "echo > $out" ];
          test = derivation { name = "inner"; inherit system; builder = "/bin/sh"; args = [ "-c" "echo > $out" ]; requiredSystemFeatures = [ "nixos-test" ]; }; };
      };
    }""")
with_slots = {"PATH": slotted_bin + os.pathsep + bin_dir + os.pathsep + os.environ["PATH"]}
VM_CHECKS = ("-needs-kvm.drv^*", "-structured.drv^*", "-around.drv^*")


def in_slot():
    path = os.path.join(tmp, "nix.log.slot")
    return open(path).read().splitlines() if os.path.exists(path) else []


def split(calls, out):
    """The check with no VM built first and with no slot, the VM tests after it, in one."""
    plain, vm = builds(calls)
    assert plain.endswith("-a.drv^*") and not any(c in plain for c in VM_CHECKS), plain
    assert all(c in vm for c in VM_CHECKS) and "-a.drv" not in vm, vm
    assert in_slot() == [vm], (in_slot(), vm)
    assert vm.split("--max-jobs ")[1].isdigit(), vm
    assert "waiting for a heavy slot for 3 checks" in out, out


@case
def waits_for_slot():
    "while every slot is held, the gate builds what needs no VM and its VM tests wait, then build holding one"
    log = os.path.join(tmp, "nix.log")
    for f in (log, log + ".links", log + ".slot", log + ".roots"):
        if os.path.exists(f):
            os.remove(f)
    full = {**nix_env, **with_slots, "NIX_LOG": log, "APART": "true", "SKIP": "", "BUILD_FAIL": "",
            "RUNNER_TEMP": tmp}

    def logged():
        return builds(open(log).read().splitlines()) if os.path.exists(log) else []

    with open(slot) as held:
        fcntl.flock(held, fcntl.LOCK_EX)
        p = subprocess.Popen([bash, "--noprofile", "--norc", "-eo", "pipefail", script], cwd=vms, env=full,
                             stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
        try:
            deadline = time.time() + 300
            while not logged() and p.poll() is None and time.time() < deadline:
                time.sleep(0.2)
            assert p.poll() is None, p.stdout.read()
            # Long enough for a gate with no wait in it to have gone on to its
            # VM tests.
            time.sleep(5)
            assert len(logged()) == 1 and logged()[0].endswith("-a.drv^*"), logged()
        except BaseException:
            p.kill()
            raise
    # The slot's free now.
    out, _ = p.communicate(timeout=300)
    assert p.returncode == 0, out
    assert "all 1 slots busy, waiting" in out, out
    split(open(log).read().splitlines(), out)


@case
def split_with_slots():
    "without eval-apart too, and a failed half fails the gate with the other still built"
    rc, out, calls = run(vms, apart=False, env=with_slots)
    assert rc == 0, out
    split(calls, out)
    # Both halves' outputs stay roots for nix flake check, which then takes no slot.
    assert calls[-1] == "flake check -L --keep-going", calls
    links = [b.split()[3] for b in builds(calls)]
    assert links[1] == links[0] + "-vm", links
    for fail, apart in (("-a.drv", True), ("-needs-kvm.drv", True), ("-a.drv", False), ("-needs-kvm.drv", False)):
        rc, out, calls = run(vms, apart=apart, build_fail=fail, env=with_slots)
        assert rc != 0 and len(builds(calls)) == 2, (fail, apart, out, calls)


@case
def built_vms_wait_for_nothing():
    "a VM test already built, a rerun's, takes no slot, its outputs kept as roots first"
    for apart in (True, False):
        rc, out, calls = run(vms, apart=apart, env={**with_slots, "ALL_BUILT": "1"})
        assert rc == 0, out
        [b] = builds(calls)
        assert all(c in b for c in VM_CHECKS + ("-a.drv^*",)), b
        assert not in_slot(), in_slot()
        roots = open(os.path.join(tmp, "nix.log.roots")).read().splitlines()
        # One each: the three VM tests, the one inside the wrapper among them.
        assert len(roots) == 3 and all(r.split()[2].endswith(("-needs-kvm", "-structured", "-inner")) for r in roots), roots
        assert len({r.split()[1] for r in roots}) == 3, roots


@case
def old_json():
    "an older nix's JSON tells the same VM tests apart, structured attrs and all"
    rc, out, calls = run(vms, env={**with_slots, "OLD_JSON": "1"})
    assert rc == 0, out
    split(calls, out)


@case
def closure_unknown():
    "a check whose closure can't be read builds in the slot"
    rc, out, calls = run(vms, env={**with_slots, "NO_CLOSURE": "1"})
    assert rc == 0, out
    [b] = builds(calls)
    assert in_slot() == [b] and "-a.drv^*" in b, (in_slot(), b)


@case
def unsure_takes_slot():
    "with no telling what's a VM test, every check builds in the slot"
    broken = os.path.join(tmp, "nojq")
    os.makedirs(broken, exist_ok=True)
    jq = os.path.join(broken, "jq")
    open(jq, "w").write(f"""#!{bash}
if [[ "$*" == *requiredSystemFeatures* ]]; then exit 1; fi
exec {shutil.which("jq")} "$@"
""")
    os.chmod(jq, 0o755)
    rc, out, calls = run(vms, env={"PATH": broken + os.pathsep + with_slots["PATH"]})
    assert rc == 0, out
    [b] = builds(calls)
    assert in_slot() == [b], in_slot()


@case
def unsplit_takes_slot():
    "what nix builds by name, a nixConfig flake's, builds in a slot"
    d = flake("""{ checks.${system} = { a = drv "a"; vm = drv "vm"; }; }""", top='nixConfig.bash-prompt = "x";')
    rc, out, calls = run(d, apart=False, env=with_slots)
    assert rc == 0 and len(in_slot()) == 1 and in_slot()[0].startswith("flake check -L --keep-going"), (out, in_slot())
    # Its check that only evaluates takes none; the build does.
    rc, out, calls = run(d, apart=False, skip="vm", env=with_slots)
    assert rc == 0, out
    [line] = in_slot()
    assert line.startswith("build -L --no-link --keep-going"), line


@case
def no_slots():
    "a runner without slots, a hosted one, builds it all at once, as it always has"
    rc, out, calls = run(vms)
    assert rc == 0, out
    [b] = builds(calls)
    assert all(c in b for c in VM_CHECKS + ("-a.drv^*",)) and "--max-jobs" not in b, b
    assert not in_slot() and "heavy slot" not in out, out


if failures:
    sys.exit(f"failed: {', '.join(failures)}")
