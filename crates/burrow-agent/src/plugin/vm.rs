//! One VM for one run of the plugin. What isn't in its [`Attach`] has no
//! device, share, forward or file here, which is the whole of the guard.

use std::collections::BTreeSet;
use std::fs;
use std::io::BufReader;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use super::render::{Attach, Does};
use super::{Frame, PORT, SERVICE_IP, SERVICE_PORT, Spec, receive, send};
use crate::ssh::BOOT;
use crate::{Agent, Engine, Plugin, as_account, stderr};

/// Connections one service has open at once, at most.
const CONNECTIONS: usize = 16;
/// How long a virtiofsd may take to listen.
const LISTEN: Duration = Duration::from_secs(30);

/// Processes that go when the run does, however it ends.
struct Running(Vec<Child>);

impl Drop for Running {
    fn drop(&mut self) {
        for child in self.0.iter_mut().rev() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// One VM's inputs.
pub(crate) struct Vm<'a> {
    pub agent: &'a Agent<'a>,
    pub engine: &'a Engine,
    pub plugin: &'a Plugin,
    /// Its own directory, under the call's.
    pub dir: PathBuf,
    /// The clone it shares, when it has a workspace.
    pub clone: Option<PathBuf>,
    pub attach: &'a Attach,
}

impl Vm<'_> {
    /// Boots it, runs the request, and kills it: the plugin's answer, its
    /// log handed to `log` as it comes.
    pub fn run(&self, request: String, log: &mut dyn FnMut(String)) -> Result<String, String> {
        let (dir, a) = (&self.dir, self.attach);
        self.directory()?;
        let mut running = Running(Vec::new());
        running.0.push(self.store()?);
        if let Some(clone) = &self.clone {
            running.0.push(self.files(clone)?);
        }
        let rpc = self.listen("rpc.sock")?;
        for (i, service) in a.services.iter().enumerate() {
            let listener = self.listen(&format!("svc-{i}.sock"))?;
            // QEMU can be handed one word for a forward: the script's name
            // is which service it is.
            let script = dir.join(format!("svc-{i}"));
            symlink(&self.agent.cfg.plugins.forward, &script)
                .map_err(|e| format!("{}: {e}", script.display()))?;
            host_service(self.agent, &service.does, listener)?;
        }
        if let Some(from) = &a.credential {
            let to = dir.join("credential");
            let (from, to) = (from.to_string_lossy(), to.to_string_lossy());
            self.agent.run(
                "install",
                &["-m", "0400", "-o", "microvm", "-g", "kvm", &from, &to],
            )?;
        }
        let args = qemu_args(dir, a, &self.engine.forward);
        fs::write(dir.join("qemu-args"), args).map_err(|e| format!("{}: {e}", dir.display()))?;
        let mut waited = vec![dir.join("store/ro-store.sock")];
        waited.extend(
            self.clone
                .as_ref()
                .map(|_| dir.join("files/workspace.sock")),
        );
        listening(&waited, &mut running)?;
        if self.clone.is_some() {
            self.open_share()?;
        }

        let mut qemu = as_account(self.agent, "microvm", "kvm");
        qemu.arg(self.plugin.runner.join("bin/microvm-run"))
            .current_dir(dir)
            .stdin(Stdio::null())
            .stdout(stderr()?)
            .stderr(stderr()?);
        running
            .0
            .push(qemu.spawn().map_err(|e| format!("the VM: {e}"))?);
        let until = Instant::now() + BOOT;
        let mut host = connected(&rpc, &mut running, until)?;
        host.set_read_timeout(Some(
            until
                .saturating_duration_since(Instant::now())
                .max(Duration::from_secs(1)),
        ))
        .map_err(|e| e.to_string())?;
        let mut from = BufReader::new(host.try_clone().map_err(|e| e.to_string())?);
        match receive(&mut from) {
            Ok(Some(Frame::Ready)) => {}
            _ => return Err("the plugin's VM didn't come up".to_owned()),
        }
        host.set_read_timeout(None).map_err(|e| e.to_string())?;
        let spec = Spec {
            request,
            env: a.env.clone(),
            workspace: a.workspace.clone(),
            sockets: a.services.iter().map(|s| s.socket.clone()).collect(),
            nic: a.nic(),
        };
        send(&mut host, &spec)?;
        // The guest is the plugin's: a word of it is the plugin's word, and
        // decides nothing here.
        loop {
            match receive(&mut from)? {
                Some(Frame::Ready) => {}
                Some(Frame::Log(text)) => log(text),
                Some(Frame::Answer(text)) => return Ok(text),
                Some(Frame::Failed(why)) => return Err(why),
                None => return Err("the plugin's VM ended without an answer".to_owned()),
            }
        }
    }

