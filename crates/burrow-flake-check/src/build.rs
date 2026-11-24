//! Shared build policy: root cached VM outputs, then build only pending VMs
//! under heavy. Unknown closures conservatively take a slot.
use super::*;
fn has_slots() -> bool {
    env::split_paths(&env::var_os("PATH").unwrap_or_default()).any(|p| p.join("heavy").is_file())
}
fn in_slot(args: &[String]) -> Result<bool> {
    if has_slots() {
        // Heavy owns the Nix defaults; a CLI max-jobs would override them.
        let mut c = cmd("heavy", &["--", "nix"]);
        c.args(args);
        run(c)
    } else {
        let mut c = cmd("nix", &[]);
        c.args(args);
        run(c)
    }
}
pub fn slot(args: &[&str]) -> Result<bool> {
    in_slot(&args.iter().map(|v| (*v).to_owned()).collect::<Vec<_>>())
}
fn unbuilt_vms(targets: &[String], roots: &Path) -> Result<Vec<String>> {
    let mut c = cmd("nix", &["derivation", "show", "-r"]);
    c.args(
        targets
            .iter()
            .map(|t| format!("{}^*", t.trim_end_matches("^*"))),
    );
    let json = checked(c)?;
    let names = jq(
        &json,
        r#"(.derivations // .) | to_entries[]
      | (.value.env | if type != "object" then error("a derivation with no env") else . end) as $env
      | select(($env.requiredSystemFeatures // "" | [splits("[ \t\n\r]+")])
        + (.value.structuredAttrs.requiredSystemFeatures // [])
        + ($env.__json // "{}" | fromjson | .requiredSystemFeatures // [])
        | any(. == "kvm" or . == "nixos-test")) | .key | sub(".*/"; "")"#,
    )?;
    let store = Path::new(&targets[0])
        .parent()
        .ok_or_else(|| err("no derivation directory"))?;
    let mut pending = Vec::new();
    let mut i = 0;
    for name in names.lines() {
        let outs = output(cmd("nix-store", &["-q", "--outputs", s(&store.join(name))]))?;
        let paths = String::from_utf8(outs.stdout)?;
        let mut missing = !outs.status.success() || paths.trim().is_empty();
        for out in paths.split_whitespace() {
            i += 1;
            if !output(cmd(
                "nix-store",
                &[
                    "--add-root",
                    s(&roots.join(format!("vm-output-{i}"))),
                    "--indirect",
                    "-r",
                    out,
                ],
            ))?
            .status
            .success()
            {
                missing = true;
            }
        }
        if missing {
            pending.push(name.to_owned());
        }
    }
    Ok(pending)
}
/// The derivations a `nix build --dry-run` says it will build, not fetch.
pub fn cold_drvs(dry_run_stderr: &str) -> Vec<String> {
    let mut drvs = Vec::new();
    let mut building = false;
    for line in dry_run_stderr.lines() {
        let l = line.trim();
        if l.contains("will be built:") {
            building = true;
        } else if l.contains("will be fetched") || l.contains("don't know how to build") {
            building = false;
        } else if building && l.starts_with("/nix/store/") {
            drvs.push(l.to_owned());
        }
    }
    drvs.sort();
    drvs.dedup();
    drvs
}
/// Their names, each once.
pub fn cold_names(dry_run_stderr: &str) -> Vec<String> {
    let mut names: Vec<String> = cold_drvs(dry_run_stderr)
        .iter()
        .map(|d| super::builds::name(d))
        .collect();
    names.sort();
    names.dedup();
    names
}
/// Say by name what this leg will compile because no cache has it, so a long
/// quiet build isn't mistaken for a hang: those derivations. Informational:
/// it never fails a leg.
fn announce_cold(targets: &[String]) -> Vec<String> {
    let mut c = cmd("nix-store", &["--realise", "--dry-run"]);
    c.args(targets.iter().map(|t| t.trim_end_matches("^*")));
    let Ok(o) = output(c) else { return Vec::new() };
    let said = String::from_utf8_lossy(&o.stderr);
    let (drvs, names) = (cold_drvs(&said), cold_names(&said));
    if names.is_empty() {
        return drvs;
    }
    let mut msg = format!(
        "cold build: {} derivations no cache has will be built here, not restored: {}",
        names.len(),
        names
            .iter()
            .take(40)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ")
    );
    if names.len() > 40 {
        msg.push_str(&format!(", and {} more", names.len() - 40));
    }
    println!("{msg}");
    if let Some(path) = env::var_os("GITHUB_STEP_SUMMARY")
        && let Ok(mut f) = fs::OpenOptions::new().append(true).create(true).open(path)
    {
        let _ = writeln!(f, "{msg}");
    }
    drvs
}
pub fn plan(targets: &[String]) -> Result<()> {
    let mut cold: Vec<String> = if targets.is_empty() {
        Vec::new()
    } else {
        announce_cold(targets)
            .iter()
            .map(|d| super::builds::name(d))
            .collect()
    };
    cold.sort();
    cold.dedup();
    fs::write(temp().join("cold.txt"), cold.join("\n"))?;
    Ok(())
}
/// What the store has of the checks the whole-flake fallback builds by
/// attribute (gate::whole), as [`checks`] says it of derivations: their
/// derivations, from `nix path-info --derivation`. Only in a run that names
/// a store, and never failing.
pub fn shadow_attributes(attributes: &[String]) {
    if !super::builds::writes() || attributes.is_empty() {
        return;
    }
    let mut args = vec!["path-info", "--derivation"];
    args.extend(attributes.iter().map(String::as_str));
    let mut c = cmd("nix", &super::gate::locked(&args));
    c.stdin(Stdio::null());
    let drvs = match output(c) {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .split_whitespace()
            .map(str::to_owned)
            .collect::<Vec<_>>(),
        Ok(o) => {
            println!(
                "build store (shadow, nothing substituted): couldn't tell which derivations the checks are: {}",
                String::from_utf8_lossy(&o.stderr).trim()
            );
            return;
        }
        Err(e) => {
            println!(
                "build store (shadow, nothing substituted): couldn't tell which derivations the checks are: {e}"
            );
            return;
        }
    };
    super::builds::shadow(&announce_cold(&drvs));
}
pub fn checks(link: Option<&Path>, targets: &[String], roots: &Path) -> Result<bool> {
    if targets.is_empty() {
        return Ok(true);
    }
    // What the build store would have served of them, when the run names
    // one: nothing is taken from it (builds.rs).
    super::builds::shadow(&announce_cold(targets));
    built(link, targets, roots)
}
/// The gate's checks, with a second try for a flaky one (flaky.rs): the
/// first build as [`checks`], and each second try in the slot its first
/// took, linked apart from it.
pub fn gated(
    flaky: Option<&super::flaky::Flaky>,
    link: Option<&Path>,
    checks: &[super::flaky::Check],
    roots: &Path,
) -> Result<bool> {
    if checks.is_empty() {
        return Ok(true);
    }
    struct Real<'a> {
        link: Option<&'a Path>,
        roots: &'a Path,
        tries: usize,
    }
    impl super::flaky::Nix for Real<'_> {
        fn build(&mut self, targets: &[String]) -> Result<bool> {
            self.tries += 1;
            if self.tries == 1 {
                return self::checks(self.link, targets, self.roots);
            }
            let link = self
                .link
                .map(|p| PathBuf::from(format!("{}-again-{}", s(p), self.tries)));
            built(link.as_deref(), targets, self.roots)
        }
        fn pending(&mut self, target: &str) -> Result<Vec<String>> {
            let o = output(cmd(
                "nix-store",
                &["--realise", "--dry-run", target.trim_end_matches("^*")],
            ))?;
            if !o.status.success() {
                return Err(err(String::from_utf8_lossy(&o.stderr).trim().to_owned()));
            }
            Ok(cold_drvs(&String::from_utf8_lossy(&o.stderr)))
        }
        fn log(&mut self, target: &str) -> Option<String> {
            let mut c = cmd("nix", &["log", target.trim_end_matches("^*")]);
            c.stdin(Stdio::null());
            let o = output(c).ok().filter(|o| o.status.success())?;
            Some(String::from_utf8_lossy(&o.stdout).into_owned())
        }
    }
    super::flaky::build(
        flaky,
        checks,
        &mut Real {
            link,
            roots,
            tries: 0,
        },
    )
}
/// Build them, those with VM tests in a heavy slot where there are slots.
fn built(link: Option<&Path>, targets: &[String], roots: &Path) -> Result<bool> {
    let args = |ts: &[String], vm: bool| {
        let mut a = vec!["build".to_owned(), "-L".into()];
        if let Some(p) = link {
            a.extend([
                "--out-link".into(),
                if vm {
                    format!("{}-vm", s(p))
                } else {
                    s(p).to_owned()
                },
            ]);
        } else {
            a.push("--no-link".into());
        }
        a.push("--keep-going".into());
        a.extend_from_slice(ts);
        a
    };
    if !has_slots() {
        let mut c = cmd("nix", &[]);
        c.args(args(targets, false));
        return run(c);
    }
    let mut plain = Vec::new();
    let mut vms = Vec::new();
    match unbuilt_vms(targets, roots) {
        Err(_) => {
            println!("couldn't tell which checks are VM tests, so all of them take the slot");
            vms.extend_from_slice(targets);
        }
        Ok(pending) => {
            for t in targets {
                let o = output(cmd("nix-store", &["-q", "-R", t.trim_end_matches("^*")]))?;
                if o.status.success()
                    && !pending
                        .iter()
                        .any(|p| String::from_utf8_lossy(&o.stdout).contains(p))
                {
                    plain.push(t.clone());
                } else {
                    vms.push(t.clone());
                }
            }
        }
    }
    let mut ok = true;
    if !plain.is_empty() {
        let mut c = cmd("nix", &[]);
        c.args(args(&plain, false));
        ok = run(c)?;
    }
    if !vms.is_empty() {
        println!(
            "waiting for a heavy slot for {} checks with VM tests",
            vms.len()
        );
        ok = in_slot(&args(&vms, true))? && ok;
    }
    Ok(ok)
}
#[cfg(test)]
mod tests {
    #[test]
    fn names_only_what_will_be_built() {
        let out = "this derivation will be built:\n  /nix/store/aaa-codex-1.0.drv\nthese 2 derivations will be built:\n  /nix/store/bbb-x.drv\n  /nix/store/ccc-y.drv\nthese 3 paths will be fetched (1 MiB download):\n  /nix/store/ddd-z\n";
        assert_eq!(super::cold_names(out), ["codex-1.0", "x", "y"]);
        assert!(super::cold_names("this path will be fetched:\n  /nix/store/d-z\n").is_empty());
    }
}
