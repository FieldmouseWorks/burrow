//! The Rust claude-rc-scan against the shell it replaced (tests/old/old-scan.sh,
//! as modules/remote-control.nix had it inline), on a fake machine that takes
//! random steps: folders made, made into checkouts, removed; sessions busy,
//! recent, long quiet, written, or gone; servers current or stale, with live
//! and ended sessions. Each step runs both on the same machine, a scan and the
//! look after it, and what they ask systemd for, and the wake they note, must
//! agree. systemctl, systemd-run and systemd-escape are stand-ins that log
//! what they are asked.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::symlink;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use burrow_rc_scan::machine;

const TMUX: &str = "/fake/bin/tmux";
const SERVE: &str = "/fake/current/bin/claude-rc-serve";
const IDLE: i64 = 30;
const MINUTE: i64 = 60_000;

/// The same sequence from each seed, whatever runs it.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }
    fn below(&mut self, n: usize) -> usize {
        self.next() as usize % n
    }
    fn pick<T: Copy>(&mut self, of: &[T]) -> T {
        of[self.below(of.len())]
    }
}

struct Procs(Vec<Child>);

impl Drop for Procs {
    fn drop(&mut self) {
        for c in &mut self.0 {
            // Its whole group: the server, its Claude and the sessions.
            let _ = Command::new("sh")
                .args(["-c", &format!("kill -KILL -- -{}", c.id())])
                .status();
            let _ = c.wait();
        }
    }
}

struct Machine {
    root: tempfile::TempDir,
    rng: Rng,
    procs: Procs,
    /// The live sessions below each server, to write their files for.
    sessions: BTreeMap<String, Vec<u32>>,
}

impl Machine {
    fn path(&self, rel: &str) -> PathBuf {
        self.root.path().join(rel)
    }
    fn projects(&self) -> PathBuf {
        self.path("projects")
    }
    /// The machine's flake, spelled with a `..`, as realpath -m reads it.
    fn flake(&self) -> PathBuf {
        self.path("projects/../projects/flk")
    }
    fn config(&self) -> PathBuf {
        let bin = self.path("bin");
        let config = self.path("config.json");
        let json = serde_json::json!({
            "projectsDir": self.projects(),
            "flake": self.flake(),
            "idleMinutes": IDLE,
            "tmux": TMUX,
            "serve": SERVE,
            "tools": {
                "systemctl": bin.join("systemctl"),
                "systemdRun": bin.join("systemd-run"),
                "systemdEscape": bin.join("systemd-escape"),
                "git": which("git"),
                "jq": which("jq"),
            },
        });
        fs::write(&config, json.to_string()).unwrap();
        config
    }
}

fn which(tool: &str) -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|d| d.join(tool))
        .find(|p| p.is_file())
        .unwrap_or_else(|| panic!("{tool} isn't on PATH"))
}

fn stand_in(dir: &Path, name: &str, body: &str) {
    burrow_test::executable(
        dir.join(name),
        format!("#!{}\n{body}", which("bash").display()),
    );
}

fn setup(seed: u64) -> Machine {
    let m = Machine {
        root: tempfile::tempdir().unwrap(),
        rng: Rng(seed),
        procs: Procs(Vec::new()),
        sessions: BTreeMap::new(),
    };
    for d in [
        "bin",
        "projects",
        "home/.claude/sessions",
        "fake/pid",
        "run/old",
        "run/new",
    ] {
        fs::create_dir_all(m.path(d)).unwrap();
    }
    let bin = m.path("bin");
    stand_in(
        &bin,
        "systemctl",
        r#"echo "systemctl $*" >>"$LOG"
shift
case $1 in
  show) cat "$FAKE/pid/${@: -1}" 2>/dev/null || echo 0 ;;
  list-units)
    glob=${@: -1}
    while read -r u; do [[ $u == $glob ]] && echo "$u loaded active running x"; done <"$FAKE/units"
    exit 0 ;;
esac
"#,
    );
    stand_in(&bin, "systemd-run", "echo \"systemd-run $*\" >>\"$LOG\"\n");
    stand_in(&bin, "systemd-escape", "echo \"${2/@./@$3.}\"\n");
    m
}

