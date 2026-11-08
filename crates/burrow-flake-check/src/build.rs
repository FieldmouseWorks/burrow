//! Shared build policy: root cached VM outputs, then build only pending VMs
//! under heavy. Unknown closures conservatively take a slot.
use super::*;
fn has_slots() -> bool {
    env::split_paths(&env::var_os("PATH").unwrap_or_default()).any(|p| p.join("heavy").is_file())
}
fn in_slot(args: &[String]) -> Result<bool> {
    if has_slots() {
        let mut c = cmd(
            "heavy",
            &[
                "--",
                "bash",
                "-c",
                "exec \"$@\" --max-jobs \"$HEAVY_JOBS\"",
                "bash",
                "nix",
            ],
        );
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
pub fn checks(link: Option<&Path>, targets: &[String], roots: &Path) -> Result<bool> {
    if targets.is_empty() {
        return Ok(true);
    }
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
