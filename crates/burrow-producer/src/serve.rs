//! The producer's socket (Conary's `docs/m6-plugins.md`, "Only the flow may
//! ask" and "Restart and cancellation"). Only the flow's controller user
//! may ask, by the connection's peer credentials. A request's work lives as
//! long as its connection: the plugin sends its request and then nothing,
//! so the connection ending means the plugin is gone (killed, past its
//! deadline, cancelled), and its work is stopped and answers nothing.
use crate::wire::{self, Reply};
use conary_core::plugin::{Request, Response};
use rustix::net::sockopt::socket_peercred;
use std::io::{self, BufReader, Read};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::process::{Command, Output};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Instant;

/// The one account the flow runs its plugins as, which no authoring
/// session shares (B3), by its uid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Controller(pub u32);

impl Controller {
    /// The account named `name`, as /etc/passwd has it.
    pub fn named(name: &str) -> Result<Self, String> {
        let passwd =
            std::fs::read_to_string("/etc/passwd").map_err(|e| format!("/etc/passwd: {e}"))?;
        passwd
            .lines()
            .map(|l| l.split(':').collect::<Vec<_>>())
            .find(|f| f.first() == Some(&name))
            .and_then(|f| f.get(2)?.parse().ok())
            .map(Self)
            .ok_or_else(|| format!("no user {name:?}"))
    }
}

/// A request's work, as long as its connection lasts.
#[derive(Clone)]
pub struct Work {
    gone: Arc<AtomicBool>,
}

impl Work {
    fn new() -> Self {
        Self {
            gone: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Work no connection holds: finishing at startup what a crash left
    /// ([`crate::records`]). It's never abandoned.
    pub fn reconciling() -> Self {
        Self::new()
    }

    /// Whether its connection has ended: the work stops, and publishes
    /// nothing.
    pub fn abandoned(&self) -> bool {
        self.gone.load(Ordering::SeqCst)
    }

    /// Run `command` in a process group of its own, which is killed when
    /// it's past `until`, when it finishes, and as soon as the work is
    /// abandoned.
    pub fn run(
        &self,
        command: &mut Command,
        input: Option<&[u8]>,
        until: Instant,
        max: usize,
    ) -> Result<Output, String> {
        burrow_process::output_unless(command, input, until, max, &|| self.abandoned())
    }
}

/// What a producer does with one request: its answer, or why there's none.
pub trait Handler: Fn(Request, &Work) -> Result<Response, String> + Send + Sync {}
impl<T: Fn(Request, &Work) -> Result<Response, String> + Send + Sync> Handler for T {}

/// A producer's listening socket.
pub struct Server {
    listener: UnixListener,
    controller: Controller,
}

impl Server {
    /// Listen at `path`, in place of any socket a stopped producer left
    /// there, for `controller` only.
    pub fn bind(path: &Path, controller: Controller) -> io::Result<Self> {
        use std::os::unix::fs::FileTypeExt;
        match std::fs::symlink_metadata(path) {
            Ok(m) if m.file_type().is_socket() => std::fs::remove_file(path)?,
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("{} is there and isn't a socket", path.display()),
                ));
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        Ok(Self {
            listener: UnixListener::bind(path)?,
            controller,
        })
    }

    /// Serve every connection, each on its own thread, until accepting
    /// fails.
    pub fn serve(&self, handler: impl Handler + 'static) -> io::Result<()> {
        let handler = Arc::new(handler);
        for stream in self.listener.incoming() {
            let stream = stream?;
            let (handler, controller) = (handler.clone(), self.controller);
            thread::spawn(move || connection(stream, controller, &*handler));
        }
        Ok(())
    }

    /// Serve the next connection on this thread, to its end.
    pub fn once(&self, handler: &dyn Handler) -> io::Result<()> {
        let (stream, _) = self.listener.accept()?;
        connection(stream, self.controller, handler);
        Ok(())
    }
}

fn connection(stream: UnixStream, controller: Controller, handler: &dyn Handler) {
    let refuse = |why: String| {
        let _ = wire::send(&mut &stream, &Reply::Refused(why));
    };
    match socket_peercred(&stream) {
        Ok(cred) if cred.uid.as_raw() == controller.0 => {}
        Ok(cred) => {
            return refuse(format!(
                "only the flow's controller user may ask, and uid {} isn't it",
                cred.uid.as_raw()
            ));
        }
        Err(e) => return refuse(format!("its peer can't be told: {e}")),
    }
    let Ok(read) = stream.try_clone() else {
        return refuse("the connection can't be watched".into());
    };
    let mut read = BufReader::new(read);
    let request = match wire::line(&mut read) {
        Ok(Some(line)) => match serde_json::from_str::<Request>(&line) {
            Ok(request) => request,
            Err(e) => return refuse(format!("the request doesn't read: {e}")),
        },
        Ok(None) => return,
        Err(e) => return refuse(format!("the request can't be read: {e}")),
    };
    let work = Work::new();
    // The plugin sends nothing after its request: anything that ends this
    // read, its end or an error, is the plugin gone.
    let watched = work.clone();
    let watch = thread::spawn(move || {
        let mut byte = [0];
        while matches!(read.read(&mut byte), Ok(1)) {}
        watched.gone.store(true, Ordering::SeqCst);
    });
    let answer = handler(request, &work);
    if !work.abandoned() {
        let reply = match answer {
            Ok(response) => Reply::Answer(Box::new(response)),
            Err(why) => Reply::Refused(why),
        };
        let _ = wire::send(&mut &stream, &reply);
    }
    // Ends the watch's read whether or not the plugin is still there.
    let _ = stream.shutdown(std::net::Shutdown::Both);
    let _ = watch.join();
}
