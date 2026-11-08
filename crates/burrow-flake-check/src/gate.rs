//! Gate planning, per-output evaluation and whole-flake verification.
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
fn apart(skip: &str, legs: usize, leg: usize) -> Result<bool> {
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
    if string(cmd(
        "nix-instantiate",
        &[
            "--eval",
            "--argstr",
            "f",
            s(&flake.join("flake.nix")),
            "--expr",
            "{ f }: (import f) ? nixConfig",
        ],
    ))? == "true"
    {
        return Err(err(
            "eval-apart can't carry this flake's nixConfig to every evaluation and build. Its gate is a whole `nix flake check`, without eval-apart.",
        ));
    }
    let plan = checked(evaluate(&flake, &system, ""))?;
    fs::write(temp().join("plan.json"), &plan)?;
    if jq(&plan, ".other | length")? != "0" {
        return Err(err(format!(
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
    fs::write(temp().join("entries.json"), &entries)?;
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
    let results = memory::parallel(&mine, side, |i| -> Result<Option<String>> {
        let entry = jq(entries.as_bytes(), &format!(".[{i}]"))?;
        let what = jq(entry.as_bytes(), ".what")?;
        let mut log = format!("evaluating {what}\n");
        let result = (|| -> Result<Option<String>> {
            let o = output(evaluate(&flake, &system, &format!("entry = {i};")))?;
            log.push_str(&String::from_utf8_lossy(&o.stderr));
            if !o.status.success() {
                return Err(err("evaluation failed"));
            }
            if jq(entry.as_bytes(), ".nixos")? == "true" {
                return Ok(None);
            }
            let drv = jq(&o.stdout, ".drv // empty")?;
            if drv.is_empty() || root(&drv, &roots.join(i)).is_err() {
                return Err(err(format!("evaluating {what} gave no derivation")));
            }
            if jq(entry.as_bytes(), ".build")? == "true" {
                return Ok(Some(format!("{drv}^*")));
            }
            if jq(entry.as_bytes(), ".skip")? == "true" {
                log.push_str(&format!("not building {what}\n"));
            }
            Ok(None)
        })();
        if let Err(e) = &result {
            log.push_str(&format!("{e}\n"));
        }
        fs::write(evals.join(format!("{i}.log")), log)?;
        result
    });
    let mut ok = true;
    let mut targets = Vec::new();
    for (i, r) in mine.iter().zip(results) {
        print!("{}", fs::read_to_string(evals.join(format!("{i}.log")))?);
        match r {
            Ok(Some(t)) => targets.push(t),
            Ok(None) => (),
            Err(_) => ok = false,
        }
    }
    Ok(build::checks(None, &targets, &roots)? && ok)
}
fn whole(skip: &str) -> Result<bool> {
    let work = Work::new("roots")?;
    let roots = &work.0;
    let mut failed = false;
    let mut built = false;
    let plan = (|| -> Result<Vec<String>> {
        let metadata = checked(cmd("nix", &["flake", "metadata", "--json", "."]))?;
        let url = jq(&metadata, ".url")?;
        let args = plan_args(&url, &system()?, skip);
        let mut c = cmd("nix-instantiate", &["--eval", "--strict", "--json"]);
        c.args(&args);
        let plan = checked(c)?;
        fs::write(temp().join("plan.json"), &plan)?;
        if jq(&plan, ".nixConfig")? == "true" {
            return Err(err("flake has nixConfig"));
        }
        Ok(args)
    })();
    if let Ok(args) = plan {
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
            if build::checks(Some(&roots.join("out")), &targets, roots)? {
                built = true;
            } else {
                let _ = run(cmd(
                    "nix",
                    &["flake", "check", "--no-build", "-L", "--keep-going"],
                ));
                return Ok(false);
            }
        } else {
            show(&o)?;
            failed = true;
        }
    }
    if skip.is_empty() {
        let ok = if built {
            run(cmd("nix", &["flake", "check", "-L", "--keep-going"]))?
        } else {
            build::slot(&["flake", "check", "-L", "--keep-going"])?
        };
        return Ok(ok && !failed);
    }
    if !run(cmd("nix", &["flake", "check", "--no-build", "-L"]))? {
        return Ok(false);
    }
    if built {
        for c in skip.split_whitespace() {
            println!("not building {c}");
        }
        return Ok(!failed);
    }
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
    let names = string(cmd(
        "nix",
        &[
            "eval",
            "--raw",
            &format!(".#checks.{system}"),
            "--apply",
            "cs: toString (builtins.attrNames cs)",
        ],
    ))?;
    let mut args = vec![
        "build".to_owned(),
        "-L".into(),
        "--no-link".into(),
        "--keep-going".into(),
    ];
    for c in names.split_whitespace() {
        if skip.split_whitespace().any(|s| s == c) {
            println!("not building {c}");
        } else {
            args.push(format!(".#checks.{system}.{c}"));
        }
    }
    let refs: Vec<_> = args.iter().map(String::as_str).collect();
    Ok(build::slot(&refs)? && !failed)
}
pub fn gate() -> Result<bool> {
    let legs = var("LEGS");
    let leg = var("LEG");
    let parse = |v: &str| {
        if v.len() == 1 {
            v.parse::<usize>().ok().filter(|v| (1..=8).contains(v))
        } else {
            None
        }
    };
    let (legs, leg) = match (parse(&legs), parse(&leg)) {
        (Some(ls), Some(l)) if l <= ls => (ls, l),
        _ => {
            return Err(err(format!(
                "can't run leg {leg} of {legs}: there are 1 to 8 legs, numbered from 1"
            )));
        }
    };
    if legs > 1 && var("APART") != "true" {
        return Err(err(
            "legs need eval-apart: without it, the gate is one nix flake check, which can't be split",
        ));
    }
    if var("APART") == "true" {
        apart(&var("SKIP"), legs, leg)
    } else {
        whole(&var("SKIP"))
    }
}
