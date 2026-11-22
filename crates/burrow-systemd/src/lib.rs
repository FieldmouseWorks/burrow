//! What units share: telling systemd about their own, and inspection of
//! the work inside them.
use std::ffi::OsStr;
use std::os::linux::net::SocketAddrExt as _;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::net::{SocketAddr, UnixDatagram};
use std::{fs, path::Path};

/// Tells systemd `state` (sd_notify(3)) at `socket`, the NOTIFY_SOCKET it
/// gave the unit: a path, or an abstract name after an @.
pub fn notify(socket: &OsStr, state: &str) -> std::io::Result<()> {
    let address = match socket.as_bytes().strip_prefix(b"@") {
        Some(name) => SocketAddr::from_abstract_name(name)?,
        None => SocketAddr::from_pathname(socket)?,
    };
    UnixDatagram::unbound()?.send_to_addr(state.as_bytes(), &address)?;
    Ok(())
}

/// Whether a Runner.Worker process is anywhere in this cgroup: an error
/// when the cgroup, or the name of a process in it, can't be read. A
/// process whose name is missing is one gone, unless the cgroup still has
/// it: then /proc hides it from us.
pub fn working(cg: &Path) -> std::io::Result<bool> {
    working_in(cg, Path::new("/proc"))
}

fn working_in(cg: &Path, proc: &Path) -> std::io::Result<bool> {
    let procs = |dir: &Path| -> std::io::Result<Vec<String>> {
        Ok(fs::read_to_string(dir.join("cgroup.procs"))?
            .lines()
            .map(|p| p.trim().to_owned())
            .collect())
    };
    let mut missing = Vec::new();
    for pid in procs(cg)? {
        match fs::read_to_string(proc.join(&pid).join("comm")) {
            Ok(comm) if comm.trim() == "Runner.Worker" => return Ok(true),
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => missing.push(pid),
            Err(e) => return Err(e),
        }
    }
    if let Some(pid) = procs(cg)?.into_iter().find(|p| missing.contains(p)) {
        return Err(std::io::Error::other(format!(
            "process {pid} is in {} but not in /proc",
            cg.display()
        )));
    }
    for e in fs::read_dir(cg)? {
        let e = e?;
        if e.file_type()?.is_dir() && working_in(&e.path(), proc)? {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_and_an_abstract_name_both_hear_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notify");
        let name = path.as_os_str().as_bytes();
        let by_name = [&b"@"[..], name].concat();
        for (socket, at) in [
            (UnixDatagram::bind(&path).unwrap(), path.as_os_str()),
            (
                UnixDatagram::bind_addr(&SocketAddr::from_abstract_name(name).unwrap()).unwrap(),
                OsStr::from_bytes(&by_name),
            ),
        ] {
            notify(at, "READY=1").unwrap();
            let mut heard = [0; 16];
            let n = socket.recv(&mut heard).unwrap();
            assert_eq!(&heard[..n], b"READY=1");
        }
    }
    #[test]
    fn a_listener_is_idle_but_a_worker_in_a_child_cgroup_is_busy() {
        let dir = tempfile::tempdir().unwrap();
        let cg = dir.path().join("cg");
        let proc = dir.path().join("proc");
        fs::create_dir_all(cg.join("child")).unwrap();
        fs::create_dir_all(proc.join("1")).unwrap();
        fs::create_dir_all(proc.join("2")).unwrap();
        fs::write(cg.join("cgroup.procs"), "1\n").unwrap();
        fs::write(cg.join("child/cgroup.procs"), "2\n").unwrap();
        fs::write(proc.join("1/comm"), "Runner.Listener\n").unwrap();
        fs::write(proc.join("2/comm"), "other\n").unwrap();
        assert!(!working_in(&cg, &proc).unwrap());
        fs::write(proc.join("2/comm"), "Runner.Worker\n").unwrap();
        assert!(working_in(&cg, &proc).unwrap());
        fs::remove_file(proc.join("2/comm")).unwrap();
        assert!(working_in(&cg, &proc).is_err());
    }
}
