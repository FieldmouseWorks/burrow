//! The slot a gate's VM tests wait for: on a machine with heavy's slots, a
//! check with a VM test still to run builds only while it holds one, once
//! the rest are built with none; a runner without slots builds it all at
//! once, as it always has.

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use super::{Gate, Ran, Run, lines};
use crate::harness::{executable, read, repo_file, which};

/// The VM tests' checks, as a build names them.
const VM_CHECKS: [&str; 4] = [
    "-needs-kvm.drv^*",
    "-structured.drv^*",
    "-tabbed.drv^*",
    "-around.drv^*",
];

/// A gate on a machine with one heavy slot: heavy itself, as burrow.cpu
/// assembles it, first on its PATH.
struct Slotted {
    g: Gate,
    /// The one slot's lock file.
    slot: PathBuf,
    /// PATH with heavy first.
    path: String,
    /// A flake with a check that needs no VM and four that do: one by its
    /// features, one with structured attrs, one whose features a tab
    /// separates, and one around a VM test.
    vms: PathBuf,
}

impl Slotted {
    fn new() -> Slotted {
        let g = Gate::new();
        let slots = g.s.join("slots");
        fs::create_dir(&slots).unwrap();
        let slot = slots.join("slot1");
        File::create(&slot).unwrap();
        let bin = g.s.join("slotted");
        fs::create_dir(&bin).unwrap();
        executable(
            &bin.join("heavy"),
            &format!(
                "set -euo pipefail\nSLOTS=1\nGRACE=3\nDIR={}\n{}",
                slots.display(),
                read(&repo_file("modules/cpu/heavy.sh"))
            ),
        );
        let path = format!("{}:{}", bin.display(), g.s.path_var());
        let vms = g.flake(
            r#"{
      checks.${system} = {
        a = drv "a";
        needs-kvm = derivation { name = "needs-kvm"; inherit system; builder = "/bin/sh"; args = [ "-c" "echo > $out" ]; requiredSystemFeatures = [ "kvm" "nixos-test" ]; };
        structured = derivation { name = "structured"; inherit system; builder = "/bin/sh"; args = [ "-c" "echo > $out" ]; __structuredAttrs = true; requiredSystemFeatures = [ "kvm" ]; };
        tabbed = derivation { name = "tabbed"; inherit system; builder = "/bin/sh"; args = [ "-c" "echo > $out" ]; requiredSystemFeatures = "big-parallel\tkvm"; };
        # A check around a VM test builds it too.
        around = derivation { name = "around"; inherit system; builder = "/bin/sh"; args = [ "-c" "echo > $out" ];
          test = derivation { name = "inner"; inherit system; builder = "/bin/sh"; args = [ "-c" "echo > $out" ]; requiredSystemFeatures = [ "nixos-test" ]; }; };
      };
    }"#,
        );
        Slotted { g, slot, path, vms }
    }

    /// The gate on `dir` with heavy's slots: heavy first on PATH, unless
    /// the run's own env says otherwise.
    fn run(&self, dir: &Path, r: Run) -> Ran {
        let mut env = vec![("PATH", self.path.as_str())];
        env.extend_from_slice(r.env);
        self.g.run(dir, Run { env: &env, ..r })
    }

    /// The builds nix ran in a slot.
    fn in_slot(&self) -> Vec<String> {
        lines(&self.g.s.join("nix.log.slot"))
    }

    /// The check with no VM built first and with no slot, the VM tests after
    /// it, in one build, in the slot, with as many jobs as the slot's share.
    fn split(&self, r: &Ran) {
        let [plain, vm] = r.builds()[..] else {
            panic!("not two builds: {:?}\n{}", r.builds(), r.out)
        };
        assert!(
            plain.ends_with("-a.drv^*") && !VM_CHECKS.iter().any(|c| plain.contains(c)),
            "{plain}"
        );
        assert!(
            VM_CHECKS.iter().all(|c| vm.contains(c)) && !vm.contains("-a.drv"),
            "{vm}"
        );
        assert_eq!(self.in_slot(), [vm]);
        let jobs = vm.split("--max-jobs ").nth(1).unwrap_or_default();
        assert!(
            !jobs.is_empty() && jobs.bytes().all(|b| b.is_ascii_digit()),
            "{vm}"
        );
        assert!(
            r.out.contains("waiting for a heavy slot for 4 checks"),
            "{}",
            r.out
        );
    }
}

