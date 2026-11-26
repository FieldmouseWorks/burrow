//! The VM's side (`burrow-agent-guest`): starts the plugin and carries its
//! stdio over the virtio port. It's for the plugin's convenience and guards
//! nothing. Whoever is root in there can do all it does.

use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read, Seek, Write};
use std::net::UdpSocket;
use std::os::unix::fs::chown;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use super::{Frame, MOST_TEXT, PORT, SERVICE_IP, SERVICE_PORT, Spec, receive, send};

const LOOK: Duration = Duration::from_millis(50);

/// What the image says: who the plugin runs as, what it runs, and the
/// variable its engine's login becomes.
pub struct Image {
    pub program: PathBuf,
    pub uid: u32,
    pub gid: u32,
    pub credential_env: Option<String>,
}

/// Waits for what `look` finds, or for it to say it never will. No clock
/// of its own: a loaded machine boots a VM ten times slower, and the call's
/// deadline already bounds the wait, as the host ends the VM with the call.
fn until<T>(mut look: impl FnMut() -> Result<Option<T>, String>) -> Result<T, String> {
    loop {
        if let Some(found) = look()? {
            return Ok(found);
        }
        thread::sleep(LOOK);
    }
}

pub fn plugin(image: &Image) -> Result<(), String> {
    let at = Path::new("/dev/virtio-ports").join(PORT);
    let port = until(|| Ok(File::options().read(true).write(true).open(&at).ok()))?;
    let mut to = port.try_clone().map_err(|e| e.to_string())?;
    send(&mut to, &Frame::Ready)?;
    let spec: Spec = receive(&mut BufReader::new(port))?.ok_or("the host sent nothing")?;
    let to = Arc::new(Mutex::new(to));
    let last = match run(image, &spec, &to) {
        Ok(answer) => Frame::Answer(answer),
        Err(why) => Frame::Failed(why),
    };
    // The host kills the VM on the answer: what's written is down first.
    rustix::fs::sync();
    let mut to = to.lock().map_err(|_| "the port's lock")?;
    send(&mut *to, &last)
}

fn prepare_guest(spec: &Spec) -> Result<(PathBuf, Vec<Child>), String> {
    let io = |e: io::Error| e.to_string();
    let mut cwd = PathBuf::from(spec.env.get("HOME").map_or("/", String::as_str));
    if let Some(workspace) = &spec.workspace {
        fs::create_dir_all(&workspace.mount).map_err(io)?;
        let mounted = Command::new("mount")
            .args(["-t", "virtiofs", "workspace"])
            .arg(&workspace.mount)
            .status()
            .map_err(io)?;
        if !mounted.success() {
            return Err("the workspace didn't mount".to_owned());
        }
        cwd = workspace.workdir.clone();
    }
    if spec.nic {
        // A route is all it needs: connect sends nothing.
        until(|| {
            Ok(UdpSocket::bind("0.0.0.0:0")
                .ok()
                .and_then(|socket| socket.connect((SERVICE_IP, SERVICE_PORT)).ok()))
        })?;
    }
    let mut bridges: Vec<Child> = Vec::new();
    for (socket, port) in spec.sockets.iter().zip(SERVICE_PORT..) {
        if let Some(dir) = socket.parent() {
            fs::create_dir_all(dir).map_err(io)?;
        }
        let mut bridge = Command::new("socat")
            .args(["-t", "300"])
            .arg(format!("UNIX-LISTEN:{},fork,mode=666", socket.display()))
            .arg(format!("TCP:{SERVICE_IP}:{port}"))
            .stdin(Stdio::null())
            .spawn()
            .map_err(io)?;
        let listened = until(|| {
            if socket.exists() {
                return Ok(Some(()));
            }
            match bridge.try_wait() {
                Ok(None) => Ok(None),
                Ok(Some(status)) => {
                    Err(format!("{}: its bridge ended ({status})", socket.display()))
                }
                Err(e) => Err(e.to_string()),
            }
        });
        bridges.push(bridge);
        listened?;
    }
    Ok((cwd, bridges))
}