const KINDS: [&str; 7] = [
    "plain", "nogit", "origin", "worktree", "hidden", "link", "origin",
];

fn make(m: &Machine, name: &str, kind: &str) {
    let dir = m.projects().join(name);
    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_file(&dir);
    match kind {
        "plain" => fs::create_dir_all(&dir).unwrap(),
        "nogit" | "origin" => {
            fs::create_dir_all(dir.join(".git")).unwrap();
            let url = if kind == "origin" {
                "[remote \"origin\"]\n\turl = git@example.invalid:x/y.git\n"
            } else {
                ""
            };
            fs::write(
                dir.join(".git/config"),
                format!("[core]\n\tbare = false\n{url}"),
            )
            .unwrap();
        }
        "worktree" => {
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join(".git"), "gitdir: /nowhere\n").unwrap();
        }
        "hidden" => fs::create_dir_all(m.projects().join(format!(".{name}"))).unwrap(),
        "link" => {
            let target = m.path(&format!("elsewhere-{name}"));
            fs::create_dir_all(target.join(".git")).unwrap();
            fs::write(
                target.join(".git/config"),
                "[remote \"origin\"]\n\turl = x\n",
            )
            .unwrap();
            symlink(&target, &dir).unwrap();
        }
        other => panic!("{other}"),
    }
}

/// A server for a unit: its pid, with a Claude below it and the sessions below
/// that, `live` of them working and `ended` one that has ended and isn't waited
/// for, a zombie.
fn spawn(m: &mut Machine, name: &str, current: bool, live: usize, ended: bool) {
    let unit = format!("claude-rc@{name}.service");
    let mut claude = String::new();
    if ended {
        // It ends after its Claude has stopped waiting for children.
        claude.push_str("(sleep 0.3; exit 0) & ");
    }
    for _ in 0..live {
        claude.push_str("sleep 300 & ");
    }
    // Its own process never waits for a child: one that ended stays a zombie.
    claude.push_str("exec sleep 300");
    let serve = if current {
        SERVE
    } else {
        "/fake/old/bin/claude-rc-serve"
    };
    let child = Command::new("sh")
        .arg0(TMUX)
        .args(["-c", &format!("# {serve}\nsh -c '{claude}' & wait")])
        .process_group(0)
        .stdin(Stdio::null())
        .spawn()
        .unwrap();
    let pid = child.id();
    m.procs.0.push(child);
    let started = Instant::now();
    let rc = loop {
        if let Some(&(rc, _)) = machine::children(pid).first() {
            break rc;
        }
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "server {name} never started"
        );
        sleep(Duration::from_millis(20));
    };
    // Its Claude starts its sessions, and an ended one is a zombie by now.
    loop {
        let states: Vec<char> = machine::children(rc).into_iter().map(|(_, s)| s).collect();
        if states.len() == live + usize::from(ended) && states.contains(&'Z') == ended {
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "server {name} never started its sessions: {states:?}"
        );
        sleep(Duration::from_millis(20));
    }
    fs::write(m.path(&format!("fake/pid/{unit}")), format!("{pid}\n")).unwrap();
    let alive = machine::children(rc)
        .into_iter()
        .filter(|&(_, s)| s != 'Z')
        .map(|(p, _)| p)
        .collect();
    m.sessions.insert(name.to_string(), alive);
}

/// What Claude Code writes in ~/.claude/sessions/PID.json, or doesn't.
fn write_session(m: &mut Machine, pid: u32) {
    let now = machine::now_ms();
    let file = m.path(&format!("home/.claude/sessions/{pid}.json"));
    let at = |age: i64| now - age;
    let json = match m.rng.below(8) {
        0 => {
            let _ = fs::remove_file(file);
            return;
        }
        1 => "{ not json".to_string(),
        2 => r#"{"status":"busy"}"#.to_string(),
        3 => r#"{"status":"idle"}"#.to_string(),
        _ => {
            let status = m.rng.pick(&["busy", "idle", "waiting", "idle"]);
            let age = m.rng.pick(&[
                0,
                5 * MINUTE,
                20 * MINUTE,
                29 * MINUTE,
                31 * MINUTE,
                180 * MINUTE,
            ]);
            format!(
                "{{\"pid\":{pid},\"status\":\"{status}\",\"statusUpdatedAt\":{}}}",
                at(age)
            )
        }
    };
    fs::write(file, json).unwrap();
}

