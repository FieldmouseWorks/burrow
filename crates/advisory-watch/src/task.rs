//! What a project's job is told: everything waiting for it, and what to do
//! about it. The machine's own job patches the machine; a project's gets
//! its dependencies fixed.

use crate::{Config, Finding};

pub fn text(cfg: &Config, project: &str, found: &[&Finding]) -> String {
    let list: Vec<String> = found.iter().map(|f| format!("- {}", f.text)).collect();
    let list = list.join("\n");
    let Config {
        host,
        channel,
        update,
        flake,
        owner,
        notify,
        ..
    } = cfg;
    if project == cfg.project {
        let checks: Vec<String> = cfg
            .checks
            .iter()
            .map(|c| format!(".#checks.{}.{c}", cfg.system))
            .collect();
        let checks = checks.join(" ");
        // Only the merge train moves main in a train repo: the patch goes up as a PR.
        let deploy = if cfg.train {
            format!(
                "push it to a branch of your own and open a PR against main, which the steward settles and the merge train lands (nothing else may move main); {update} deploys it once it has."
            )
        } else {
            format!(
                "push to main, then `sudo systemctl start --no-block {update}` to deploy it now."
            )
        };
        let kernel = cfg.boot.as_ref().map_or(String::new(), |boot| {
            format!(" If the fix is in the kernel, `sudo {boot} trial` once it's confirmed.")
        });
        let verdicts = cfg.state.join("verdicts.md");
        let verdicts = verdicts.display();
        format!(
            "advisory-watch found new vulnerabilities in what {host} runs:
{list}

For each one: does it really affect {host} as it runs now? Check the version and the patches nixpkgs applies; vulnix and name matching both over-report. If the {channel} channel already carries the fix, {update} deploys it on its next run (see `{update} status`). If it doesn't, and it matters (known exploited, reachable from the network, or CVSS 9+), patch {host} now: an overlay in the flake that applies the upstream fix (system.replaceDependencies for a library, so the world doesn't rebuild), noted in docs/patches.md with the CVE and the nixpkgs version that will carry the fix, so the patch can go once the channel has it. Work in your own worktree (git -C {flake} worktree add \"$PROJ_SCRATCH/fix\" origin/main), never in {flake} itself. Build and run `nix build -L {checks}`, {deploy}{kernel} Write one line per CVE (affected or not, and what you did) to {verdicts}. If something needs {owner}, run `{notify} open advisory \"title\" \"what they need to do\"`."
        )
    } else {
        format!(
            "advisory-watch found new vulnerabilities in this project's dependencies (osv-scanner):
{list}

Renovate opens security PRs for these on its own, without the usual wait. Make sure each fix lands today: if Renovate's PR exists, get its CI green and let it merge; if Renovate can't fix it (a transitive dependency, no fixed version yet, a breaking bump), make the fix yourself on a branch and open a PR. If a vulnerability can't affect this project (dev-only tooling, code path not used), say why in one line instead. If something needs {owner}, run `{notify} open advisory-{project} \"title\" \"what they need to do\"`."
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(train: bool, boot: Option<&str>) -> Config {
        serde_json::from_value(serde_json::json!({
            "state": "/var/lib/burrow/advisories", "src": "/src", "user": "sam", "project": "box",
            "host": "boxhost", "channel": "nixos-26.05", "update": "box-update", "flake": "/src/box",
            "owner": "Sam", "notify": "box-notify", "boot": boot, "train": train,
            "checks": ["rehearsal", "sandbox"], "system": "x86_64-linux", "app": null, "kev": [],
            "tools": {"sudo": "/s", "runuser": "/r", "nixStore": "/n", "vulnix": "/v", "osvScanner": "/o", "proj": "/p"},
        }))
        .unwrap()
    }

    /// What updates/advisory-watch.sh told the machine's job, run on this
    /// config, word for word.
    const MACHINE: &str = "advisory-watch found new vulnerabilities in what boxhost runs:
- CVE-1 (KNOWN EXPLOITED): a bug
- CVE-2 in openssl-3 (CVSS 9.8)

For each one: does it really affect boxhost as it runs now? Check the version and the patches nixpkgs applies; vulnix and name matching both over-report. If the nixos-26.05 channel already carries the fix, box-update deploys it on its next run (see `box-update status`). If it doesn't, and it matters (known exploited, reachable from the network, or CVSS 9+), patch boxhost now: an overlay in the flake that applies the upstream fix (system.replaceDependencies for a library, so the world doesn't rebuild), noted in docs/patches.md with the CVE and the nixpkgs version that will carry the fix, so the patch can go once the channel has it. Work in your own worktree (git -C /src/box worktree add \"$PROJ_SCRATCH/fix\" origin/main), never in /src/box itself. Build and run `nix build -L .#checks.x86_64-linux.rehearsal .#checks.x86_64-linux.sandbox`, ";

    const ENDING: &str = " Write one line per CVE (affected or not, and what you did) to /var/lib/burrow/advisories/verdicts.md. If something needs Sam, run `box-notify open advisory \"title\" \"what they need to do\"`.";

    #[test]
    fn the_machines_job_is_told_how_to_patch_it() {
        let found = [
            Finding::new("box", "kev:CVE-1", "CVE-1 (KNOWN EXPLOITED): a bug"),
            Finding::new(
                "box",
                "vulnix:CVE-2:openssl-3",
                "CVE-2 in openssl-3 (CVSS 9.8)",
            ),
        ];
        let found: Vec<&Finding> = found.iter().collect();
        let push =
            "push to main, then `sudo systemctl start --no-block box-update` to deploy it now.";
        assert_eq!(
            text(&config(false, None), "box", &found),
            format!("{MACHINE}{push}{ENDING}")
        );
        let pr = "push it to a branch of your own and open a PR against main, which the steward settles and the merge train lands (nothing else may move main); box-update deploys it once it has.";
        let kernel = " If the fix is in the kernel, `sudo box-boot trial` once it's confirmed.";
        assert_eq!(
            text(&config(true, Some("box-boot")), "box", &found),
            format!("{MACHINE}{pr}{kernel}{ENDING}")
        );
    }

    #[test]
    fn a_projects_job_is_told_to_land_its_fixes() {
        let found = [Finding::new(
            "app",
            "osv:app:GHSA-1:rustls",
            "GHSA-1 in rustls 0.23.1 (severity 7.5)",
        )];
        assert_eq!(
            text(&config(false, None), "app", &[&found[0]]),
            "advisory-watch found new vulnerabilities in this project's dependencies (osv-scanner):
- GHSA-1 in rustls 0.23.1 (severity 7.5)

Renovate opens security PRs for these on its own, without the usual wait. Make sure each fix lands today: if Renovate's PR exists, get its CI green and let it merge; if Renovate can't fix it (a transitive dependency, no fixed version yet, a breaking bump), make the fix yourself on a branch and open a PR. If a vulnerability can't affect this project (dev-only tooling, code path not used), say why in one line instead. If something needs Sam, run `box-notify open advisory-app \"title\" \"what they need to do\"`."
        );
    }
}