/// While every slot is held, the gate builds what needs no VM and its VM
/// tests wait, then build holding one.
#[test]
fn slots_the_vm_tests_wait_for_one() {
    let t = Slotted::new();
    let (script, env) = t.g.gate_step(&Run {
        env: &[("PATH", t.path.as_str())],
        ..Run::default()
    });
    let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let said = t.g.s.join("gate.out");
    let builds = || {
        t.g.calls()
            .into_iter()
            .filter(|c| c.starts_with("build "))
            .collect::<Vec<_>>()
    };
    let held = File::open(&t.slot).unwrap();
    held.lock().unwrap();
    let mut gate = t.g.s.start(&script, &t.vms, &env, &said);
    let deadline = Instant::now() + Duration::from_secs(300);
    while builds().is_empty() && gate.try_wait().unwrap().is_none() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(200));
    }
    let running = gate.try_wait().unwrap().is_none();
    if running {
        // Long enough for a gate with no wait in it to have gone on to its
        // VM tests.
        thread::sleep(Duration::from_secs(5));
    }
    let so_far = builds();
    if !running || so_far.len() != 1 || !so_far[0].ends_with("-a.drv^*") {
        let _ = gate.kill();
        let _ = gate.wait();
        panic!("{so_far:?}\n{}", read(&said));
    }
    // The slot's free now.
    drop(held);
    let code = gate.wait().unwrap().code().unwrap_or(-1);
    let r = t.g.ran(code, read(&said));
    assert_eq!(r.code, 0, "{}", r.out);
    assert!(r.out.contains("all 1 slots busy, waiting"), "{}", r.out);
    t.split(&r);
}

/// Without eval-apart too; and a failed half fails the gate with the other
/// still built.
#[test]
fn slots_split_without_eval_apart_and_on_failure() {
    let t = Slotted::new();
    let r = t.run(
        &t.vms,
        Run {
            whole: true,
            ..Run::default()
        },
    );
    assert_eq!(r.code, 0, "{}", r.out);
    t.split(&r);
    // Both halves' outputs stay roots for nix flake check, which then takes
    // no slot.
    assert_eq!(
        r.calls.last().map(String::as_str),
        Some("flake check -L --keep-going"),
        "{:?}",
        r.calls
    );
    let links: Vec<&str> = r
        .builds()
        .iter()
        .map(|b| b.split_whitespace().nth(3).unwrap_or_default())
        .collect();
    assert_eq!(links[1], format!("{}-vm", links[0]), "{links:?}");
    for (fail, whole) in [
        ("-a.drv", false),
        ("-needs-kvm.drv", false),
        ("-a.drv", true),
        ("-needs-kvm.drv", true),
    ] {
        let r = t.run(
            &t.vms,
            Run {
                whole,
                build_fail: fail,
                ..Run::default()
            },
        );
        assert!(
            r.code != 0 && r.builds().len() == 2,
            "{fail} {whole}: {}\n{:?}",
            r.out,
            r.calls
        );
    }
}

/// A VM test already built, a rerun's, takes no slot, its outputs kept as
/// roots first.
#[test]
fn slots_built_vm_tests_wait_for_nothing() {
    let t = Slotted::new();
    for whole in [false, true] {
        let r = t.run(
            &t.vms,
            Run {
                whole,
                env: &[("ALL_BUILT", "1")],
                ..Run::default()
            },
        );
        assert_eq!(r.code, 0, "{}", r.out);
        let b = r.build();
        assert!(
            VM_CHECKS.iter().chain(&["-a.drv^*"]).all(|c| b.contains(c)),
            "{b}"
        );
        assert!(t.in_slot().is_empty(), "{:?}", t.in_slot());
        let roots = lines(&t.g.s.join("nix.log.roots"));
        // One each: the four VM tests, the one inside the wrapper among
        // them.
        assert!(
            roots.len() == 4
                && roots.iter().all(|r| {
                    r.split_whitespace().nth(2).is_some_and(|o| {
                        ["-needs-kvm", "-structured", "-tabbed", "-inner"]
                            .iter()
                            .any(|n| o.ends_with(n))
                    })
                }),
            "{roots:?}"
        );
        let links: std::collections::HashSet<_> = roots
            .iter()
            .filter_map(|r| r.split_whitespace().nth(1))
            .collect();
        assert_eq!(links.len(), 4, "{roots:?}");
    }
}