    /// Root's, which QEMU works in and nobody else lists. Each virtiofsd
    /// gets a folder of its own for its socket.
    fn directory(&self) -> Result<(), String> {
        let user = &self.agent.cfg.user;
        for (dir, owner, mode) in [
            (self.dir.clone(), "root", 0o771),
            (self.dir.join("store"), "microvm", 0o2750),
            (self.dir.join("files"), user.as_str(), 0o2750),
        ] {
            fs::create_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            let at = dir.to_string_lossy().into_owned();
            self.agent.run("chown", &[&format!("{owner}:kvm"), &at])?;
            fs::set_permissions(&dir, fs::Permissions::from_mode(mode))
                .map_err(|e| format!("{at}: {e}"))?;
        }
        Ok(())
    }

    fn listen(&self, name: &str) -> Result<UnixListener, String> {
        let at = self.dir.join(name);
        let listener = UnixListener::bind(&at).map_err(|e| format!("{}: {e}", at.display()))?;
        let path = at.to_string_lossy().into_owned();
        self.agent.run("chown", &["root:kvm", &path])?;
        fs::set_permissions(&at, fs::Permissions::from_mode(0o660))
            .map_err(|e| format!("{path}: {e}"))?;
        Ok(listener)
    }

    /// A virtiofsd with nothing to see but itself and what it shares. It
    /// has no privilege, so the confinement is bubblewrap's.
    fn virtiofsd(&self, account: (&str, &str), socket_dir: &Path) -> Result<Command, String> {
        let host = &self.agent.cfg.plugins;
        let mut c = as_account(self.agent, account.0, account.1);
        c.args([
            "bwrap",
            "--unshare-all",
            "--die-with-parent",
            "--proc",
            "/proc",
            "--dev",
            "/dev",
        ]);
        for path in paths(&host.virtiofsd.closure)? {
            c.arg("--ro-bind").arg(&path).arg(&path);
        }
        c.arg("--bind").arg(socket_dir).arg("/socket");
        Ok(c)
    }

    /// Starts it on `share`, seen by it at that path, with the translation
    /// of owners that share needs.
    fn serving(
        &self,
        c: &mut Command,
        socket: &str,
        share: &str,
        owners: &[String],
    ) -> Result<Child, String> {
        c.arg("--")
            .arg(self.agent.cfg.plugins.virtiofsd.root.join("bin/virtiofsd"))
            .args([
                "--sandbox=none",
                "--inode-file-handles=never",
                "--no-announce-submounts",
                "--cache=auto",
            ])
            .arg(format!("--socket-path=/socket/{socket}"))
            .arg(format!("--shared-dir={share}"))
            .args(owners)
            .stdin(Stdio::null())
            .stdout(stderr()?)
            .stderr(stderr()?)
            .spawn()
            .map_err(|e| format!("virtiofsd for {share}: {e}"))
    }

    /// The VM's /nix/store: its guest's closure and its granted tools', and
    /// no other path of the host's store.
    fn store(&self) -> Result<Child, String> {
        let sockets = self.dir.join("store");
        let mut c = self.virtiofsd(("microvm", "kvm"), &sockets)?;
        c.args(["--dir", "/store"]);
        let mut seen = BTreeSet::new();
        seen.extend(paths(&self.plugin.closure)?);
        for closure in &self.attach.closures {
            seen.extend(paths(closure)?);
        }
        for path in seen {
            let name = path
                .file_name()
                .ok_or_else(|| format!("{}: not a store path", path.display()))?;
            let at = Path::new("/store").join(name);
            // bwrap would bind what a link points to in the link's place.
            match fs::read_link(&path) {
                Ok(target) => c.arg("--symlink").arg(target).arg(at),
                Err(_) => c.arg("--ro-bind").arg(&path).arg(at),
            };
        }
        // The store's owner is root, whom its user namespace can't map.
        let owners = [
            "--readonly",
            "--translate-uid=host:65534:0:1",
            "--translate-gid=host:65534:0:1",
        ]
        .map(str::to_owned);
        let child = self.serving(&mut c, "ro-store.sock", "/store", &owners)?;
        symlink("store/ro-store.sock", self.dir.join("ro-store.sock"))
            .map_err(|e| format!("the store's socket: {e}"))?;
        Ok(child)
    }

