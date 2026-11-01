//! `agent new`: a fresh sandbox for a project, on a clone of the data
//! dataset as of now, for free until something changes.

use std::fs::{self, File};
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::Stdio;

use crate::path::{project_name, resolve, slug};
use crate::sandbox::{self, Sandbox};
use crate::{Agent, Config, hostname, read, ssh, touch, write_line};

/// The new sandbox's project, the paths it also gets, and whether it skips
/// the allowlist.
#[derive(Debug, PartialEq, Eq)]
pub struct Ask {
    pub project: String,
    pub withs: Vec<PathBuf>,
    pub open: bool,
}

pub fn usage(cfg: &Config) -> String {
    format!(
        "usage: agent new PROJECT [--with {}/PATH]... [--open]",
        cfg.data.display()
    )
}

/// What `agent new` was asked, the project checked to be a folder directly
/// in the projects folder, and each --with path a path under the data
/// dataset's mountpoint, both there.
pub fn ask(cfg: &Config, args: &[String], cwd: &Path) -> Result<Ask, String> {
    let Some((p, rest)) = args.split_first() else {
        return Err(usage(cfg));
    };
    if !project_name(p) {
        return Err(format!("not a project name: {p}"));
    }
    let at = cfg.src.join(p);
    if at.is_symlink() {
        return Err(format!("a symlink, not a project folder: {}", at.display()));
    }
    if !at.is_dir() {
        return Err(format!("no such project: {}", at.display()));
    }
    let mut ask = Ask {
        project: p.clone(),
        withs: Vec::new(),
        open: false,
    };
    let mut rest = rest.iter();
    while let Some(o) = rest.next() {
        match o.as_str() {
            "--open" => ask.open = true,
            "--with" => {
                let Some(w) = rest.next().filter(|w| !w.is_empty()) else {
                    return Err(format!("--with needs a path under {}", cfg.data.display()));
                };
                let path = resolve(Path::new(w), cwd).map_err(|e| format!("--with {w}: {e}"))?;
                let under = path
                    .strip_prefix(&cfg.data)
                    .is_ok_and(|r| !r.as_os_str().is_empty());
                if !under {
                    return Err(format!(
                        "--with takes a path under {}, not {w}",
                        cfg.data.display()
                    ));
                }
                if !path.exists() {
                    return Err(format!("no such path: {}", path.display()));
                }
                ask.withs.push(path);
            }
            _ => return Err(format!("unknown option: {o}")),
        }
    }
    Ok(ask)
}

/// The user's side: their key to the sandboxes, then the sandbox (made as
/// root), then the wait for it to come up. Prints its id.
pub fn new(agent: &Agent, args: &[String]) -> Result<String, String> {
    let cfg = agent.cfg;
    let key = cfg.key();
    let keys = key.parent().unwrap_or(Path::new("/"));
    fs::create_dir_all(keys).map_err(|e| format!("{}: {e}", keys.display()))?;
    fs::set_permissions(keys, fs::Permissions::from_mode(0o700))
        .map_err(|e| format!("{}: {e}", keys.display()))?;
    if !key.is_file() {
        let comment = format!("agent@{}", hostname());
        let key = key.to_string_lossy();
        agent.run(
            "ssh-keygen",
            &["-q", "-t", "ed25519", "-N", "", "-C", &comment, "-f", &key],
        )?;
    }
    let mut words = vec!["_new".to_owned()];
    words.extend(args.iter().cloned());
    let made = agent
        .as_root_command(&words)
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()
        .map_err(|e| format!("{}: {e}", cfg.sudo.display()))?;
    if !made.status.success() {
        return Err(String::new());
    }
    let said = String::from_utf8_lossy(&made.stdout);
    let id = said.lines().last().unwrap_or("").trim().to_owned();
    let sb = Sandbox::find(cfg, &id)?;
    ssh::wait_ready(agent, &sb)?;
    Ok(id)
}

