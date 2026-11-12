//! The execution boundary for engine plugins. A plugin gets the shared
//! contract ([`super::capabilities`]) narrowed to the capabilities it
//! declared in `describe` *and* the owner approved in `burrow.proj.plugins`;
//! its code, `describe` and conformance runs included, executes in the agent
//! sandbox on the filtered egress allowlist, never `--open`.
//!
//! A plugin never holds GitHub authority or the job's login environment: the
//! App token and its socket stay on the host, and anything a job is
//! authorized to do with GitHub reaches it through host-mediated services
//! (the probe), which are capabilities like any other.

use super::capabilities::Capabilities;
pub use super::capabilities::Capability;
use serde::Deserialize;
use std::{collections::BTreeSet, path::Path, process::Command};

/// What no plugin is ever given, declared or approved: the App token's
/// socket and the login shell's environment, where its credentials are.
pub const NEVER: &[Capability] = &[Capability::Github, Capability::Environment];

/// What every engine must be able to do to run a job at all. A plugin
/// whose grant lacks one of these fails conformance.
pub const REQUIRED: &[Capability] = &[
    Capability::Workspace,
    Capability::JobState,
    Capability::GitMetadata,
    Capability::System,
    Capability::Probe,
];

/// The part of a plugin's `describe` (Conary's capability description) that
/// the boundary reads.
#[derive(Debug, Deserialize)]
pub struct Describe {
    pub capabilities: Vec<Capability>,
}

/// What the plugin may use: declared, approved, and not [`NEVER`].
pub fn granted(describe: &Describe, approved: &[Capability]) -> BTreeSet<Capability> {
    describe
        .capabilities
        .iter()
        .filter(|c| approved.contains(c) && !NEVER.contains(c))
        .copied()
        .collect()
}

/// The shared contract as this plugin holds it. Nothing it didn't get is
/// there to read: the whole grant goes, its paths with it.
pub(super) fn narrow(
    capabilities: Capabilities,
    describe: &Describe,
    approved: &[Capability],
) -> Capabilities {
    let keep = granted(describe, approved);
    capabilities.retain(|c| keep.contains(&c))
}

/// The conformance every plugin passes, in place of a per-engine parity
/// check: with this plugin's approval, the grant it ends up with holds
/// everything in `required`. Err names what's missing.
pub fn conformance(
    describe: &Describe,
    approved: &[Capability],
    required: &[Capability],
) -> Result<(), Vec<Capability>> {
    let held = granted(describe, approved);
    let missing: Vec<_> = required
        .iter()
        .filter(|c| !held.contains(c))
        .copied()
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(missing)
    }
}

/// Run `argv` in agent sandbox `id` (made with `agent new PROJECT`, which
/// has the allowlist unless `--open`). `describe` and conformance runs use
/// this too: no plugin code starts on the host.
pub fn in_sandbox(agent: &Path, id: &str, argv: &[&str]) -> Command {
    let mut c = Command::new(agent);
    c.args(["sh", id]).args(argv);
    c
}

#[cfg(test)]
mod tests {
    use super::*;
    use Capability::*;

    fn describe(caps: &[Capability]) -> Describe {
        Describe {
            capabilities: caps.to_vec(),
        }
    }

    #[test]
    fn grant_is_declared_and_approved_and_never_credentials() {
        let d = describe(&[Workspace, Heavy, Github, Environment]);
        // Heavy isn't approved; Kvm isn't declared; Github and Environment
        // are both, and still never.
        let g = granted(&d, &[Workspace, Kvm, Github, Environment]);
        assert_eq!(g, BTreeSet::from([Workspace]));
    }

    #[test]
    fn removing_a_required_capability_fails_conformance() {
        let all = [Workspace, JobState, GitMetadata, System, Probe];
        assert_eq!(conformance(&describe(&all), &all, REQUIRED), Ok(()));
        let without: Vec<_> = all.iter().copied().filter(|c| *c != Probe).collect();
        assert_eq!(
            conformance(&describe(&without), &all, REQUIRED),
            Err(vec![Probe])
        );
        // Approval is half the grant too.
        assert_eq!(
            conformance(&describe(&all), &without, REQUIRED),
            Err(vec![Probe])
        );
    }

    #[test]
    fn plugin_profile_has_no_credentials_network_settings_or_undeclared_paths() {
        use super::super::sandbox;
        use crate::{config::Config, job::Job};
        let dir = tempfile::tempdir().unwrap();
        let mut cfg: Config = serde_json::from_value(crate::config::tests::example()).unwrap();
        cfg.jobs = dir.path().join("jobs");
        cfg.tools.git = "git".into();
        let job = Job::new(&cfg, "plugin");
        std::fs::create_dir_all(&job.dir).unwrap();
        std::fs::write(
            job.path("project"),
            dir.path().join("project").to_str().unwrap(),
        )
        .unwrap();
        let home = dir.path().join("home");
        let tokens = dir.path().join("run/tokens");
        let env = |name: &str| match name {
            "HOME" => Some(home.clone().into_os_string()),
            "GH_TOKEN_SOCKET" => Some(tokens.join("sock").into_os_string()),
            "GH_TOKEN" => Some("ghs_secret".into()),
            _ => None,
        };
        let machine = sandbox::tests::machine_in(dir.path());
        let scratch = dir.path().join("scratch");
        let all = [
            Workspace,
            JobState,
            GitMetadata,
            System,
            Probe,
            Github,
            Network,
            Kvm,
            Heavy,
            Environment,
        ];
        // Declares everything; approved for everything but Kvm and Network.
        let d = describe(&all);
        let approved: Vec<_> = all
            .iter()
            .copied()
            .filter(|c| *c != Kvm && *c != Network)
            .collect();
        let narrowed = sandbox::flags_with(&cfg, &job, &scratch, &env, &machine, |c| {
            narrow(c, &d, &approved)
        })
        .unwrap();
        let profile = narrowed.flags.join("\n");
        let full = sandbox::flags(&cfg, &job, &scratch, &env, &machine)
            .unwrap()
            .flags
            .join("\n");
        // The App's socket directory is in the built-in engines' profile only.
        assert!(full.contains(tokens.to_str().unwrap()));
        assert!(!profile.contains(tokens.to_str().unwrap()));
        assert!(!profile.contains("ghs_secret"));
        assert!(!profile.contains("/dev/kvm"), "approval is half the grant");
        assert!(
            !profile.contains(".cargo"),
            "undeclared toolchains stay out"
        );
        assert!(profile.contains("network.enabled=false"));
        assert!(profile.contains("shell_environment_policy.inherit=\"none\""));
        assert!(profile.contains(dir.path().join("project").to_str().unwrap()));
    }

    #[test]
    fn sandbox_command_never_runs_on_the_host_shell() {
        let c = in_sandbox(Path::new("/bin/agent"), "7", &["plugin", "describe"]);
        let args: Vec<_> = c.get_args().collect();
        assert_eq!(args, ["sh", "7", "plugin", "describe"]);
    }
}