    /// Lets QEMU, by its group, connect to the workspace's virtiofsd, whose
    /// socket is its account's alone as made (`--socket-group` takes a group
    /// its namespace hasn't). As that account: root would follow a link put
    /// there instead.
    fn open_share(&self) -> Result<(), String> {
        let cfg = self.agent.cfg;
        let status = as_account(self.agent, &cfg.user, &cfg.group)
            .args(["chmod", "0660"])
            .arg(self.dir.join("files/workspace.sock"))
            .stdin(Stdio::null())
            .stdout(stderr()?)
            .status()
            .map_err(|e| format!("chmod: {e}"))?;
        if status.success() {
            Ok(())
        } else {
            Err("the workspace's socket couldn't be opened to QEMU".to_owned())
        }
    }

    /// The workspace's clone, its repository's own directory covered over
    /// unless git-metadata is granted.
    fn files(&self, clone: &Path) -> Result<Child, String> {
        let cfg = self.agent.cfg;
        let sockets = self.dir.join("files");
        let mut c = self.virtiofsd((&cfg.user, &cfg.group), &sockets)?;
        c.arg("--bind").arg(clone).arg("/workspace");
        if !self.attach.git_metadata {
            c.args(covered(clone)?);
        }
        // As in every sandbox, what any of its users writes is the user's.
        let owners = [
            format!("--translate-uid=squash-guest:0:{}:4294967295", cfg.uid),
            format!("--translate-gid=squash-guest:0:{}:4294967295", cfg.gid),
        ];
        self.serving(&mut c, "workspace.sock", "/workspace", &owners)
    }
}

/// bwrap's words that cover a workspace's .git. The guest can't move or
/// remove what's a mount outside it, and the host's copy stays as it was.
fn covered(clone: &Path) -> Result<Vec<String>, String> {
    let git = clone.join(".git");
    let kind = fs::symlink_metadata(&git).map_err(|e| format!("{}: {e}", git.display()))?;
    if !kind.is_dir() {
        return Err(format!("{}: not a directory", git.display()));
    }
    let at = "/workspace/.git";
    Ok(["--tmpfs", at, "--remount-ro", at]
        .map(str::to_owned)
        .to_vec())
}

/// The store paths a closure's list names.
fn paths(list: &Path) -> Result<Vec<PathBuf>, String> {
    let text = fs::read_to_string(list).map_err(|e| format!("{}: {e}", list.display()))?;
    Ok(text
        .lines()
        .filter(|l| !l.is_empty())
        .map(PathBuf::from)
        .collect())
}

fn ended(running: &mut Running) -> Result<(), String> {
    for child in &mut running.0 {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            return Err(format!("the plugin's VM or a share of it ended ({status})"));
        }
    }
    Ok(())
}