/// Root's side: the clone, the sandbox's directory and what's in it, then
/// its VM started. Returns its id.
pub fn make(agent: &Agent, ask: &Ask) -> Result<String, String> {
    let cfg = agent.cfg;
    let n = next_number(&cfg.root)?;
    let id = format!("{}-{n}", slug(&ask.project));
    let d = cfg.root.join(&id);
    let snapshot = format!("{}@agent-{id}", cfg.data_dataset);
    let mine = format!("{}/{id}", cfg.dataset);
    agent.run("zfs", &["snapshot", &snapshot])?;
    agent.run("zfs", &["create", &mine])?;
    agent.run("zfs", &["clone", &snapshot, &format!("{mine}/data")])?;
    let owned = format!("{}:kvm", cfg.user);
    let dir = d.to_string_lossy().into_owned();
    agent.run("chown", &[&owned, &dir])?;
    fs::set_permissions(&d, fs::Permissions::from_mode(0o770))
        .map_err(|e| format!("{dir}: {e}"))?;

    let home = d.join("home");
    let (home_s, ssh_s) = (
        home.to_string_lossy().into_owned(),
        home.join(".ssh").to_string_lossy().into_owned(),
    );
    let (user, group) = (cfg.user.as_str(), cfg.group.as_str());
    agent.run(
        "install",
        &["-d", "-m", "0700", "-o", user, "-g", group, &home_s, &ssh_s],
    )?;
    let public = format!("{}.pub", cfg.key().display());
    let authorized = home
        .join(".ssh/authorized_keys")
        .to_string_lossy()
        .into_owned();
    agent.run(
        "install",
        &["-m", "0600", "-o", user, "-g", group, &public, &authorized],
    )?;
    // Claude inside starts straight into work: no onboarding, folder trusted.
    let claude = home.join(".claude.json");
    let project = cfg.src.join(&ask.project);
    let trusted = serde_json::json!({
        "hasCompletedOnboarding": true,
        "projects": { project.to_string_lossy(): { "hasTrustDialogAccepted": true } },
    });
    let text = serde_json::to_string_pretty(&trusted).map_err(|e| e.to_string())? + "\n";
    fs::write(&claude, text).map_err(|e| format!("{}: {e}", claude.display()))?;
    let owner = format!("{user}:{group}");
    agent.run("chown", &[&owner, &claude.to_string_lossy().into_owned()])?;

    let at = |name: &str| d.join(name);
    write_line(&at(sandbox::PROJECT), &ask.project)?;
    write_line(&at(sandbox::CID), &(1000 + n).to_string())?;
    write_line(
        &at(sandbox::NET),
        if ask.open { "open" } else { "filtered" },
    )?;
    write_line(&at(sandbox::TTL_DAYS), "7")?;
    touch(&at(sandbox::LAST_USED))?;
    let (shares, mounts) = shares(cfg, &d, &ask.project, &ask.withs)?;
    fs::write(at(sandbox::SHARES), shares).map_err(|e| format!("{dir}/shares: {e}"))?;
    fs::write(at(sandbox::MOUNTS), mounts).map_err(|e| format!("{dir}/mounts: {e}"))?;
    let mut chown = vec![owned];
    for f in [
        sandbox::PROJECT,
        sandbox::CID,
        sandbox::NET,
        sandbox::TTL_DAYS,
        sandbox::LAST_USED,
        sandbox::SHARES,
        sandbox::MOUNTS,
    ] {
        chown.push(at(f).to_string_lossy().into_owned());
    }
    agent.run("chown", &chown)?;
    symlink(&cfg.runner, at("current")).map_err(|e| format!("{dir}/current: {e}"))?;
    let sb = Sandbox::find(cfg, &id)?;
    sb.write_args(agent)?;
    agent.run("systemctl", &["start", &sb.unit()])?;
    Ok(id)
}

/// The next sandbox's number, one past the last one made.
fn next_number(root: &Path) -> Result<u32, String> {
    let lock = root.join(".lock");
    let held = File::create(&lock).map_err(|e| format!("{}: {e}", lock.display()))?;
    rustix::fs::flock(&held, rustix::fs::FlockOperation::LockExclusive)
        .map_err(|e| format!("{}: {e}", lock.display()))?;
    let counter = root.join(".counter");
    let last = match fs::metadata(&counter) {
        Ok(_) => {
            let n = read(&counter)?;
            n.parse::<u32>()
                .map_err(|_| format!("{}: not a number: {n}", counter.display()))?
        }
        Err(_) => 0,
    };
    write_line(&counter, &(last + 1).to_string())?;
    Ok(last + 1)
}