fn run(image: &Image, spec: &Spec, to: &Arc<Mutex<File>>) -> Result<String, String> {
    let io = |e: io::Error| e.to_string();
    let (cwd, mut bridges) = prepare_guest(spec)?;

    // Files, as Conary's runner gives a plugin: a child that never reads,
    // or one it leaves behind holding them, can't hang the run.
    let mut input = tempfile::tempfile().map_err(io)?;
    input.write_all(spec.request.as_bytes()).map_err(io)?;
    input.rewind().map_err(io)?;
    let mut output = tempfile::tempfile().map_err(io)?;
    let mut command = Command::new(&image.program);
    command
        .env_clear()
        .envs(&spec.env)
        .current_dir(cwd)
        .uid(image.uid)
        .gid(image.gid)
        .stdin(input)
        .stdout(output.try_clone().map_err(io)?)
        .stderr(Stdio::piped());
    if let Some(home) = spec.env.get("HOME") {
        let _ = chown(home, Some(image.uid), Some(image.gid));
    }
    if let Some(name) = &image.credential_env
        && let Some(dir) = std::env::var_os("CREDENTIALS_DIRECTORY")
        && let Ok(login) = fs::read_to_string(Path::new(&dir).join("agent.credential"))
    {
        command.env(name, login.trim_end_matches('\n'));
    }
    let mut child = command
        .spawn()
        .map_err(|e| format!("{}: {e}", image.program.display()))?;
    let logged = child.stderr.take().map(|from| log(from, to.clone()));
    let status = loop {
        if output.metadata().map_err(io)?.len() > MOST_TEXT {
            let _ = child.kill();
            let _ = child.wait();
            return Err("the plugin's answer is too large".to_owned());
        }
        if let Some(status) = child.try_wait().map_err(io)? {
            break status;
        }
        thread::sleep(LOOK);
    };
    if let Some(logged) = logged {
        // What it printed last, unless something it left holds the pipe.
        let end = Instant::now() + Duration::from_secs(1);
        while !logged.is_finished() && Instant::now() < end {
            thread::sleep(LOOK);
        }
    }
    for bridge in &mut bridges {
        let _ = bridge.kill();
        let _ = bridge.wait();
    }
    if !status.success() {
        return Err(format!("the plugin failed ({status})"));
    }
    output.rewind().map_err(io)?;
    let mut answer = String::new();
    output
        .take(MOST_TEXT + 1)
        .read_to_string(&mut answer)
        .map_err(io)?;
    if answer.len() as u64 > MOST_TEXT {
        return Err("the plugin's answer is too large".to_owned());
    }
    Ok(answer)
}

fn log(from: impl Read + Send + 'static, to: Arc<Mutex<File>>) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut from = BufReader::new(from);
        loop {
            let mut line = Vec::new();
            match (&mut from).take(65536).read_until(b'\n', &mut line) {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            }
            let Ok(mut to) = to.lock() else { return };
            let text = String::from_utf8_lossy(&line).into_owned();
            if send(&mut *to, &Frame::Log(text)).is_err() {
                return;
            }
        }
    })
}

/// `heavy -- COMMAND...` in a plugin's VM: the host takes the slot and
/// holds it while the connection is open.
pub fn heavy(args: &[OsString]) -> Result<u8, String> {
    let command = match args {
        [dashes, rest @ ..] if dashes == "--" => rest,
        _ => args,
    };
    let (program, args) = command.split_first().ok_or("usage: heavy -- COMMAND...")?;
    let at = std::env::var_os("HEAVY_SOCKET").ok_or("heavy isn't granted to this plugin")?;
    let slot =
        UnixStream::connect(&at).map_err(|e| format!("{}: {e}", Path::new(&at).display()))?;
    let mut share = String::new();
    BufReader::new(&slot)
        .read_line(&mut share)
        .map_err(|e| e.to_string())?;
    let share = share.trim();
    if share.is_empty() {
        return Err("the host gave no slot".to_owned());
    }
    let status = Command::new(program)
        .args(args)
        .env("HEAVY_SLOT", "host")
        .env("HEAVY_JOBS", share)
        .env("CARGO_BUILD_JOBS", share)
        .env("RUST_TEST_THREADS", share)
        .status()
        .map_err(|e| format!("{}: {e}", Path::new(program).display()))?;
    drop(slot);
    Ok(crate::exit_code(status).clamp(0, 255) as u8)
}