fn listening(sockets: &[PathBuf], running: &mut Running) -> Result<(), String> {
    let until = Instant::now() + LISTEN;
    while !sockets.iter().all(|s| s.exists()) {
        ended(running)?;
        if Instant::now() >= until {
            return Err("a share of the plugin's VM never listened".to_owned());
        }
        thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}

fn connected(
    rpc: &UnixListener,
    running: &mut Running,
    until: Instant,
) -> Result<UnixStream, String> {
    rpc.set_nonblocking(true).map_err(|e| e.to_string())?;
    loop {
        match rpc.accept() {
            Ok((stream, _)) => {
                stream.set_nonblocking(false).map_err(|e| e.to_string())?;
                return Ok(stream);
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => return Err(e.to_string()),
        }
        ended(running)?;
        if Instant::now() >= until {
            return Err("the plugin's VM didn't start".to_owned());
        }
        thread::sleep(Duration::from_millis(20));
    }
}

/// Serves one service's socket: a process of the user's for each
/// connection, which is all of the host that connection reaches. They end
/// with the call, whose service's cgroup they're in.
fn host_service(agent: &Agent, does: &Does, listener: UnixListener) -> Result<(), String> {
    let cfg = agent.cfg;
    let (user, group, home) = (cfg.user.clone(), cfg.group.clone(), cfg.home.clone());
    let (path, does) = (agent.path.clone(), does.clone());
    let agent_itself = [agent.exe.clone(), agent.config.clone()];
    let open = Arc::new(AtomicUsize::new(0));
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { return };
            if open.load(Ordering::SeqCst) >= CONNECTIONS {
                continue;
            }
            let mut c = crate::account_command(&path, &user, &group);
            c.env("HOME", &home).env("USER", &user);
            match &does {
                Does::Relay(to) => {
                    let (Some(dir), Some(name)) = (to.parent(), to.file_name()) else {
                        return;
                    };
                    // From its directory: a socket's address is short.
                    c.current_dir(dir)
                        .args(["socat", "-t", "300", "FD:0"])
                        .arg(format!("UNIX-CONNECT:{}", name.to_string_lossy()));
                }
                Does::Jobs(to) => {
                    let (Some(dir), Some(name)) = (to.parent(), to.file_name()) else {
                        return;
                    };
                    c.current_dir(dir)
                        .args(&agent_itself)
                        .arg("_jobs")
                        .arg(name);
                }
                Does::Heavy(heavy) => {
                    c.arg(heavy).arg("--").args(&agent_itself).arg("_hold");
                }
                Does::Server { command, args, env } => {
                    c.current_dir("/").arg(command).args(args).envs(env);
                }
            }
            let Ok(output) = stream.try_clone() else {
                continue;
            };
            c.stdin(Stdio::from(std::os::fd::OwnedFd::from(stream)))
                .stdout(Stdio::from(std::os::fd::OwnedFd::from(output)));
            match stderr() {
                Ok(to) => c.stderr(to),
                Err(_) => c.stderr(Stdio::null()),
            };
            let Ok(mut child) = c.spawn() else { continue };
            // The command holds the connection's descriptors until dropped.
            drop(c);
            if matches!(does, Does::Heavy(_)) {
                // A run holds one slot at a time; the next waits its turn.
                let _ = child.wait();
                continue;
            }
            open.fetch_add(1, Ordering::SeqCst);
            let open = open.clone();
            thread::spawn(move || {
                let _ = child.wait();
                open.fetch_sub(1, Ordering::SeqCst);
            });
        }
    });
    Ok(())
}

/// The per-VM QEMU arguments, on one line. A plugin's VM has no vsock (the
/// host listens there) and, without a service or the network, no NIC.
pub(crate) fn qemu_args(dir: &Path, a: &Attach, proxy: &Path) -> String {
    let mut s = String::new();
    s += "-device virtio-serial-pci -chardev socket,id=agentrpc,path=rpc.sock ";
    s += &format!("-device virtserialport,chardev=agentrpc,name={PORT} ");
    if a.credential.is_some() {
        s += "-fw_cfg name=opt/io.systemd.credentials/agent.credential,file=credential ";
    }
    if cfg!(target_arch = "x86_64") && !a.kvm {
        // The image's -cpu host carries nested virtualization; the last
        // -cpu is the one QEMU takes.
        s += "-cpu host,+x2apic,-sgx,-vmx,-svm ";
    }
    if a.workspace.is_some() {
        s += "-chardev socket,id=agentfs,path=files/workspace.sock ";
        s += "-device vhost-user-fs-pci,chardev=agentfs,tag=workspace ";
    }
    if a.nic() {
        s += "-netdev user,id=agentnet,restrict=on,ipv6=off";
        if a.network {
            s += &format!(",guestfwd=tcp:10.0.2.100:3128-cmd:{}", proxy.display());
        }
        for i in 0..a.services.len() {
            s += &format!(
                ",guestfwd=tcp:{SERVICE_IP}:{}-cmd:{}",
                usize::from(SERVICE_PORT) + i,
                dir.join(format!("svc-{i}")).display()
            );
        }
        s += " -device virtio-net-pci,netdev=agentnet,romfile=,mac=02:00:00:00:00:02 ";
    }
    s + "\n"
}

#[cfg(test)]
mod tests;
