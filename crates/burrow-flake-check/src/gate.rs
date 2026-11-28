//! Gate planning, per-output evaluation and whole-flake verification.
use super::flaky::{Check, Flaky};
use super::plan::PLAN;
use super::*;
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
    let plan = judged(evaluate(&flake, &system, ""))?;
    save(&temp().join("plan.json"), &plan)?;
    if jq(&plan, ".other | length")? != "0" {
        return Err(failed(format!(
            "eval-apart doesn't check apps, overlays, hydraJobs, bundlers or the old default* outputs, and this flake has {}. Its gate is a whole `nix flake check`, without eval-apart.",
            jq(&plan, ".other")?
        )));
    }
    for unknown in jq(&plan, ".unknown[]")?.lines() {
        println!("warning: unknown flake output '{unknown}'");
    }
    let entries = jq(
        &plan,
        &format!(
            r#"def hash: reduce explode[] as $c (0; (. * 31 + $c) % 1000003);
        ({} | split(" ") | map(select(. != ""))) as $s
        | .entries | map(.check and (.name as $n | $s | index([$n]) | not)) as $build
        | [range(length) as $i | .[$i] + {{build: $build[$i], skip: (.[$i].check and ($build[$i] | not)), mine: (.[$i].what | hash % {legs} == {leg} - 1)}}]"#,
            quote(skip)
        ),
    )?;
    save(&temp().join("entries.json"), &entries)?;
    let mine = lines(&jq(
        entries.as_bytes(),
        "to_entries[] | select(.value.mine) | .key",
    )?);
    if legs > 1 {
        println!(
            "leg {leg} of {legs}: {} of the plan's {} outputs, those whose names hash to it",
            mine.len(),
            jq(entries.as_bytes(), "length")?
        );
    }
    let roots = work.join("roots");
    fs::create_dir(&roots)?;
    let evals = work.join("evals");
    fs::create_dir(&evals)?;
    let side = memory::side();
    println!("{} outputs to evaluate, {side} side by side", mine.len());
    let results = memory::parallel(&mine, side, |i, slot| -> Result<Option<Check>> {
        let entry = jq(entries.as_bytes(), &format!(".[{i}]"))?;
        let what = jq(entry.as_bytes(), ".what")?;
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
            if jq(entry.as_bytes(), ".nixos")? == "true" {
                return Ok(None);
            }
            let drv = jq(&o.stdout, ".drv // empty")?;
            let none = || failed(format!("evaluating {what} gave no derivation"));
            if drv.is_empty() {
                return Err(none());
            }
            if let Err(e) = root(&drv, &roots.join(i)) {
                // The flake's, if it named one the store hasn't got; rooting
                // one it has is the gate's own work.
                let has = output(cmd("nix-store", &["--check-validity", &drv]))?;
                return Err(if has.status.success() { e } else { none() });
            }
            if jq(entry.as_bytes(), ".build")? == "true" {
                return Ok(Some(Check {
                    name: Some(jq(entry.as_bytes(), ".name")?),
                    target: format!("{drv}^*"),
                }));
            }
            if jq(entry.as_bytes(), ".skip")? == "true" {
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
    let plan = (|| -> Result<(Vec<String>, Vec<u8>)> {
        let metadata = judged(cmd("nix", &locked(&["flake", "metadata", "--json", "."])))?;
        let url = jq(&metadata, ".url")?;
        let args = plan_args(&url, &system()?, skip);
        let mut c = cmd("nix-instantiate", &["--eval", "--strict", "--json"]);
        c.args(&args);
        let plan = judged(c)?;
        save(&temp().join("plan.json"), &plan)?;
        if jq(&plan, ".nixConfig")? == "true" {
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
            let names = jq(
                &plan,
                &format!(
                    r#"({} | split(" ") | map(select(. != ""))) as $s
                | .entries[] | select(.check and (.name as $n | $s | index([$n]) | not)) | .name"#,
                    quote(skip)
                ),
            )
            .map(|n| lines(&n))
            .unwrap_or_default();
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
