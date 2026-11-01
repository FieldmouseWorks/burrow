//! A sandbox: its directory under the root (rpool/agents/ID), what it keeps
//! there, and the arguments its QEMU starts with.

use std::fs;
use std::path::{Path, PathBuf};

use crate::{Agent, Config, DAY, mtime, read};

/// What a sandbox's directory holds, by name: written by `agent new` and
/// kept up by the rest.
pub const PROJECT: &str = "project";
pub const CID: &str = "cid";
pub const NET: &str = "net";
pub const TTL_DAYS: &str = "ttl-days";
pub const LAST_USED: &str = "last-used";
pub const REMOVED: &str = "removed";
pub const SHARES: &str = "shares";
pub const MOUNTS: &str = "mounts";
pub const QEMU_ARGS: &str = "qemu-args";
pub const TOKEN: &str = "claude-token";

pub struct Sandbox {
    pub id: String,
    pub dir: PathBuf,
}

impl Sandbox {
    /// The sandbox called `id`: one whose directory has its project.
    pub fn find(cfg: &Config, id: &str) -> Result<Sandbox, String> {
        let plain = !id.is_empty() && id != "." && id != ".." && !id.contains('/');
        let dir = cfg.root.join(id);
        if !plain || !dir.join(PROJECT).is_file() {
            return Err(format!("no such sandbox: {id}"));
        }
        Ok(Sandbox {
            id: id.to_owned(),
            dir,
        })
    }

    /// Every sandbox, by id.
    pub fn all(cfg: &Config) -> Vec<Sandbox> {
        let Ok(entries) = fs::read_dir(&cfg.root) else {
            return Vec::new();
        };
        let mut ids: Vec<String> = entries
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|id| !id.starts_with('.'))
            .collect();
        ids.sort();
        ids.iter()
            .filter_map(|id| Sandbox::find(cfg, id).ok())
            .collect()
    }

    pub fn file(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    pub fn project(&self) -> Result<String, String> {
        read(&self.file(PROJECT))
    }

    pub fn cid(&self) -> Result<u32, String> {
        let cid = read(&self.file(CID))?;
        cid.parse()
            .map_err(|_| format!("{}: not a vsock address: {cid}", self.file(CID).display()))
    }

    pub fn open(&self) -> Result<bool, String> {
        Ok(read(&self.file(NET))? == "open")
    }

    pub fn removed(&self) -> bool {
        self.file(REMOVED).exists()
    }

    /// Its unit: microvm.nix's, one per VM.
    pub fn unit(&self) -> String {
        format!("microvm@{}.service", self.id)
    }

    /// Its copy of the project, as the host sees it.
    pub fn clone_path(&self, cfg: &Config) -> Result<PathBuf, String> {
        Ok(self.dir.join("data").join(cfg.rel()?).join(self.project()?))
    }

    /// When it's deleted, in seconds since the epoch: a day after an rm, or
    /// its days to live after it was last used.
    pub fn expires(&self) -> Result<i64, String> {
        if self.removed() {
            return Ok(mtime(&self.file(REMOVED))? as i64 + DAY as i64);
        }
        let ttl = read(&self.file(TTL_DAYS))?;
        let days: i64 = ttl.parse().map_err(|_| {
            format!(
                "{}: not a number of days: {ttl}",
                self.file(TTL_DAYS).display()
            )
        })?;
        Ok(mtime(&self.file(LAST_USED))? as i64 + days * DAY as i64)
    }

    /// The tags of its shares, in order.
    pub fn tags(&self) -> Result<Vec<String>, String> {
        let shares = read(&self.file(SHARES))?;
        Ok(shares
            .lines()
            .map(|l| l.split('\t').next().unwrap_or("").to_owned())
            .collect())
    }

    /// Writes its QEMU arguments, which the runner reads at every start, with
    /// Claude's login copied fresh, so a token the host has since dropped
    /// doesn't linger.
    pub fn write_args(&self, agent: &Agent) -> Result<(), String> {
        let cfg = agent.cfg;
        let token = self.file(TOKEN);
        match fs::remove_file(&token) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                return Err(format!("{}: {e}", token.display()));
            }
            _ => {}
        }
        if let Some(from) = &cfg.token_file
            && fs::metadata(from).is_ok_and(|m| m.len() > 0)
        {
            let (from, to) = (from.to_string_lossy(), token.to_string_lossy());
            agent.run(
                "install",
                &["-m", "0400", "-o", "microvm", "-g", "kvm", &from, &to],
            )?;
        }
        let has_token = fs::metadata(&token).is_ok_and(|m| m.len() > 0);
        let forward = if self.open()? {
            &cfg.forward_open
        } else {
            &cfg.forward_filtered
        };
        let args = qemu_args(&self.id, self.cid()?, forward, has_token, &self.tags()?);
        let at = self.file(QEMU_ARGS);
        fs::write(&at, args).map_err(|e| format!("{}: {e}", at.display()))
    }
}