/// An older nix's JSON tells the same VM tests apart, structured attrs and
/// all.
#[test]
fn slots_an_older_nixs_json() {
    let t = Slotted::new();
    let r = t.run(
        &t.vms,
        Run {
            env: &[("OLD_JSON", "1")],
            ..Run::default()
        },
    );
    assert_eq!(r.code, 0, "{}", r.out);
    t.split(&r);
}

/// A check whose closure can't be read builds in the slot.
#[test]
fn slots_a_closure_it_cant_read() {
    let t = Slotted::new();
    let r = t.run(
        &t.vms,
        Run {
            env: &[("NO_CLOSURE", "1")],
            ..Run::default()
        },
    );
    assert_eq!(r.code, 0, "{}", r.out);
    let b = r.build();
    assert!(
        t.in_slot() == [b] && b.contains("-a.drv^*"),
        "{:?} {b}",
        t.in_slot()
    );
}

/// With no telling what's a VM test, every check builds in the slot.
#[test]
fn slots_unsure_takes_the_slot() {
    let t = Slotted::new();
    let broken = t.g.s.join("nojq");
    fs::create_dir(&broken).unwrap();
    executable(
        &broken.join("jq"),
        &format!(
            "if [[ \"$*\" == *requiredSystemFeatures* ]]; then exit 1; fi\nexec {} \"$@\"\n",
            which("jq").display()
        ),
    );
    let path = format!("{}:{}", broken.display(), t.path);
    let r = t.run(
        &t.vms,
        Run {
            env: &[("PATH", &path)],
            ..Run::default()
        },
    );
    assert_eq!(r.code, 0, "{}", r.out);
    assert_eq!(t.in_slot(), [r.build()]);
}

/// What nix builds by name, a nixConfig flake's, builds in a slot; its
/// check that only evaluates takes none, and the build does.
#[test]
fn slots_what_nix_builds_by_name() {
    let t = Slotted::new();
    let d = t.g.flake_with(
        r#"{ checks.${system} = { a = drv "a"; vm = drv "vm"; }; }"#,
        r#"nixConfig.bash-prompt = "x";"#,
    );
    let r = t.run(
        &d,
        Run {
            whole: true,
            ..Run::default()
        },
    );
    assert!(
        r.code == 0
            && t.in_slot().len() == 1
            && t.in_slot()[0].starts_with("flake check -L --keep-going"),
        "{}\n{:?}",
        r.out,
        t.in_slot()
    );
    let r = t.run(
        &d,
        Run {
            whole: true,
            skip: "vm",
            ..Run::default()
        },
    );
    assert_eq!(r.code, 0, "{}", r.out);
    let [line] = &t.in_slot()[..] else {
        panic!("{:?}", t.in_slot())
    };
    assert!(
        line.starts_with("build -L --no-link --keep-going"),
        "{line}"
    );
}

/// A runner without slots, a hosted one, builds it all at once, as it
/// always has.
#[test]
fn slots_none_on_a_hosted_runner() {
    let t = Slotted::new();
    let r = t.g.apart(&t.vms);
    assert_eq!(r.code, 0, "{}", r.out);
    let b = r.build();
    assert!(
        VM_CHECKS.iter().chain(&["-a.drv^*"]).all(|c| b.contains(c)) && !b.contains("--max-jobs"),
        "{b}"
    );
    assert!(
        t.in_slot().is_empty() && !r.out.contains("heavy slot"),
        "{}",
        r.out
    );
}
