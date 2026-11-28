//! Gate planning, per-output evaluation and whole-flake verification.
use super::flaky::{Check, Flaky};
use super::plan::PLAN;
use super::*;
use serde::{Deserialize, Serialize};
#[derive(Deserialize)]
struct Plan {
    #[serde(default)]
    other: Vec<String>,
    #[serde(default)]
    unknown: Vec<String>,
    #[serde(default)]
    entries: Vec<PlanEntry>,
    #[serde(default, rename = "nixConfig")]
    nix_config: bool,
}
#[derive(Deserialize)]
struct PlanEntry {
    what: String,
    #[serde(default)]
    check: bool,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    nixos: bool,
}
#[derive(Debug, PartialEq, Serialize)]
struct AnnotatedEntry {
    what: String,
    check: bool,
    name: Option<String>,
    nixos: bool,
    build: bool,
    skip: bool,
    mine: bool,
}
#[derive(Deserialize)]
struct EvalOut {
    #[serde(default)]
    drv: Option<String>,
}
#[derive(Deserialize)]
struct FlakeMetadata {
    url: String,
}
fn hash_what(what: &str) -> usize {
    what.chars()
        .fold(0u64, |acc, c| (acc * 31 + u64::from(c as u32)) % 1_000_003) as usize
}
fn annotate_entries(
    entries: &[PlanEntry],
    skip: &str,
    legs: usize,
    leg: usize,
) -> Vec<AnnotatedEntry> {
    let skipped: Vec<&str> = skip.split(' ').filter(|s| !s.is_empty()).collect();
    entries
        .iter()
        .map(|e| {
            let build = e.check && e.name.as_deref().is_none_or(|n| !skipped.contains(&n));
            AnnotatedEntry {
                what: e.what.clone(),
                check: e.check,
                name: e.name.clone(),
                nixos: e.nixos,
                build,
                skip: e.check && !build,
                mine: hash_what(&e.what) % legs == leg - 1,
            }
        })
        .collect()
}
fn plan_args(url: &str, system: &str, skip: &str) -> Vec<String> {
    vec![
        "--option".into(),
        "pure-eval".into(),
        "true".into(),
        "--argstr".into(),
        "url".into(),
        url.into(),
        "--argstr".into(),
        "system".into(),
        system.into(),
        "--argstr".into(),
        "skip".into(),
        skip.into(),
        "--expr".into(),
        format!(
            r#"{{ url, system, skip, builds ? false }}:
      let flake = builtins.getFlake url;
        planned = ({PLAN}) {{ inherit system skip builds; }} flake.outputs;
      in if builds then planned else planned // {{ nixConfig = import "${{flake.outPath}}/flake.nix" ? nixConfig; }}"#
        ),
    ]
}
fn evaluate(flake: &Path, system: &str, entry: &str) -> Command {
    cmd(
        "nix",
        &[
            "eval",
            "--no-update-lock-file",
            "--option",
            "eval-cache",
            "false",
            "--apply",
            &format!("({PLAN}) {{ system = \"{system}\"; {entry} }}"),
            "--json",
            &format!("{}#.", s(flake)),
        ],
    )
}
fn apart(skip: &str, legs: usize, leg: usize, flaky: Option<&Flaky>) -> Result<bool> {
    let system = system()?;
    let work = Work::new("eval-apart")?;
    let work = &work.0;
    let source = work.join("source");
    let top = output(cmd("git", &["rev-parse", "--show-toplevel"]))?;
    let flake = if top.status.success() {
        let top = text_output(top.stdout)?;
        snapshot::snapshot(Path::new(&top), &source)?;
        let prefix = string(cmd("git", &["rev-parse", "--show-prefix"]))?;
        source.join(prefix)
    } else {
        checked(cmd("cp", &["-a", ".", s(&source)]))?;
        source
    };
    if text_output(judged(cmd(
        "nix-instantiate",
        &[
            "--eval",
            "--argstr",
            "f",
            s(&flake.join("flake.nix")),
            "--expr",
            "{ f }: (import f) ? nixConfig",
        ],
    ))?)?
        == "true"
    {
        return Err(failed(
            "eval-apart can't carry this flake's nixConfig to every evaluation and build. Its gate is a whole `nix flake check`, without eval-apart.",
        ));
    }
    let plan_bytes = judged(evaluate(&flake, &system, ""))?;
    save(&temp().join("plan.json"), &plan_bytes)?;
    let plan: Plan = serde_json::from_slice(&plan_bytes)?;
    if !plan.other.is_empty() {
        return Err(failed(format!(
            "eval-apart doesn't check apps, overlays, hydraJobs, bundlers or the old default* outputs, and this flake has {}. Its gate is a whole `nix flake check`, without eval-apart.",
            serde_json::to_string(&plan.other)?
        )));
    }
    for unknown in &plan.unknown {
        println!("warning: unknown flake output '{unknown}'");
    }
    let entries = annotate_entries(&plan.entries, skip, legs, leg);
    save(&temp().join("entries.json"), serde_json::to_vec(&entries)?)?;
    let mine: Vec<usize> = entries
        .iter()
        .enumerate()
        .filter_map(|(i, e)| e.mine.then_some(i))
        .collect();
    if legs > 1 {
        println!(
            "leg {leg} of {legs}: {} of the plan's {} outputs, those whose names hash to it",
            mine.len(),
            entries.len()
        );
    }
    let roots = work.join("roots");
    fs::create_dir(&roots)?;
    let evals = work.join("evals");
    fs::create_dir(&evals)?;
    let side = memory::side();
    println!("{} outputs to evaluate, {side} side by side", mine.len());
    let results = memory::parallel(&mine, side, |&i, slot| -> Result<Option<Check>> {
        let entry = &entries[i];
        let what = &entry.what;
        let mut log = format!("evaluating {what}\n");
        let result = (|| -> Result<Option<Check>> {
            let o = slot.output(
                evaluate(&flake, &system, &format!("entry = {i};")),
                &format!("evaluating {what}"),
            )?;
            log.push_str(&String::from_utf8_lossy(&o.stderr));
            if !verdict::passed("nix eval", o.status)? {
                return Err(failed("evaluation failed"));
            }
            if entry.nixos {
                return Ok(None);
            }
            let drv = serde_json::from_slice::<EvalOut>(&o.stdout)?
                .drv
                .unwrap_or_default();
            let none = || failed(format!("evaluating {what} gave no derivation"));
            if drv.is_empty() {
                return Err(none());
            }
            if let Err(e) = root(&drv, &roots.join(i.to_string())) {
                // The flake's, if it named one the store hasn't got; rooting
                // one it has is the gate's own work.
                let has = output(cmd("nix-store", &["--check-validity", &drv]))?;
                return Err(if has.status.success() { e } else { none() });
            }
            if entry.build {
                return Ok(Some(Check {
                    name: entry.name.clone(),
                    target: format!("{drv}^*"),
                }));
            }
            if entry.skip {
                log.push_str(&format!("not building {what}\n"));
            }
            Ok(None)
        })();
        if let Err(e) = &result {
            log.push_str(&format!("{e}\n"));
        }
        save(&evals.join(format!("{i}.log")), log)?;
        result
    });
    let mut ok = true;
    let mut targets = Vec::new();
    for (i, r) in mine.iter().zip(results) {
        // One whose log couldn't be written says why in its own error.
        if let Ok(log) = fs::read_to_string(evals.join(format!("{i}.log"))) {
            print!("{log}");
        }
        match r {
            Ok(Some(t)) => targets.push(t),
            Ok(None) => (),
            Err(e) => {
                ok = false;
                if !e.is::<verdict::Failed>() {
                    verdict::broke(&*e);
                }
            }
        }
    }
    Ok(build::gated(flaky, None, &targets, &roots)? && ok)
}
/// `args` for a nix command on the flake, which with gate-reuse may not
/// change its lock file: the pass it attests is the tree's, so the inputs
/// it evaluates are the ones the tree locks (eval-apart's snapshot refuses
/// a lock that needs changing already).
pub(crate) fn locked<'a>(args: &[&'a str]) -> Vec<&'a str> {
    let mut v = args.to_vec();
    if var("TREE_SOURCE") == "true" {
        v.insert(args.len().min(2), "--no-update-lock-file");
    }
    v
}
fn whole(skip: &str, flaky: Option<&Flaky>) -> Result<bool> {
    let work = Work::new("roots")?;
    let roots = &work.0;
    let mut unbuildable = false;
    let mut built = false;
    let plan = (|| -> Result<(Vec<String>, Plan)> {
        let metadata = judged(cmd("nix", &locked(&["flake", "metadata", "--json", "."])))?;
        let url = serde_json::from_slice::<FlakeMetadata>(&metadata)?.url;
        let args = plan_args(&url, &system()?, skip);
        let mut c = cmd("nix-instantiate", &["--eval", "--strict", "--json"]);
        c.args(&args);
        let plan_bytes = judged(c)?;
        save(&temp().join("plan.json"), &plan_bytes)?;
        let plan: Plan = serde_json::from_slice(&plan_bytes)?;
        if plan.nix_config {
            return Err(failed("flake has nixConfig"));
        }
        Ok((args, plan))
    })();
    let plan = match plan {
        Err(e) if !e.is::<verdict::Failed>() => {
            verdict::broke(&*e);
            Err(e)
        }
        plan => plan,
    };
    if let Ok((args, plan)) = plan {
        let mut c = cmd(
            "nix-instantiate",
            &[
                "--add-root",
                s(&roots.join("check")),
                "--arg",
                "builds",
                "true",
            ],
        );
        c.args(args);
        let o = output(c)?;
        if o.status.success() {
            let mut targets = Vec::new();
            for r in String::from_utf8(o.stdout)?.lines() {
                let r = r.split('!').next().unwrap();
                targets.push(format!("{}^*", s(&fs::read_link(r)?)));
            }
            // The checks' names, in the order the plan builds them (its
            // `builds`), for their second tries: none if they don't pair.
            let skipped: Vec<&str> = skip.split(' ').filter(|s| !s.is_empty()).collect();
            let names: Vec<String> = plan
                .entries
                .iter()
                .filter(|e| e.check && e.name.as_deref().is_some_and(|n| !skipped.contains(&n)))
                .filter_map(|e| e.name.clone())
                .collect();
            let paired = names.len() == targets.len();
            let checks: Vec<Check> = targets
                .into_iter()
                .enumerate()
                .map(|(i, target)| Check {
                    name: paired.then(|| names[i].clone()),
                    target,
                })
                .collect();
            if build::gated(flaky, Some(&roots.join("out")), &checks, roots)? {
                built = true;
            } else {
                let _ = run(cmd(
                    "nix",
                    &locked(&["flake", "check", "--no-build", "-L", "--keep-going"]),
                ));
                return Ok(false);
            }
        } else {
            show(&o)?;
            if let Err(e) = verdict::passed("nix-instantiate", o.status) {
                verdict::broke(&*e);
            }
            unbuildable = true;
        }
    }
    if skip.is_empty() {
        let ok = if built {
            run(cmd(
                "nix",
                &locked(&["flake", "check", "-L", "--keep-going"]),
            ))?
        } else {
            build::slot(&locked(&["flake", "check", "-L", "--keep-going"]))?
        };
        return Ok(ok && !unbuildable);
    }
    if !run(cmd("nix", &locked(&["flake", "check", "--no-build", "-L"])))? {
        return Ok(false);
    }
    if built {
        for c in skip.split_whitespace() {
            println!("not building {c}");
        }
        return Ok(!unbuildable);
    }
    let checks = attributes(skip)?;
    let mut args = vec!["build", "-L", "--no-link", "--keep-going"];
    args.extend(checks.iter().map(String::as_str));
    Ok(build::slot(&locked(&args))? && !unbuildable)
}
/// The flake's checks for this system, as attributes, but those `skip`
/// names, each said as it's left out.
fn attributes(skip: &str) -> Result<Vec<String>> {
    let system = string(cmd(
        "nix",
        &[
            "eval",
            "--impure",
            "--raw",
            "--expr",
            "builtins.currentSystem",
        ],
    ))?;
    let names = text_output(judged(cmd(
        "nix",
        &locked(&[
            "eval",
            "--raw",
            &format!(".#checks.{system}"),
            "--apply",
            "cs: toString (builtins.attrNames cs)",
        ]),
    ))?)?;
    let mut checks = Vec::new();
    for c in names.split_whitespace() {
        if skip.split_whitespace().any(|s| s == c) {
            println!("not building {c}");
        } else {
            checks.push(format!(".#checks.{system}.{c}"));
        }
    }
    Ok(checks)
}
pub fn gate() -> Result<bool> {
    let legs = var("LEGS");
    let leg = var("LEG");
    let parse = |v: &str| {
        if (1..=2).contains(&v.len())
            && v.bytes().all(|b| b.is_ascii_digit())
            && !v.starts_with('0')
        {
            v.parse::<usize>().ok().filter(|v| (1..=12).contains(v))
        } else {
            None
        }
    };
    let (legs, leg) = match (parse(&legs), parse(&leg)) {
        (Some(ls), Some(l)) if l <= ls => (ls, l),
        _ => {
            return Err(failed(format!(
                "can't run leg {leg} of {legs}: there are 1 to 12 legs, numbered from 1"
            )));
        }
    };
    if legs > 1 && var("APART") != "true" {
        return Err(failed(
            "legs need eval-apart: without it, the gate is one nix flake check, which can't be split",
        ));
    }
    // gate-reuse: Nix reads the commit of the tree alone (source.rs).
    let _source = if var("TREE_SOURCE") == "true" {
        Some(super::source::TreeSource::enter()?)
    } else {
        None
    };
    // A second try for a flaky check (docs/flaky-tests.md, flaky.rs).
    let flaky = Flaky::from_env();
    if var("APART") == "true" {
        apart(&var("SKIP"), legs, leg, flaky.as_ref())
    } else {
        whole(&var("SKIP"), flaky.as_ref())
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_deserializes_and_assigns_legs_and_skips() {
        let raw = br#"{
            "other": [],
            "unknown": ["extra"],
            "nixConfig": true,
            "entries": [
                {"what": "checks.x86_64-linux.a", "check": true, "name": "a", "nixos": false},
                {"what": "checks.x86_64-linux.agents", "check": true, "name": "agents", "nixos": false},
                {"what": "packages.x86_64-linux.p", "check": false, "name": "p", "nixos": false},
                {"what": "nixosConfigurations.box", "check": false, "name": null, "nixos": true}
            ]
        }"#;
        let plan: Plan = serde_json::from_slice(raw).unwrap();
        assert!(plan.nix_config);
        assert_eq!(plan.unknown, ["extra"]);
        let annotated = annotate_entries(&plan.entries, " agents ", 3, 1);
        assert!(annotated[0].build && !annotated[0].skip);
        assert!(!annotated[1].build && annotated[1].skip);
        assert!(!annotated[2].build && !annotated[2].skip);
        assert!(annotated[3].nixos && annotated[3].name.is_none());
        // Every entry belongs to exactly one leg across 1..=3.
        for idx in 0..plan.entries.len() {
            let owners: Vec<usize> = (1..=3)
                .filter(|&leg| annotate_entries(&plan.entries, "", 3, leg)[idx].mine)
                .collect();
            assert_eq!(owners.len(), 1, "entry {idx}: {owners:?}");
        }
        // Polynomial rolling hash over Unicode codepoints mod 1_000_003.
        assert_eq!(hash_what("checks.x86_64-linux.a"), 883_929);
    }
}