/// The per-VM QEMU arguments: the network, the vsock address, the shares and
/// the credentials, on one line.
pub fn qemu_args(id: &str, cid: u32, forward: &Path, token: bool, tags: &[String]) -> String {
    let mut a = String::new();
    a += "-fw_cfg name=opt/io.systemd.credentials/agent.mounts,file=mounts ";
    a += &format!("-fw_cfg name=opt/io.systemd.credentials/agent.id,string={id} ");
    if token {
        a += "-fw_cfg name=opt/io.systemd.credentials/agent.claude-token,file=claude-token ";
    }
    a += &format!("-device vhost-vsock-pci,guest-cid={cid} ");
    // restrict=on: no route out and no way to the host, except the one
    // forwarded address that is the proxy.
    a += &format!(
        "-netdev user,id=agentnet,restrict=on,guestfwd=tcp:10.0.2.100:3128-cmd:{} ",
        forward.display()
    );
    let [b0, b1, b2, b3] = cid.to_be_bytes();
    a += &format!(
        "-device virtio-net-pci,netdev=agentnet,romfile=,mac=02:00:{b0:02x}:{b1:02x}:{b2:02x}:{b3:02x} "
    );
    for (i, tag) in tags.iter().enumerate() {
        a += &format!("-chardev socket,id=agentfs{i},path={tag}.sock ");
        a += &format!("-device vhost-user-fs-pci,chardev=agentfs{i},tag={tag} ");
    }
    a + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::new::tests::config;
    use std::time::{Duration, SystemTime};

    fn made(cfg: &Config, id: &str) -> PathBuf {
        let d = cfg.root.join(id);
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join(PROJECT), "web\n").unwrap();
        d
    }

    fn aged(path: &Path, secs: u64) {
        let f = fs::File::options().append(true).open(path).unwrap();
        f.set_modified(SystemTime::now() - Duration::from_secs(secs))
            .unwrap();
    }

    #[test]
    fn a_sandbox_is_a_directory_with_its_project() {
        let d = tempfile::tempdir().unwrap();
        let cfg = config(d.path());
        for id in ["web-2", "web-10", "a-1", "z-9", "m-4", "c-12", "k-7", "b-3"] {
            made(&cfg, id);
        }
        fs::create_dir_all(cfg.root.join("half")).unwrap();
        made(&cfg, ".hidden");
        fs::write(cfg.root.join(".counter"), "10\n").unwrap();
        let ids: Vec<String> = Sandbox::all(&cfg).into_iter().map(|s| s.id).collect();
        assert_eq!(
            ids,
            ["a-1", "b-3", "c-12", "k-7", "m-4", "web-10", "web-2", "z-9"]
        );
        assert_eq!(
            Sandbox::find(&cfg, "web-2").unwrap().dir,
            cfg.root.join("web-2")
        );
        assert_eq!(
            Sandbox::find(&cfg, "web-2").unwrap().project().unwrap(),
            "web"
        );
        // What the ids could reach outside the sandboxes, made to look
        // like sandboxes.
        made(&cfg, "../outside");
        made(&cfg, "..");
        made(&cfg, ".");
        for id in ["", ".", "..", "half", "gone", "web-2/", "../outside", "a/b"] {
            assert_eq!(
                Sandbox::find(&cfg, id).err(),
                Some(format!("no such sandbox: {id}")),
                "{id}"
            );
        }
    }

    #[test]
    fn it_expires_its_days_after_its_last_use_or_a_day_after_an_rm() {
        let d = tempfile::tempdir().unwrap();
        let cfg = config(d.path());
        let dir = made(&cfg, "web-1");
        fs::write(dir.join(TTL_DAYS), "3\n").unwrap();
        fs::write(dir.join(LAST_USED), "").unwrap();
        aged(&dir.join(LAST_USED), 3600);
        let sb = Sandbox::find(&cfg, "web-1").unwrap();
        let now = crate::now() as i64;
        let left = sb.expires().unwrap() - now;
        assert!(
            (3 * 86_400 - 3600 - 5..=3 * 86_400 - 3600).contains(&left),
            "{left}"
        );
        fs::write(dir.join(REMOVED), "").unwrap();
        aged(&dir.join(REMOVED), 7200);
        let left = sb.expires().unwrap() - now;
        assert!(
            (86_400 - 7200 - 5..=86_400 - 7200).contains(&left),
            "{left}"
        );
        fs::remove_file(dir.join(REMOVED)).unwrap();
        fs::write(dir.join(TTL_DAYS), "soon\n").unwrap();
        assert!(sb.expires().is_err());
    }

    #[test]
    fn its_args_are_written_fresh_without_a_token_the_host_dropped() {
        let d = tempfile::tempdir().unwrap();
        let cfg = config(d.path());
        let dir = made(&cfg, "web-1");
        fs::write(dir.join(CID), "1001\n").unwrap();
        fs::write(dir.join(NET), "open\n").unwrap();
        fs::write(dir.join(SHARES), "share0\t/a\t/b\nshare1\t/c\t/d\n").unwrap();
        fs::write(dir.join(TOKEN), "old\n").unwrap();
        let agent = Agent::new(&cfg, Path::new("/nonexistent.json")).unwrap();
        let sb = Sandbox::find(&cfg, "web-1").unwrap();
        sb.write_args(&agent).unwrap();
        assert!(!dir.join(TOKEN).exists());
        let args = fs::read_to_string(dir.join(QEMU_ARGS)).unwrap();
        assert_eq!(
            args,
            qemu_args(
                "web-1",
                1001,
                &cfg.forward_open,
                false,
                &["share0".into(), "share1".into()]
            )
        );
        fs::write(dir.join(NET), "filtered\n").unwrap();
        sb.write_args(&agent).unwrap();
        let args = fs::read_to_string(dir.join(QEMU_ARGS)).unwrap();
        assert!(args.contains(&format!("-cmd:{} ", cfg.forward_filtered.display())));
    }

    #[test]
    fn the_qemu_args_are_one_line() {
        let tags = ["share0".to_owned(), "share1".to_owned()];
        assert_eq!(
            qemu_args(
                "web-3",
                1003,
                Path::new("/nix/store/x-agent-proxy-3128"),
                false,
                &tags
            ),
            "-fw_cfg name=opt/io.systemd.credentials/agent.mounts,file=mounts \
             -fw_cfg name=opt/io.systemd.credentials/agent.id,string=web-3 \
             -device vhost-vsock-pci,guest-cid=1003 \
             -netdev user,id=agentnet,restrict=on,guestfwd=tcp:10.0.2.100:3128-cmd:/nix/store/x-agent-proxy-3128 \
             -device virtio-net-pci,netdev=agentnet,romfile=,mac=02:00:00:00:03:eb \
             -chardev socket,id=agentfs0,path=share0.sock \
             -device vhost-user-fs-pci,chardev=agentfs0,tag=share0 \
             -chardev socket,id=agentfs1,path=share1.sock \
             -device vhost-user-fs-pci,chardev=agentfs1,tag=share1 \n"
        );
        let with_token = qemu_args("a-1", 0x01020304, Path::new("/f"), true, &[]);
        assert!(with_token.contains(
            "string=a-1 -fw_cfg name=opt/io.systemd.credentials/agent.claude-token,file=claude-token -device"
        ));
        assert!(with_token.contains("mac=02:00:01:02:03:04 \n"));
    }
}