/// The sandbox's shares, one a line (tag, path on the host, path in the
/// VM), and its mounts, what the VM sees (tag, path in the VM): the
/// project's clone, its home, then each --with path's clone.
pub fn shares(
    cfg: &Config,
    d: &Path,
    project: &str,
    withs: &[PathBuf],
) -> Result<(String, String), String> {
    let clone = d.join("data");
    let mut rows = vec![
        (clone.join(cfg.rel()?).join(project), cfg.src.join(project)),
        (d.join("home"), cfg.home.clone()),
    ];
    for w in withs {
        let inside = w
            .strip_prefix(&cfg.data)
            .map_err(|_| format!("{} isn't under {}", w.display(), cfg.data.display()))?;
        rows.push((clone.join(inside), w.clone()));
    }
    let (mut shares, mut mounts) = (String::new(), String::new());
    for (i, (host, vm)) in rows.iter().enumerate() {
        shares += &format!("share{i}\t{}\t{}\n", host.display(), vm.display());
        mounts += &format!("share{i}\t{}\n", vm.display());
    }
    Ok((shares, mounts))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn config(dir: &Path) -> Config {
        serde_json::from_value(serde_json::json!({
            "runner": "/nix/store/x-runner",
            "forwardFiltered": "/nix/store/x-proxy-3128",
            "forwardOpen": "/nix/store/x-proxy-3129",
            "root": dir.join("agents"),
            "src": dir.join("data/dev/src"),
            "data": dir.join("data"),
            "dataDataset": "rpool/safe/data",
            "dataset": "rpool/agents",
            "user": "dev",
            "uid": 1000,
            "group": "users",
            "home": "/home/dev",
            "tokenFile": null,
            "sudo": "/run/wrappers/bin/sudo",
            "path": "/nix/store/x/bin"
        }))
        .unwrap()
    }

    #[test]
    fn what_new_is_asked_is_checked() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().canonicalize().unwrap();
        let cfg = config(&root);
        fs::create_dir_all(cfg.src.join("web")).unwrap();
        fs::create_dir_all(root.join("data/lab")).unwrap();
        symlink(root.join("data/lab"), cfg.src.join("link")).unwrap();
        let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let ask = |a: &[&str]| super::ask(&cfg, &args(a), &root);
        assert_eq!(
            ask(&["web", "--with", "data/lab", "--open"]),
            Ok(Ask {
                project: "web".to_owned(),
                withs: vec![root.join("data/lab")],
                open: true,
            })
        );
        assert_eq!(ask(&[]), Err(usage(&cfg)));
        assert_eq!(ask(&["a/b"]), Err("not a project name: a/b".to_owned()));
        assert_eq!(ask(&[".."]), Err("not a project name: ..".to_owned()));
        assert_eq!(
            ask(&["link"]),
            Err(format!(
                "a symlink, not a project folder: {}",
                cfg.src.join("link").display()
            ))
        );
        assert_eq!(
            ask(&["gone"]),
            Err(format!(
                "no such project: {}",
                cfg.src.join("gone").display()
            ))
        );
        assert_eq!(
            ask(&["web", "--with"]),
            Err(format!("--with needs a path under {}", cfg.data.display()))
        );
        for outside in ["/etc", "data", "data/lab/../.."] {
            assert_eq!(
                ask(&["web", "--with", outside]),
                Err(format!(
                    "--with takes a path under {}, not {outside}",
                    cfg.data.display()
                ))
            );
        }
        // Through a symlink out of it, it's where the link goes.
        symlink("/etc", root.join("data/out")).unwrap();
        assert_eq!(
            ask(&["web", "--with", "data/out"]),
            Err(format!(
                "--with takes a path under {}, not data/out",
                cfg.data.display()
            ))
        );
        assert_eq!(
            ask(&["web", "--with", "data/none"]),
            Err(format!(
                "no such path: {}",
                root.join("data/none").display()
            ))
        );
        // Even where the empty path would name a folder under it.
        assert_eq!(
            super::ask(&cfg, &args(&["web", "--with", ""]), &root.join("data/lab")),
            Err(format!("--with needs a path under {}", cfg.data.display()))
        );
        symlink("grow/grow", root.join("data/grow")).unwrap();
        assert_eq!(
            ask(&["web", "--with", "data/grow/x"]),
            Err("--with data/grow/x: too many levels of symbolic links".to_owned())
        );
        assert_eq!(
            ask(&["web", "--wide"]),
            Err("unknown option: --wide".to_owned())
        );
    }

    #[test]
    fn the_shares_are_the_project_the_home_and_each_with() {
        let cfg = config(Path::new("/x"));
        let (shares, mounts) = shares(
            &cfg,
            Path::new("/x/agents/web-3"),
            "web",
            &[PathBuf::from("/x/data/lab/vm images")],
        )
        .unwrap();
        assert_eq!(
            shares,
            "share0\t/x/agents/web-3/data/dev/src/web\t/x/data/dev/src/web\n\
             share1\t/x/agents/web-3/home\t/home/dev\n\
             share2\t/x/agents/web-3/data/lab/vm images\t/x/data/lab/vm images\n"
        );
        assert_eq!(
            mounts,
            "share0\t/x/data/dev/src/web\nshare1\t/home/dev\nshare2\t/x/data/lab/vm images\n"
        );
    }

    #[test]
    fn each_sandbox_gets_the_next_number() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(next_number(d.path()), Ok(1));
        assert_eq!(next_number(d.path()), Ok(2));
        assert_eq!(read(&d.path().join(".counter")).unwrap(), "2");
        fs::write(d.path().join(".counter"), "x\n").unwrap();
        assert!(next_number(d.path()).is_err());
    }
}
