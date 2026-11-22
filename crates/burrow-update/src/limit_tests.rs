//! The limit as systemd would keep it: a socket stands in for the unit's,
//! and the end moves as each message that comes to it says.

use std::fs;
use std::os::unix::net::UnixDatagram;
use std::time::{Duration, Instant};

use burrow_github::fake::Fake;

use super::*;
use crate::testing::{Machine, bin, lock, sha};

/// A unit's notify socket and each ask that came to it, with when.
struct Unit {
    began: Instant,
    heard: thread::JoinHandle<Vec<(Instant, Duration)>>,
    stop: mpsc::Sender<()>,
}

impl Unit {
    fn start(m: &mut Machine) -> Unit {
        let path = m.path("notify");
        let socket = UnixDatagram::bind(&path).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_millis(20)))
            .unwrap();
        m.cfg.notify = Some(path.into());
        let (stop, stopped) = mpsc::channel();
        let began = Instant::now();
        let heard = thread::spawn(move || {
            let (mut heard, mut buf) = (Vec::new(), [0u8; 128]);
            loop {
                match socket.recv(&mut buf) {
                    Ok(n) => {
                        let said = std::str::from_utf8(&buf[..n]).unwrap();
                        let left = said.strip_prefix("EXTEND_TIMEOUT_USEC=").expect(said);
                        heard.push((Instant::now(), Duration::from_micros(left.parse().unwrap())));
                    }
                    Err(_) if stopped.try_recv().is_ok() => return heard,
                    Err(_) => {}
                }
            }
        });
        Unit { began, heard, stop }
    }

    /// When systemd would end a run with `limit`, from its start: each ask
    /// counts only if it came before the end as it then stood.
    fn ends(self, limit: Duration) -> Duration {
        self.stop.send(()).unwrap();
        let mut end = self.began + limit;
        for (at, left) in self.heard.join().unwrap() {
            if at > end {
                break;
            }
            end = end.max(at + left);
        }
        end - self.began
    }
}

#[test]
fn an_update_that_waits_longer_than_its_work_limit_isnt_killed_for_it() {
    let mut m = Machine::running_main(&[("nixpkgs", &sha('a')), ("burrow", &sha('b'))]);
    m.cfg.heavy = Some(bin().join("heavy"));
    m.cfg.work_limit = 4;
    let limit = Duration::from_secs(m.cfg.work_limit);
    m.landed(
        "flake.lock",
        &lock(&[("nixpkgs", &sha('c')), ("burrow", &sha('b'))]),
    );
    fs::write(m.home("in-slot"), "sleep 6").unwrap();
    let unit = Unit::start(&mut m);
    let began = Instant::now();
    assert_eq!(crate::run::once(&m.cfg, &mut Fake::default(), false), 0);
    let took = began.elapsed();
    let ends = unit.ends(limit);
    assert!(took > limit, "it waited past its limit: {took:?}");
    assert!(ends > took, "killed at {ends:?}, and it took {took:?}");
    // All of the wait came back.
    assert!(ends >= limit + Duration::from_millis(5900), "{ends:?}");
}

#[test]
fn each_wait_comes_back_and_the_work_between_them_doesnt() {
    let mut m = Machine::new();
    m.cfg.work_limit = 2;
    let unit = Unit::start(&mut m);
    let limit = Limit::begin(&m.cfg);
    let slot_after = |wait: u64| {
        let (said, mut says) = std::io::pipe().unwrap();
        let heavy = thread::spawn(move || {
            thread::sleep(Duration::from_millis(wait));
            std::io::Write::write_all(&mut says, format!("{IN_SLOT}\n").as_bytes()).unwrap();
        });
        limit.wait(said);
        heavy.join().unwrap();
    };
    slot_after(1500);
    thread::sleep(Duration::from_millis(700));
    slot_after(1500);
    let ends = unit.ends(Duration::from_secs(2));
    // 2s to work in and 3s waited, however much of the 2s was worked.
    assert!(
        ends >= Duration::from_millis(4900) && ends < Duration::from_millis(5500),
        "{ends:?}"
    );
}

#[test]
fn a_heavy_that_ends_without_a_slot_ends_the_wait() {
    let mut m = Machine::new();
    m.cfg.work_limit = 600;
    let limit = Limit::begin(&m.cfg);
    let began = Instant::now();
    limit.wait(&b"not a slot\n"[..]);
    assert!(began.elapsed() < Duration::from_secs(10));
}
