use super::*;
use conary_core::plugin::{Capabilities, Kind, Sandbox};
use std::collections::BTreeSet;
use std::os::unix::net::UnixListener;

struct Machine {
    dir: tempfile::TempDir,
    cfg: Config,
    header: Header,
    _listeners: Vec<UnixListener>,
}

impl Machine {
    fn new(approved: &[&str]) -> Machine {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = crate::new::tests::config(dir.path());
        cfg.engines.get_mut("claude").unwrap().capabilities =
            approved.iter().map(|&n| n.to_owned()).collect();
        std::fs::create_dir_all(cfg.src.join("web/.git")).unwrap();
        let job = cfg.plugins.job_sockets.join("job-1");
        std::fs::create_dir_all(&job).unwrap();
        let listeners = ["probe-1.sock", "host-jobs-1.sock"]
            .map(|name| UnixListener::bind(job.join(name)).unwrap())
            .into();
        let header = Header {
            request: String::new(),
            probe: Some(job.join("probe-1.sock")),
            jobs: Some(job.join("host-jobs-1.sock")),
            instructions: None,
        };
        Machine {
            dir,
            cfg,
            header,
            _listeners: listeners,
        }
    }

    fn task(&self, workdir: &Path) -> Task {
        Task::Job {
            repo: "o/web".into(),
            workdir: workdir.to_str().unwrap().into(),
            prompt: "work".into(),
            model: None,
            effort: None,
        }
    }

    fn attach(&self) -> Attach {
        let offered = Capabilities {
            sandbox: Sandbox::Needs,
            network: true,
            devices: vec!["kvm".into()],
            mcp: true,
        };
        let grants = self.cfg.grants("claude", &[Kind::Job], &offered).unwrap();
        let task = self.task(&self.cfg.src.join("web"));
        let login = Path::new("/run/keys/claude");
        Attach::granted(&self.cfg, Some(login), &grants, &self.header, &task).unwrap()
    }
}

fn held(a: &Attach) -> Vec<String> {
    serde_json::from_str(&a.env["PROJ_CAPABILITIES"]).unwrap()
}

fn grantable() -> Vec<&'static str> {
    Capability::ALL
        .iter()
        .filter(|c| !matches!(c, Capability::Github | Capability::Environment))
        .map(|c| c.name())
        .collect()
}

#[test]
fn describe_is_given_nothing() {
    let m = Machine::new(&grantable());
    let a = Attach::nothing(&m.cfg);
    assert!(a.credential.is_none() && a.workspace.is_none());
    assert!(!a.network && !a.kvm && !a.git_metadata && !a.nic());
    assert!(a.services.is_empty() && a.closures.is_empty());
    assert!(held(&a).is_empty());
}

#[test]
fn every_grant_has_its_own_form() {
    let m = Machine::new(&grantable());
    let a = m.attach();
    assert_eq!(held(&a), grantable());
    assert!(a.absent.is_empty(), "{:?}", a.absent);
    assert!(a.network && a.kvm && a.git_metadata);
    assert_eq!(a.credential.as_deref(), Some(Path::new("/run/keys/claude")));
    let sockets: Vec<_> = a
        .services
        .iter()
        .map(|s| s.socket.to_str().unwrap())
        .collect();
    assert_eq!(
        sockets,
        ["host-jobs", "probe", "units-clear", "heavy", "mcp-0"]
            .map(|name| format!("/run/agent/{name}.sock"))
    );
    assert_eq!(a.closures.len(), 3);
    // proj's jobs socket is never passed through whole.
    assert!(
        matches!(a.services[0].does, Does::Jobs(_)),
        "{:?}",
        a.services[0]
    );
    // The approved server, and not the one that acts as the owner.
    let servers: serde_json::Value = serde_json::from_str(&a.env["PROJ_MCP_SERVERS"]).unwrap();
    assert_eq!(
        servers.as_object().unwrap().keys().collect::<Vec<_>>(),
        ["ledger"]
    );
    let Does::Server { command, .. } = &a.services[4].does else {
        panic!("{:?}", a.services[4]);
    };
    assert_eq!(command, "/bin/ledger");
    assert!(!a.env["PROJ_MCP_SERVERS"].contains("/bin/ledger"));
}

#[test]
fn each_thing_attached_needs_its_grant() {
    // What's attached for one grant is there for that one alone.
    let attached = |a: &Attach| {
        let mut what = BTreeSet::new();
        let mut add = |yes: bool, name: &str| {
            if yes {
                what.insert(name.to_owned());
            }
        };
        add(a.network, "network");
        add(a.kvm, "kvm");
        add(a.git_metadata, "git-metadata");
        add(a.workspace.is_some(), "workspace");
        for (closure, name) in [
            ("system", "system"),
            ("toolchains", "toolchains"),
            ("nix", "nix"),
        ] {
            let list = PathBuf::from(format!("/nix/store/x-{closure}-paths"));
            add(a.closures.contains(&list), name);
        }
        for (socket, name) in [
            ("host-jobs", "job-state"),
            ("probe", "probe"),
            ("units-clear", "units-clear"),
            ("heavy", "heavy"),
            ("mcp-0", "mcp"),
        ] {
            let at = PathBuf::from(format!("/run/agent/{socket}.sock"));
            add(a.services.iter().any(|s| s.socket == at), name);
        }
        what
    };
    assert!(attached(&Machine::new(&[]).attach()).is_empty());
    for name in grantable() {
        // git-metadata is the workspace's.
        let approved = if name == "git-metadata" {
            vec!["workspace", name]
        } else {
            vec![name]
        };
        let a = Machine::new(&approved).attach();
        let expected: BTreeSet<String> = approved.iter().map(|&n| n.to_owned()).collect();
        assert_eq!(attached(&a), expected, "{name}");
        let held: BTreeSet<String> = held(&a).into_iter().collect();
        assert_eq!(held, expected, "{name}");
    }
}

