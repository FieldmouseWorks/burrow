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
/// Derivation names a `nix build --dry-run` says it will build, not fetch.
pub fn cold_names(dry_run_stderr: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut building = false;
    for line in dry_run_stderr.lines() {
        let l = line.trim();
        if l.contains("will be built:") {
            building = true;
        } else if l.contains("will be fetched") || l.contains("don't know how to build") {
            building = false;
        } else if building && l.starts_with("/nix/store/") {
            let name = l.rsplit('/').next().unwrap_or(l);
            let name = name.split_once('-').map_or(name, |(_, n)| n);
            names.push(name.trim_end_matches(".drv").to_owned());
        }
    }
    names.sort();
    names.dedup();
    names
}
/// Say by name what this leg will compile because no cache has it, so a long
/// quiet build isn't mistaken for a hang. Informational: it never fails a leg.
fn announce_cold(targets: &[String]) {
    let mut c = cmd("nix-store", &["--realise", "--dry-run"]);
    c.args(targets.iter().map(|t| t.trim_end_matches("^*")));
    let Ok(o) = output(c) else { return };
    let names = cold_names(&String::from_utf8_lossy(&o.stderr));
    if names.is_empty() {
        return;
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
}
pub fn checks(link: Option<&Path>, targets: &[String], roots: &Path) -> Result<bool> {
    if targets.is_empty() {
        return Ok(true);
    }
    announce_cold(targets);
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
