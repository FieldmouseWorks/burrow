//! The shared CI's gate step: nix evaluates test flakes for real, in a store
//! of its own, with stand-ins for `nix build` and `nix flake check`. With
//! eval-apart, each derivation gets a nix process of its own and stays a GC
//! root until it's built, only the checks are built, every output is checked
//! as nix flake check checks it, a failed evaluation or build fails the gate,
//! every evaluation reads one snapshot of the checkout, and a flake with an
//! output eval-apart doesn't check, or with a nixConfig, is refused rather
//! than half checked. With legs, each leg evaluates its own share of the
//! outputs, those whose names hash to it, and builds only the checks among
//! them. Without it, the checks are built first, by derivation,
//! kept as GC roots, once the one nix process that evaluated them has
//! exited, so no evaluation stays in memory beside the builds; nix flake
//! check then finds them built and its outputs still roots. A flake with a
//! nixConfig is left to nix flake check alone, and the flake is only ever
//! read purely. Where the machine has heavy's slots (the real heavy, on a
//! slot of its own), a check with a VM test to run waits for one, once the
//! others are built, and a runner without them takes none.
//!
//! Each case has a store, and a scratch directory, of its own. The cases are
//! by what they hold the gate to: the outputs it checks, the derivations it
//! takes, the checkout it reads, the gate split across legs, the gate without
//! eval-apart, and the slot its VM tests wait for.

use std::fs;
use std::path::{Path, PathBuf};

use crate::harness::{Scratch, output, read, script, step, which, workflow, write};

mod checkout;
mod derivations;
mod legs;
mod outputs;
mod slots;
mod whole;

/// A case's runner: real nix for evaluation, in a store under its scratch
/// directory, so it runs in the build sandbox; builds are only logged, and
/// fail on request.
struct Gate {
    s: Scratch,
    nix_config: String,
    system: String,
    flakes: std::cell::Cell<usize>,
}

/// What the gate did: its exit code, what it printed, and nix's calls as
/// the stand-ins logged them.
struct Ran {
    code: i32,
    out: String,
    calls: Vec<String>,
}

impl Ran {
    fn entries(&self) -> Vec<&str> {
        self.calls
            .iter()
            .map(String::as_str)
            .filter(|c| c.starts_with("eval entry "))
            .collect()
    }

    fn builds(&self) -> Vec<&str> {
        self.calls
            .iter()
            .map(String::as_str)
            .filter(|c| c.starts_with("build "))
            .collect()
    }

    /// The one build.
    fn build(&self) -> &str {
        match self.builds()[..] {
            [b] => b,
            ref b => panic!("not one build: {b:?}\n{}", self.out),
        }
    }

    fn fails_with(&self, error: &str) -> bool {
        self.code != 0 && self.out.contains(error)
    }
}

/// How a case runs the gate.
#[derive(Default)]
struct Run<'a> {
    whole: bool,
    skip: &'a str,
    build_fail: &'a str,
    /// The leg it runs and how many there are; none for the one leg.
    legs: Option<(&'a str, &'a str)>,
    env: &'a [(&'a str, &'a str)],
}

impl Gate {
    fn new() -> Gate {
        let s = Scratch::default();
        let nix = which("nix");
        let (real_nix, real_bin) = (nix.display(), nix.parent().unwrap().display());
        let nix_config = format!(
            "experimental-features = nix-command flakes\nflake-registry =\nstore = {}",
            s.join("store").display()
        );
        // A build collects garbage first, as a GC on the runner could between
        // the evaluations and the build, and every derivation it's given
        // must survive it. A flake check evaluates as nix's does, and builds
        // nothing, and whatever a build before it kept as a root must still
        // be one.
        s.stub(
            "nix",
            &format!(
                r#"case $1 in
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
"#
            ),
        );
        // Each evaluation without eval-apart, logged as the plan or the
        // checks: the ones given the flake's URL, so not eval-apart's look at
        // its nixConfig or its rooting.
        s.stub(
            "nix-instantiate",
            &format!(
                r#"args=("$@")
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
"#
            ),
        );
        // Builds are only logged, so whether an output is built is the
        // case's to say.
        s.stub(
            "nix-store",
            &format!(
                r#"if [ "$1" = --add-root ] && [ -n "$ALL_BUILT" ]; then
  echo "root $2 $5" >> "$NIX_LOG.roots"
  exit 0
fi
if [ "$1" = -q ] && [ "$2" = -R ] && [ -n "$NO_CLOSURE" ]; then exit 1; fi
exec {real_bin}/nix-store "$@"
"#
            ),
        );
        let mut cmd = s.command(&nix);
        cmd.args(["config", "show", "system"])
            .env("NIX_CONFIG", &nix_config)
            .env("XDG_CACHE_HOME", s.join("cache"));
        let out = output(cmd);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let system = String::from_utf8(out.stdout).unwrap().trim().to_owned();
        Gate {
            s,
            nix_config,
            system,
            flakes: std::cell::Cell::new(0),
        }
    }

    /// A new directory for a flake.
    fn dir(&self) -> PathBuf {
        self.flakes.set(self.flakes.get() + 1);
        let d = self.s.join(&format!("flake{}", self.flakes.get()));
        fs::create_dir(&d).unwrap();
        d
    }