fn mutate(m: &mut Machine, names: &[String]) {
    let name = names[m.rng.below(names.len())].clone();
    match m.rng.below(5) {
        0 => {
            let kind = m.rng.pick(&KINDS);
            make(m, &name, kind);
        }
        1 => {
            let _ = fs::remove_dir_all(m.projects().join(&name));
        }
        2 => {
            // A clone's origin, written whenever it comes.
            let dir = m.projects().join(&name);
            if dir.join(".git").is_dir() {
                fs::write(dir.join(".git/config"), "[remote \"origin\"]\n\turl = x\n").unwrap();
            }
        }
        _ => {
            let all: Vec<u32> = m.sessions.values().flatten().copied().collect();
            if !all.is_empty() {
                let pid = m.rng.pick(&all);
                write_session(m, pid);
            }
        }
    }
}

/// One run of one of them.
struct Run {
    log: PathBuf,
    notes: PathBuf,
    id: String,
}

fn run(m: &Machine, config: &Path, new: bool, id: &str, words: &[&str]) -> Run {
    let tag = if new { "new" } else { "old" };
    let log = m.path(&format!("fake/{tag}.log"));
    let runtime = m.path(&format!("run/{tag}"));
    let id = format!("{tag}-{id}");
    let path = format!(
        "{}:{}",
        m.path("bin").display(),
        std::env::var("PATH").unwrap()
    );
    let mut cmd = if new {
        let mut c = Command::new(env!("CARGO_BIN_EXE_burrow-rc-scan"));
        c.arg(config).args(words);
        c
    } else {
        let mut c = Command::new(which("bash"));
        c.arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/old/old-scan.sh"
        ))
        .args(words);
        c
    };
    let out = cmd
        .env("PATH", path)
        .env("HOME", m.path("home"))
        .env("XDG_RUNTIME_DIR", &runtime)
        .env("INVOCATION_ID", &id)
        .env("LOG", &log)
        .env("FAKE", m.path("fake"))
        .env("PROJECTS", m.projects())
        .env("FLAKE", m.flake())
        .env("TMUX", TMUX)
        .env("SERVE", SERVE)
        .env("IDLE", IDLE.to_string())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{tag} {words:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    Run {
        log,
        notes: runtime.join("claude-rc-scan"),
        id,
    }
}

fn calls(r: &Run) -> Vec<String> {
    let text = fs::read_to_string(&r.log).unwrap_or_default();
    let _ = fs::remove_file(&r.log);
    text.lines().map(str::to_string).collect()
}

fn note(r: &Run, kind: &str) -> PathBuf {
    r.notes.join(format!("{}.{kind}", r.id))
}

/// A timer's delay and the wake come from the clock, which each reads a moment
/// apart: so the two agree to within a few seconds.
fn near(a: i64, b: i64) -> bool {
    (a - b).abs() < 5_000
}

fn same_calls(old: &[String], new: &[String], what: &str) {
    let ms = |l: &str| {
        l.split("--on-active=")
            .nth(1)
            .and_then(|r| r.split("ms").next()?.parse::<i64>().ok())
    };
    let ok = old.len() == new.len()
        && old.iter().zip(new).all(|(a, b)| match (ms(a), ms(b)) {
            (Some(x), Some(y)) => {
                near(x, y) && a.replace(&x.to_string(), "") == b.replace(&y.to_string(), "")
            }
            _ => a == b,
        });
    assert!(
        ok,
        "{what}\nshell:\n{}\nrust:\n{}",
        old.join("\n"),
        new.join("\n")
    );
}

fn same_wake(old: &Run, new: &Run, what: &str) {
    let read = |r: &Run| {
        fs::read_to_string(note(r, "wake"))
            .ok()
            .map(|w| w.trim().to_string())
    };
    let (a, b) = (read(old), read(new));
    let ok = match (&a, &b) {
        (Some(x), Some(y)) => match (x.parse::<i64>(), y.parse::<i64>()) {
            (Ok(x), Ok(y)) => near(x, y),
            _ => x == y,
        },
        _ => false,
    };
    assert!(ok, "{what}: wake {a:?} against {b:?}");
}

