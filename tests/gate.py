"""The shared CI's gate step, eval-apart: nix evaluates a test flake for real,
in a store of its own, with stand-ins for `nix build` and `nix flake check`.
Each derivation gets a nix process of its own, only the checks are built, a
failed evaluation or build fails the gate, and a flake with an output
eval-apart doesn't know is refused rather than half checked.

  python3 -I tests/gate.py .github/workflows/flake-check.yml
  nix build .#checks.x86_64-linux.gate
"""

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
# sandbox; builds and flake checks are only logged, and fail on request.
real_nix = shutil.which("nix")
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
nix = os.path.join(bin_dir, "nix")
open(nix, "w").write(f"""#!{shutil.which("bash")}
echo "$*" >> "$NIX_LOG"
case $1 in
  eval) exec {real_nix} "$@" ;;
  build) [ -z "$BUILD_FAIL" ] || case "$*" in *"$BUILD_FAIL"*) exit 1 ;; esac ;;
esac
exit 0
""")
os.chmod(nix, 0o755)

system = subprocess.run([real_nix, "eval", "--impure", "--raw", "--expr", "builtins.currentSystem"],
                        env={**os.environ, **nix_env}, capture_output=True, text=True, check=True).stdout


def flake(outputs):
    d = tempfile.mkdtemp(dir=tmp)
    open(os.path.join(d, "flake.nix"), "w").write(f"""{{
  outputs = {{ self }}:
    let
      system = "{system}";
      drv = name: derivation {{ inherit name system; builder = "/bin/sh"; args = [ "-c" "echo > $out" ]; }};
    in
    {outputs};
}}
""")
    return d


def run(directory, apart=True, skip="", build_fail=""):
    log = os.path.join(tmp, "nix.log")
    if os.path.exists(log):
        os.remove(log)
    env = {**nix_env, "PATH": bin_dir + os.pathsep + os.environ["PATH"], "NIX_LOG": log,
           "APART": "true" if apart else "false", "SKIP": skip, "BUILD_FAIL": build_fail,
           "RUNNER_TEMP": tmp}
    # GitHub runs `shell: bash` as this.
    p = subprocess.run([shutil.which("bash"), "--noprofile", "--norc", "-eo", "pipefail", script],
                       cwd=directory, env=env, capture_output=True, text=True)
    calls = open(log).read().splitlines() if os.path.exists(log) else []
    return p.returncode, p.stdout + p.stderr, calls


def evals(calls):
    return [c for c in calls if c.startswith("eval ") and ".drvPath" in c]


def builds(calls):
    return [c for c in calls if c.startswith("build ")]


every = flake("""{
      checks.${system} = { a = drv "a"; vm = drv "vm"; };
      packages.${system}.p = drv "p";
      devShells.${system}.default = drv "shell";
      formatter.${system} = drv "fmt";
      nixosConfigurations.box.config.system.build.toplevel = drv "box";
      # The gate never evaluates legacyPackages; nor does nix flake check.
      legacyPackages.${system}.huge = throw "evaluated legacyPackages";
    }""")

# Every derivation nix flake check would evaluate, each in a process of its
# own, then one build of the checks that aren't skipped, with no evaluation.
rc, out, calls = run(every, skip="vm")
assert rc == 0, out
assert len(evals(calls)) == 6, calls
for attr in (f'checks.{system}."a"', f'checks.{system}."vm"', f'packages.{system}."p"',
             f'devShells.{system}."default"', f"formatter.{system}",
             'nixosConfigurations."box".config.system.build.toplevel'):
    assert any(f".#{attr}.drvPath" in c for c in evals(calls)), (attr, calls)
# What it builds is the derivation the evaluation gave, outputs and all.
[b] = builds(calls)
assert b.startswith("build -L --no-link --keep-going /nix/store/") and b.endswith("-a.drv^*"), b
assert "-vm.drv" not in b and "-p.drv" not in b, b
assert "not building vm" in out, out
print("each derivation on its own, then only the checks built: ok")

# A failed build fails the gate.
rc, out, calls = run(every, build_fail="-a.drv")
assert rc != 0, out
print("a failing check fails it: ok")

# A failed evaluation fails the gate, once the rest are evaluated and built.
broken = flake("""{
      checks.${system} = { a = drv "a"; broken = throw "broken check"; };
      packages.${system}.p = drv "p";
    }""")
rc, out, calls = run(broken)
assert rc != 0 and "broken check" in out, out
assert len(evals(calls)) == 3, calls
[b] = builds(calls)
assert "-a.drv^*" in b, b
print("a check that doesn't evaluate fails it: ok")

# An output eval-apart doesn't know: refused before anything is evaluated,
# rather than passed with its checks unrun.
other = flake("""{
      checks.${system}.a = drv "a";
      nixosModules.m = { };
      overlays.o = final: prev: { };
    }""")
rc, out, calls = run(other)
assert rc != 0 and "also has: nixosModules overlays" in out, out
assert not evals(calls) and not builds(calls), calls
print("an output it doesn't know is refused: ok")

# Nothing to build is no build at all.
rc, out, calls = run(flake("""{ packages.${system}.p = drv "p"; }"""))
assert rc == 0 and len(evals(calls)) == 1 and not builds(calls), (out, calls)
print("no checks, no build: ok")

# Without eval-apart, the gate is still one nix flake check.
rc, out, calls = run(other, apart=False)
assert rc == 0 and calls == ["flake check -L --keep-going"], calls
print("without eval-apart, nix flake check: ok")