    /// A test flake, committed to git as a checkout is: `top` beside its
    /// outputs, which have `system` and `drv NAME` in scope.
    fn flake_with(&self, outputs: &str, top: &str) -> PathBuf {
        let d = self.dir();
        let system = &self.system;
        write(
            &d.join("flake.nix"),
            &format!(
                r#"{{
  {top}
  outputs = {{ self }}:
    let
      system = "{system}";
      drv = name: derivation {{ inherit name system; builder = "/bin/sh"; args = [ "-c" "echo > $out" ]; }};
    in
    {outputs};
}}
"#
            ),
        );
        self.commit_all(&d);
        d
    }

    fn flake(&self, outputs: &str) -> PathBuf {
        self.flake_with(outputs, "")
    }

    fn commit_all(&self, d: &Path) {
        self.s.git(d, &["init", "-q"]);
        self.s.git(d, &["add", "flake.nix"]);
        self.s.git(d, &["commit", "-qm", "flake"]);
    }

    /// A derivation of this system, for a flake.nix written whole.
    fn drv(&self, name: &str) -> String {
        format!(
            r#"derivation {{ name = "{name}"; system = "{}"; builder = "/bin/sh"; args = [ "-c" "echo > $out" ]; }}"#,
            self.system
        )
    }

    /// Where the stand-ins log nix's calls: `.slot` beside it has those made
    /// in a slot, `.links` the out-links, `.roots` what was rooted.
    fn log(&self) -> PathBuf {
        self.s.join("nix.log")
    }

    /// The gate step's script, and its environment for a run, with the logs
    /// of a run before cleared.
    fn gate_step(&self, r: &Run) -> (String, Vec<(String, String)>) {
        for suffix in ["", ".links", ".slot", ".roots"] {
            let _ = fs::remove_file(self.s.join(&format!("nix.log{suffix}")));
        }
        let wf = workflow(".github/workflows/flake-check.yml");
        let path = |p: PathBuf| p.to_str().unwrap().to_owned();
        let (leg, legs) = r.legs.unwrap_or(("1", "1"));
        // A case's own env comes last, so it wins (PATH among it).
        let env: Vec<(String, String)> = [
            ("NIX_CONFIG", self.nix_config.clone()),
            ("XDG_CACHE_HOME", path(self.s.join("cache"))),
            ("NIX_LOG", path(self.log())),
            ("APART", (if r.whole { "false" } else { "true" }).to_owned()),
            ("SKIP", r.skip.to_owned()),
            ("BUILD_FAIL", r.build_fail.to_owned()),
            ("LEG", leg.to_owned()),
            ("LEGS", legs.to_owned()),
            ("RUNNER_TEMP", path(self.s.path().to_path_buf())),
        ]
        .into_iter()
        .chain(r.env.iter().copied().map(|(k, v)| (k, v.to_owned())))
        .map(|(k, v)| (k.to_owned(), v))
        .collect();
        (script(step(&wf["jobs"]["check"], "gate")).to_owned(), env)
    }

    fn run(&self, dir: &Path, r: Run) -> Ran {
        let (script, env) = self.gate_step(&r);
        let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        let ran = self.s.bash(&script, dir, &env);
        self.ran(ran.code, ran.said())
    }

    /// What a run of the gate came to, from how it ended and what it printed.
    fn ran(&self, code: i32, out: String) -> Ran {
        // The roots last only as long as the gate.
        let left: Vec<_> = fs::read_dir(self.s.path())
            .unwrap()
            .filter_map(|e| e.ok()?.file_name().into_string().ok())
            .filter(|n| n.starts_with("eval-apart."))
            .collect();
        assert!(
            left.is_empty(),
            "GC roots or the snapshot left behind: {left:?}"
        );
        Ran {
            code,
            out,
            calls: self.calls(),
        }
    }

    /// nix's calls so far, as the stand-ins logged them.
    fn calls(&self) -> Vec<String> {
        lines(&self.log())
    }

    fn apart(&self, dir: &Path) -> Ran {
        self.run(dir, Run::default())
    }
}

/// A log's lines, none if it isn't there.
fn lines(path: &Path) -> Vec<String> {
    if path.exists() {
        read(path).lines().map(str::to_owned).collect()
    } else {
        Vec::new()
    }
}

#[test]
fn the_step_takes_its_inputs() {
    let wf = workflow(".github/workflows/flake-check.yml");
    let s = step(&wf["jobs"]["check"], "gate");
    assert_eq!(s["shell"], "bash");
    assert_eq!(
        s["env"],
        crate::harness::yaml(
            "{APART: '${{ inputs.eval-apart }}', SKIP: '${{ inputs.build-skip }}', \
             LEGS: '${{ inputs.legs }}', LEG: '${{ matrix.leg }}'}"
        )
    );
}

/// A flake with an output of every kind the gate checks, and two it never
/// evaluates.
const EVERY: &str = r#"{
      checks.${system} = { a = drv "a"; vm = drv "vm"; };
      packages.${system}.p = drv "p";
      devShells.${system}.default = drv "shell";
      formatter.${system} = drv "fmt";
      nixosConfigurations.box.config.system.build.toplevel = drv "box";
      # The gate never evaluates legacyPackages; nor does nix flake check.
      legacyPackages.${system}.huge = throw "evaluated legacyPackages";
      # Nor any other system's outputs.
      checks.other-system.broken = throw "evaluated another system";
    }"#;