fn one(seed: u64, seen: &mut BTreeMap<&'static str, usize>) {
    let mut m = setup(seed);
    let names: Vec<String> = ["alpha", "beta", "gamma", "delta", "eps", "flk", "zeta"]
        .map(String::from)
        .to_vec();
    for n in &names {
        let kind = if n == "flk" {
            "plain"
        } else {
            m.rng.pick(&KINDS)
        };
        make(&m, n, kind);
    }
    // Servers on a few folders, some current, some stale, with sessions.
    for n in ["alpha", "beta", "gamma", "delta", "flk"] {
        let (current, live, ended) = (m.rng.below(3) == 0, m.rng.below(3), m.rng.below(3) == 0);
        spawn(&mut m, n, current, live, ended);
        for p in m.sessions[n].clone() {
            write_session(&mut m, p);
        }
    }
    // Units systemd has that no folder wants any more.
    fs::write(
        m.path("fake/units"),
        "claude-rc@alpha.service\nclaude-rc@gone.service\nclaude-rc-origin@gone.path\nclaude-rc-origin@beta.path\nclaude-rc-scan.service\n",
    )
    .unwrap();
    let config = m.config();

    for step in 0..20 {
        for _ in 0..m.rng.below(4) {
            mutate(&mut m, &names);
        }
        let what = format!("seed {seed} step {step}");
        let id = step.to_string();
        let (old, new) = (
            run(&m, &config, false, &id, &[]),
            run(&m, &config, true, &id, &[]),
        );
        let scanned = calls(&new);
        same_calls(&calls(&old), &scanned, &format!("{what}: scan"));
        for word in ["restart", "stop", "systemd-run"] {
            if scanned
                .iter()
                .any(|c| c.contains(word) && !c.contains("claude-rc-scan.timer"))
            {
                *seen.entry(word).or_default() += 1;
            }
        }
        same_wake(&old, &new, &format!("{what}: scan"));

        // The look after it: after something changed or not, and with a spell
        // over or not.
        for _ in 0..m.rng.below(3) {
            mutate(&mut m, &names);
        }
        if m.rng.below(4) == 0 {
            for r in [&old, &new] {
                fs::write(note(r, "wake"), "1\n").unwrap();
            }
        }
        let a = run(&m, &config, false, &id, &["again", &old.id]);
        let b = run(&m, &config, true, &id, &["again", &new.id]);
        let looked = calls(&b);
        same_calls(&calls(&a), &looked, &format!("{what}: again"));
        *seen
            .entry(if looked.is_empty() { "no look" } else { "look" })
            .or_default() += 1;
        for r in [&old, &new] {
            assert!(
                !note(r, "seen").exists() && !note(r, "wake").exists(),
                "{what}: notes left"
            );
        }
    }
}

#[test]
fn rust_and_shell_scan_alike() {
    // The steps cover each way a scan acts, not just the quiet ones.
    let mut seen = BTreeMap::new();
    for seed in 1..=3 {
        one(seed, &mut seen);
    }
    for word in ["restart", "stop", "systemd-run", "look", "no look"] {
        assert!(
            seen.get(word).copied().unwrap_or(0) >= 3,
            "{word} came up too seldom: {seen:?}"
        );
    }
}

#[test]
fn done_asks_for_the_look_after_it() {
    let m = setup(1);
    let config = m.config();
    let (old, new) = (
        run(&m, &config, false, "d", &["done"]),
        run(&m, &config, true, "d", &["done"]),
    );
    for r in [&old, &new] {
        let want = vec![format!(
            "systemctl --user start --no-block claude-rc-again@{}.service",
            r.id
        )];
        assert_eq!(calls(r), want);
    }
}

#[test]
fn an_unknown_setting_is_refused() {
    let m = setup(1);
    let config = m.path("config.json");
    fs::write(&config, r#"{"projectsDir":"/p","surprise":1}"#).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_burrow-rc-scan"))
        .arg(&config)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("surprise"));
}