#[test]
fn nothing_of_the_callers_environment_is_the_plugins() {
    let a = Machine::new(&grantable()).attach();
    let names: Vec<&str> = a.env.keys().map(String::as_str).collect();
    for name in [
        "GH_TOKEN",
        "GITHUB_TOKEN",
        "GH_TOKEN_SOCKET",
        "PROJ_CONFIG",
        "PROJ_JOB",
    ] {
        assert!(!names.contains(&name), "{name}");
    }
    assert_eq!(a.env["PROJ_PROBE_SOCKET"], "/run/agent/probe.sock");
    assert_eq!(a.env["PROJ_HOST_JOBS_SOCKET"], "/run/agent/host-jobs.sock");
}

#[test]
fn what_has_nothing_behind_it_is_said_and_not_held() {
    let mut m = Machine::new(&["git-metadata", "probe", "heavy", "units-clear"]);
    m.header.probe = None;
    m.cfg.plugins.heavy = None;
    m.cfg.plugins.units_clear = None;
    let a = m.attach();
    assert!(held(&a).is_empty());
    assert!(!a.nic());
    let absent: Vec<_> = a.absent.iter().map(|(c, _)| c.name()).collect();
    assert_eq!(absent, ["git-metadata", "probe", "units-clear", "heavy"]);
}

#[test]
fn only_a_jobs_own_socket_of_proj_is_relayed() {
    let m = Machine::new(&[]);
    let root = &m.cfg.plugins.job_sockets;
    let job = root.join("job-1");
    let probe = job.join("probe-1.sock");
    assert_eq!(
        job_socket(root, Some(&probe), "probe-"),
        Some(probe.canonicalize().unwrap())
    );
    // Another kind of socket, under proj's name for neither.
    let token = job.join("token.sock");
    let _token = UnixListener::bind(&token).unwrap();
    assert_eq!(job_socket(root, Some(&token), "probe-"), None);
    assert_eq!(job_socket(root, Some(&probe), "host-jobs-"), None);
    // The name, outside a job's scratch.
    let outside = m.dir.path().join("probe-1.sock");
    let _outside = UnixListener::bind(&outside).unwrap();
    assert_eq!(job_socket(root, Some(&outside), "probe-"), None);
    // The name, linked to a socket elsewhere, and on a file.
    let linked = job.join("probe-2.sock");
    std::os::unix::fs::symlink(&token, &linked).unwrap();
    assert_eq!(job_socket(root, Some(&linked), "probe-"), None);
    let file = job.join("probe-3.sock");
    std::fs::write(&file, "").unwrap();
    assert_eq!(job_socket(root, Some(&file), "probe-"), None);
}

#[test]
fn a_workspace_is_a_project_of_its_own_and_nothing_wider() {
    let m = Machine::new(&[]);
    let src = &m.cfg.src;
    let at = |workdir: PathBuf| workspace(&m.cfg, &m.task(&workdir));
    let web = at(src.join("web/sub")).unwrap().unwrap();
    assert_eq!(web.mount, src.join("web"));
    assert_eq!(web.workdir, src.join("web/sub"));
    std::fs::create_dir_all(src.join("plain")).unwrap();
    // To a repository, so that being a link is all that's wrong with it.
    std::os::unix::fs::symlink(src.join("web"), src.join("link")).unwrap();
    for outside in [
        src.clone(),
        src.join("web/../../.."),
        src.join(".hidden"),
        src.join("plain"),
        src.join("link"),
        src.join("gone"),
        "/etc".into(),
        ".".into(),
    ] {
        assert!(at(outside.clone()).is_err(), "{}", outside.display());
    }
    // Conary's conformance job has no project.
    let fixture = fixtures()
        .into_iter()
        .find(|f| matches!(f.task, Task::Job { .. }))
        .unwrap();
    assert_eq!(workspace(&m.cfg, &fixture.task), Ok(None));
}

#[test]
fn a_jobs_turn_instructions_reach_its_vm_and_nothing_else_of_the_host() {
    let mut m = Machine::new(&[]);
    assert!(!m.attach().env.contains_key(turn::INSTRUCTIONS));
    m.header.instructions = Some("end with proj done".into());
    let a = m.attach();
    assert_eq!(a.env[turn::INSTRUCTIONS], "end with proj done");
    // Describe is given nothing, the turn's word included.
    assert!(!Attach::nothing(&m.cfg).env.contains_key(turn::INSTRUCTIONS));
}
