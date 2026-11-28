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
fn vm_derivations(json: &[u8]) -> Result<Vec<String>> {
    let map = derivations_map(json)?;
    let mut names = Vec::new();
    for (key, val) in &map {
        let env = val
            .get("env")
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| err("a derivation with no env"))?;
        let mut features: Vec<String> = Vec::new();
        if let Some(v) = env.get("requiredSystemFeatures")
            && !v.is_null()
        {
            let text = v
                .as_str()
                .ok_or_else(|| err("requiredSystemFeatures in env is not a string"))?;
            features.extend(
                text.split([' ', '\t', '\n', '\r'])
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned),
            );
        }
        if let Some(v) = val
            .get("structuredAttrs")
            .and_then(|s| s.get("requiredSystemFeatures"))
            && !v.is_null()
        {
            let arr = v
                .as_array()
                .ok_or_else(|| err("structuredAttrs.requiredSystemFeatures is not an array"))?;
            for item in arr {
                if let Some(s) = item.as_str() {
                    features.push(s.to_owned());
                }
            }
        }
        if let Some(v) = env.get("__json")
            && !v.is_null()
        {
            let raw = v
                .as_str()
                .ok_or_else(|| err("env.__json is not a string"))?;
            let parsed: serde_json::Value = serde_json::from_str(raw)?;
            if let Some(arr) = parsed
                .get("requiredSystemFeatures")
                .filter(|v| !v.is_null())
            {
                let arr = arr
                    .as_array()
                    .ok_or_else(|| err("env.__json.requiredSystemFeatures is not an array"))?;
                for item in arr {
                    if let Some(s) = item.as_str() {
                        features.push(s.to_owned());
                    }
                }
            }
        }
        if features.iter().any(|f| f == "kvm" || f == "nixos-test") {
            names.push(key.rsplit('/').next().unwrap_or(key).to_owned());
        }
    }
    Ok(names)
}
fn unbuilt_vms(targets: &[String], roots: &Path) -> Result<Vec<String>> {
    let mut c = cmd("nix", &["derivation", "show", "-r"]);
    c.args(
        targets
            .iter()
            .map(|t| format!("{}^*", t.trim_end_matches("^*"))),
    );
    let json = checked(c)?;
    let names = vm_derivations(&json)?;
    let store = Path::new(&targets[0])
        .parent()
        .ok_or_else(|| err("no derivation directory"))?;
    let mut pending = Vec::new();
    let mut i = 0;
    for name in &names {
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
        } else if building && l.starts_with('/') && l.ends_with(".drv") {
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
fn announce_cold(targets: &[String]) {
    let mut c = cmd("nix-store", &["--realise", "--dry-run"]);
    c.args(targets.iter().map(|t| t.trim_end_matches("^*")));
    let Ok(o) = output(c) else { return };
    let said = String::from_utf8_lossy(&o.stderr);
    let names = cold_names(&said);
    if names.is_empty() {
        return;
    }
    let mut msg = format!(
        "cold build: {} derivations no cache has will be built here: {}",
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
}
pub fn checks(link: Option<&Path>, targets: &[String], roots: &Path) -> Result<bool> {
    if targets.is_empty() {
        return Ok(true);
    }
    announce_cold(targets);
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
        let custom = "this derivation will be built:\n  /tmp/alt-store/aaa-cold.drv\nthis path will be fetched:\n  /tmp/alt-store/bbb-cached\n";
        assert_eq!(super::cold_drvs(custom), ["/tmp/alt-store/aaa-cold.drv"]);
        assert_eq!(super::cold_names(custom), ["cold"]);
    }

    #[test]
    fn vm_derivations_reads_env_structured_attrs_and_json_in_both_formats() {
        let newer = br#"{
            "version": 4,
            "derivations": {
                "aaa-env.drv": {"env": {"requiredSystemFeatures": "big-parallel\tkvm"}},
                "bbb-struct.drv": {"env": {}, "structuredAttrs": {"requiredSystemFeatures": ["nixos-test"]}},
                "ccc-plain.drv": {"env": {"requiredSystemFeatures": "big-parallel"}}
            }
        }"#;
        assert_eq!(
            super::vm_derivations(newer).unwrap(),
            ["aaa-env.drv", "bbb-struct.drv"]
        );
        let older = br#"{
            "/nix/store/ddd-json.drv": {"env": {"__json": "{\"requiredSystemFeatures\":[\"kvm\"]}"}},
            "/nix/store/eee-plain.drv": {"env": {}}
        }"#;
        assert_eq!(super::vm_derivations(older).unwrap(), ["ddd-json.drv"]);
        assert!(super::vm_derivations(br#"{"aaa.drv": {}}"#).is_err());
    }
}
