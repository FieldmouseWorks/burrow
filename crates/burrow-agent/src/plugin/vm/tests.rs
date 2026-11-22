use super::*;
use crate::plugin::render::{Service, Workspace};

const DIR: &str = "/run/agent-plugin-run/claude-1/work";
const PROXY: &str = "/nix/store/x-proxy-3128";

fn args(a: &Attach) -> String {
    qemu_args(Path::new(DIR), a, Path::new(PROXY))
}

fn service() -> Service {
    Service {
        socket: "/run/agent/probe.sock".into(),
        does: Does::Relay("/scratch/jobs/j/probe-1.sock".into()),
    }
}

#[test]
fn an_empty_vm_has_the_port_and_no_more() {
    let got = args(&Attach::default());
    let mut want = "-device virtio-serial-pci -chardev socket,id=agentrpc,path=rpc.sock \
                    -device virtserialport,chardev=agentrpc,name=agent.plugin "
        .to_owned();
    if cfg!(target_arch = "x86_64") {
        want += "-cpu host,+x2apic,-sgx,-vmx,-svm ";
    }
    assert_eq!(got, want + "\n");
}

#[test]
fn no_vm_of_a_plugins_has_a_vsock_or_an_open_proxy() {
    let all = Attach {
        network: true,
        kvm: true,
        credential: Some("/run/keys/claude".into()),
        services: vec![service()],
        ..Attach::default()
    };
    let got = args(&all);
    assert!(!got.contains("vsock"));
    assert!(!got.contains("3129"));
    assert!(got.contains("restrict=on,ipv6=off,"));
}

#[test]
fn the_proxy_is_forwarded_for_the_network_alone() {
    let services = Attach {
        services: vec![service()],
        ..Attach::default()
    };
    let got = args(&services);
    assert!(got.contains(&format!("guestfwd=tcp:10.0.2.101:7000-cmd:{DIR}/svc-0 ")));
    assert!(!got.contains(PROXY));
    let network = Attach {
        network: true,
        ..Attach::default()
    };
    let got = args(&network);
    assert!(got.contains(&format!("guestfwd=tcp:10.0.2.100:3128-cmd:{PROXY} ")));
    assert!(!got.contains("10.0.2.101"));
}

#[test]
fn the_login_share_and_nested_kvm_come_only_attached() {
    let a = Attach {
        credential: Some("/run/keys/claude".into()),
        kvm: true,
        workspace: Some(Workspace {
            project: "web".into(),
            mount: "/src/web".into(),
            workdir: "/src/web".into(),
        }),
        ..Attach::default()
    };
    let got = args(&a);
    assert!(got.contains("agent.credential,file=credential "));
    // The file is the call's own copy: no engine's path is named to QEMU.
    assert!(!got.contains("/run/keys"));
    assert!(got.contains("tag=workspace "));
    assert!(!got.contains("-cpu"));
    assert!(!got.contains("-netdev"));
}

#[test]
fn the_repository_is_covered_where_the_guest_cannot_uncover_it() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join(".git")).unwrap();
    assert_eq!(
        covered(dir.path()).unwrap(),
        [
            "--tmpfs",
            "/workspace/.git",
            "--remount-ro",
            "/workspace/.git"
        ]
    );
    // A link in its place has nothing bwrap can cover: no run.
    fs::remove_dir(dir.path().join(".git")).unwrap();
    symlink("/elsewhere", dir.path().join(".git")).unwrap();
    assert!(covered(dir.path()).is_err());
}
