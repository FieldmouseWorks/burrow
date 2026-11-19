//! firmware-check as the machine runs it: burrow-firmware CONFIG [--review],
//! on a machine made of files, with stand-ins for what it runs that keep
//! how they were run in FAKE, a dir of the machine's own.
//!
//! Every run is as someone other than root (as nobody, when the tests run
//! as root), so it goes through sudo wherever the tests run.

use std::fs;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::OnceLock;

use burrow_test::{chown, root, said};

const BIN: &str = env!("CARGO_BIN_EXE_burrow-firmware");

/// The shared stand-ins (sudo, runuser, proj, box-notify), installed by
/// burrow_test, and the firmware's own beside them, cached for the test binary.
fn bin() -> &'static Path {
    static BIN: OnceLock<tempfile::TempDir> = OnceLock::new();
    BIN.get_or_init(|| {
        let bin = burrow_test::stand_ins(&["sudo", "runuser", "proj", "box-notify"]);
        for (name, text) in [
            (
                // refresh fails when told to; get-updates says FAKE/updates
                // and exits with FAKE/updates-exit.
                "fwupdmgr",
                "#!/bin/sh\necho \"$*\" >>\"$FAKE/fwupdmgr\"\necho noise >&2\ncase \"$1\" in\n  refresh) echo refreshed; [ ! -e \"$FAKE/fail-refresh\" ] ;;\n  get-updates) cat \"$FAKE/updates\" 2>/dev/null; exit \"$(cat \"$FAKE/updates-exit\" 2>/dev/null || echo 0)\" ;;\nesac\n",
            ),
            (
                "dmidecode",
                "#!/bin/sh\necho \"$*\" >>\"$FAKE/dmidecode\"\necho noise >&2\n[ \"$1\" = -s ] && cat \"$FAKE/dmi-$2\" 2>/dev/null\n",
            ),
            (
                "nvme",
                "#!/bin/sh\necho \"$*\" >>\"$FAKE/nvme\"\necho noise >&2\ncat \"$FAKE/drives\" 2>/dev/null\n",
            ),
        ] {
            burrow_test::executable(bin.path().join(name), text);
        }
        bin
    })
    .path()
}

struct Machine {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

impl Machine {
    /// A machine whose state dir is empty, with a CPU in its cpuinfo.
    fn new() -> Machine {
        Machine::with_app(None)
    }

    fn with_app(app: Option<&str>) -> Machine {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        let m = Machine { _dir: dir, root };
        for d in ["fake", "state"] {
            fs::create_dir_all(m.path(d)).unwrap();
        }
        fs::write(
            m.path("cpuinfo"),
            "processor\t: 0\nvendor_id\t: AuthenticAMD\nmodel name\t: AMD Ryzen 7 PRO 8700GE\n\nprocessor\t: 1\nmodel name\t: AMD Ryzen 7 PRO 8700GE\n",
        )
        .unwrap();
        let b = bin();
        let config = serde_json::json!({
            "name": "firmware-check", "host": "box", "offered": m.path("state/firmware-offered"),
            "user": "sam", "project": "box-project", "app": app, "owner": "Sam",
            "notify": "box-notify", "reviewHints": "the board vendor's page", "cpuinfo": m.path("cpuinfo"),
            "tools": {"sudo": b.join("sudo"), "notify": b.join("box-notify"), "fwupdmgr": b.join("fwupdmgr"),
                      "dmidecode": b.join("dmidecode"), "nvme": b.join("nvme"),
                      "runuser": b.join("runuser"), "proj": b.join("proj")},
        });
        fs::write(m.path("config.json"), config.to_string()).unwrap();
        m
    }

    /// Its config, changed by `change`.
    fn config(&self, change: impl FnOnce(&mut serde_json::Value)) {
        let path = self.path("config.json");
        let mut config = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        change(&mut config);
        fs::write(path, config.to_string()).unwrap();
    }

    fn path(&self, p: &str) -> PathBuf {
        self.root.join(p)
    }

    /// FAKE/NAME, which makes a stand-in fail or answer otherwise.
    fn set(&self, name: &str, text: &str) {
        fs::write(self.path("fake").join(name), text).unwrap();
    }

    fn unset(&self, name: &str) {
        fs::remove_file(self.path("fake").join(name)).unwrap();
    }

    /// firmware-check with `words`, as anyone but root starts it.
    fn run(&self, words: &[&str]) -> Output {
        if root() {
            // Everything nobody's, for nobody's run.
            chown(&self.root);
        }
        let mut c = Command::new(BIN);
        c.arg(self.path("config.json"))
            .args(words)
            .env("FAKE", self.path("fake"));
        if root() {
            c.uid(65534).gid(65534);
        }
        c.output().unwrap()
    }

    /// How the stand-in `name` was run, since it was last asked.
    fn fake(&self, name: &str) -> String {
        let path = self.path("fake").join(name);
        let text = fs::read_to_string(&path).unwrap_or_default();
        let _ = fs::remove_file(path);
        text
    }

    fn offered(&self) -> Option<String> {
        fs::read_to_string(self.path("state/firmware-offered")).ok()
    }
}

fn ok() -> (Option<i32>, String, String) {
    (Some(0), String::new(), String::new())
}

/// A device fwupd knows: its name, its version, and its release's version
/// and summary, if it has one.
type Device<'a> = (&'a str, &'a str, Option<(&'a str, Option<&'a str>)>);

/// What fwupd offers for `devices`.
fn updates(devices: &[Device]) -> String {
    let devices: Vec<_> = devices
        .iter()
        .map(|(name, version, release)| {
            let releases: Vec<_> = release
                .iter()
                .map(|(v, s)| serde_json::json!({"Version": v, "Summary": s}))
                .collect();
            serde_json::json!({"Name": name, "Version": version, "Releases": releases})
        })
        .collect();
    serde_json::json!({"Devices": devices}).to_string()
}

const CLOSED: &str = "close|firmware|No firmware updates on offer any more.\n";

fn opened(list: &str) -> String {
    format!(
        "open|firmware|Firmware update available|fwupd has firmware for box:\n{list}\n\nFlashing is your call. To go ahead, tell the box operator \"flash it\"; it will check the release notes, snapshot, and run `fwupdmgr update`.\n"
    )
}

#[test]
fn a_usage_or_a_config_it_cant_take_exits_2_having_run_nothing() {
    let m = Machine::new();
    let out = Command::new(BIN).output().unwrap();
    assert_eq!(
        said(&out),
        (
            Some(2),
            String::new(),
            "usage: burrow-firmware CONFIG [--review]\n".into()
        )
    );
    let out = Command::new(BIN).arg(m.path("none.json")).output().unwrap();
    let (code, _, stderr) = said(&out);
    assert_eq!(code, Some(2));
    assert!(stderr.starts_with("burrow-firmware: "), "{stderr}");
    for words in [&["review"][..], &["--review", "now"], &["now"]] {
        assert_eq!(
            said(&m.run(words)),
            (
                Some(2),
                String::new(),
                "usage: firmware-check [--review]\n".into()
            ),
            "{words:?}"
        );
    }
    for f in ["sudo", "fwupdmgr", "dmidecode", "proj", "notify"] {
        assert_eq!(m.fake(f), "", "{f}");
    }
}

#[test]
fn it_starts_again_through_sudo() {
    let m = Machine::new();
    m.set("updates", "{}");
    m.run(&[]);
    m.run(&["--review"]);
    let config = m.path("config.json");
    assert_eq!(
        m.fake("sudo"),
        format!(
            "-- {BIN} {c} __through-sudo\n-- {BIN} {c} __through-sudo --review\n",
            c = config.display()
        )
    );
}

#[test]
fn an_offer_is_told_once_and_again_when_it_changes() {
    let m = Machine::new();
    m.set(
        "updates",
        &updates(&[
            (
                "System Firmware",
                "1.2",
                Some(("1.4", Some("Fixes the fans"))),
            ),
            ("Nothing new", "7", None),
            ("SSD", "3", Some(("4", None))),
        ]),
    );
    assert_eq!(said(&m.run(&[])), ok());
    assert_eq!(
        m.fake("fwupdmgr"),
        "refresh --force --no-unreported-check --no-metadata-check\nget-updates --json --no-unreported-check --no-metadata-check\n"
    );
    let list = "- System Firmware: 1.2 -> 1.4 Fixes the fans\n- SSD: 3 -> 4 ";
    assert_eq!(m.fake("notify"), opened(list));
    assert_eq!(m.offered(), Some(format!("{list}\n")));
    // The same offer the next day: nothing new to tell.
    assert_eq!(said(&m.run(&[])), ok());
    assert_eq!(m.fake("notify"), "");
    // fwupd saying it with its exit of 2: what it said counts.
    m.set("updates-exit", "2");
    assert_eq!(said(&m.run(&[])), ok());
    assert_eq!(m.fake("notify"), "");
    // Another release since: told again.
    m.set(
        "updates",
        &updates(&[("System Firmware", "1.2", Some(("1.5", Some("Fans again"))))]),
    );
    assert_eq!(said(&m.run(&[])), ok());
    let list = "- System Firmware: 1.2 -> 1.5 Fans again";
    assert_eq!(m.fake("notify"), opened(list));
    assert_eq!(m.offered(), Some(format!("{list}\n")));
    // What it kept, with newlines after it, is still the same offer.
    fs::write(m.path("state/firmware-offered"), format!("{list}\n\n\n")).unwrap();
    assert_eq!(said(&m.run(&[])), ok());
    assert_eq!(m.fake("notify"), "");
}

#[test]
fn nothing_on_offer_closes_the_notice_every_day() {
    let m = Machine::new();
    m.set("updates", &updates(&[("A", "1", Some(("2", None)))]));
    m.run(&[]);
    m.fake("notify");
    for (said_, exit) in [
        (updates(&[("A", "2", None)]), "0"),
        (String::new(), "2"),
        ("Failed to connect to daemon".into(), "1"),
        ("{}".into(), "0"),
    ] {
        m.set("updates", &said_);
        m.set("updates-exit", exit);
        assert_eq!(said(&m.run(&[])), ok(), "{said_}");
        assert_eq!(m.fake("notify"), CLOSED, "{said_}");
        assert_eq!(m.offered(), None);
    }
    // No fwupdmgr at all is nothing on offer too.
    m.config(|c| c["tools"]["fwupdmgr"] = "/nowhere/fwupdmgr".into());
    assert_eq!(said(&m.run(&[])), ok());
    assert_eq!(m.fake("notify"), CLOSED);
}

#[test]
fn a_refresh_that_fails_still_looks() {
    let m = Machine::new();
    m.set("fail-refresh", "");
    m.set("updates", &updates(&[("A", "1", Some(("2", Some("x"))))]));
    assert_eq!(said(&m.run(&[])), ok());
    assert!(m.fake("fwupdmgr").contains("get-updates"));
    assert_eq!(m.fake("notify"), opened("- A: 1 -> 2 x"));
}

#[test]
fn an_offer_whose_notice_didnt_open_is_told_again() {
    let m = Machine::new();
    m.set("updates", &updates(&[("A", "1", Some(("2", Some("x"))))]));
    m.set("fail-notify", "");
    assert_eq!(
        said(&m.run(&[])),
        (
            Some(1),
            String::new(),
            format!(
                "firmware-check: {} open: exit status: 1\n",
                bin().join("box-notify").display()
            )
        )
    );
    assert_eq!(m.offered(), None);
    m.unset("fail-notify");
    m.fake("notify");
    assert_eq!(said(&m.run(&[])), ok());
    assert_eq!(m.fake("notify"), opened("- A: 1 -> 2 x"));
    assert!(m.offered().is_some());
    // A close that fails says so.
    m.set("updates", "{}");
    m.set("fail-notify", "");
    let (code, _, stderr) = said(&m.run(&[]));
    assert_eq!(code, Some(1));
    assert!(
        stderr.ends_with("box-notify close: exit status: 1\n"),
        "{stderr}"
    );
    assert_eq!(m.offered(), None);
}

#[test]
fn an_offer_it_cant_keep_says_so() {
    let m = Machine::new();
    m.set("updates", &updates(&[("A", "1", Some(("2", None)))]));
    fs::create_dir(m.path("state/firmware-offered")).unwrap();
    let (code, stdout, stderr) = said(&m.run(&[]));
    assert_eq!((code, stdout.as_str()), (Some(1), ""));
    assert!(
        stderr.starts_with(&format!(
            "firmware-check: {}: ",
            m.path("state/firmware-offered").display()
        )),
        "{stderr}"
    );
    // Nothing told while it can't read what it told before.
    assert_eq!(m.fake("notify"), "");
    // Nor drop it.
    m.set("updates", "{}");
    let (code, _, stderr) = said(&m.run(&[]));
    assert_eq!(code, Some(1));
    assert!(stderr.contains("firmware-offered: "), "{stderr}");
    assert_eq!(m.fake("notify"), "");
}

/// The words proj was run with, each, since it was last asked.
fn proj_words(m: &Machine) -> Vec<String> {
    let mut runs = burrow_test::proj_jobs(&m.path("fake"));
    let _ = fs::remove_file(m.path("fake/proj"));
    assert_eq!(runs.len(), 1, "{runs:?}");
    runs.remove(0)
}

fn today() -> String {
    let out = Command::new("date").arg("+%F").output().unwrap();
    String::from_utf8(out.stdout).unwrap().trim_end().into()
}

fn task(board: &str, bios: &str, date: &str, nvme: &str, cpu: &str) -> String {
    format!(
        "Monthly firmware review. Don't flash or change anything; look and report.
box: board {board}, BIOS {bios} ({date}), NVMe: {nvme}. CPU: {cpu}. fwupd's view: `fwupdmgr get-devices` and `fwupdmgr get-updates`.
Find the newest firmware the vendors publish for each (the board vendor's page; microcode itself arrives with NixOS). Say what's out of date and whether any of it fixes a security problem that matters for a server on the internet. If something is worth flashing, run `box-notify open firmware-review \"title\" \"what, why, and how it would be flashed on box\"` so Sam sees it; flashing is their call. If everything is current, `box-notify close firmware-review \"current as of {}\"`.",
        today()
    )
}

#[test]
fn a_review_is_a_job_told_what_the_machine_runs() {
    let m = Machine::with_app(Some("box-app"));
    m.set("dmi-bios-version", "2.20.1272\n");
    m.set("dmi-bios-release-date", "03/14/2025\n");
    m.set("dmi-baseboard-product-name", "B665D4U-1L\n");
    m.set(
        "drives",
        r#"{"Devices": [
            {"ModelNumber": "Samsung SSD 990 PRO 2TB", "Firmware": "4B2QJXD7"},
            {"ModelNumber": "SAMSUNG MZVL21T0HCLR", "Firmware": "GXA7801Q"},
            {"ModelNumber": "Samsung SSD 990 PRO 2TB", "Firmware": "4B2QJXD7"}
        ]}"#,
    );
    assert_eq!(said(&m.run(&["--review"])), ok());
    assert_eq!(
        m.fake("dmidecode"),
        "-s bios-version\n-s bios-release-date\n-s baseboard-product-name\n"
    );
    assert_eq!(m.fake("nvme"), "list -o json\n");
    // As the user, without root's runtime dir: proj finds the user's.
    let runuser = m.fake("runuser");
    assert!(
        runuser.starts_with(&format!(
            "-u sam -- {} run --role firmware ",
            bin().join("proj").display()
        )),
        "{runuser}"
    );
    assert_eq!(m.fake("runuser.xdg"), "unset\n");
    assert_eq!(
        proj_words(&m),
        [
            "run".to_owned(),
            "--role".into(),
            "firmware".into(),
            "--app".into(),
            "box-app".into(),
            "box-project".into(),
            task(
                "B665D4U-1L",
                "2.20.1272",
                "03/14/2025",
                "SAMSUNG MZVL21T0HCLR firmware GXA7801Q;Samsung SSD 990 PRO 2TB firmware 4B2QJXD7",
                "AMD Ryzen 7 PRO 8700GE"
            )
        ]
    );
    // Nothing is looked at by fwupd, and nothing told.
    assert_eq!(m.fake("fwupdmgr"), "");
    assert_eq!(m.fake("notify"), "");
}

#[test]
fn what_a_review_cant_find_out_is_unknown() {
    // No dmidecode answers, nvme saying nothing it can read, no model name.
    let m = Machine::new();
    m.set("drives", "{}");
    fs::write(m.path("cpuinfo"), "processor\t: 0\nCPU part\t: 0xd0c\n").unwrap();
    assert_eq!(said(&m.run(&["--review"])), ok());
    let words = proj_words(&m);
    assert_eq!(&words[..4], ["run", "--role", "firmware", "box-project"]);
    assert_eq!(
        words[4],
        task("unknown", "unknown", "unknown", "unknown", "unknown")
    );
    // No drives is none, and no cpuinfo at all is unknown.
    m.set("drives", r#"{"Devices": []}"#);
    fs::remove_file(m.path("cpuinfo")).unwrap();
    m.run(&["--review"]);
    assert_eq!(
        proj_words(&m)[4],
        task("unknown", "unknown", "unknown", "", "unknown")
    );
}

#[test]
fn a_review_proj_wont_take_says_so() {
    let m = Machine::new();
    m.set("fail-proj", "");
    // proj's own complaint on stderr comes first.
    assert_eq!(
        said(&m.run(&["--review"])),
        (
            Some(1),
            String::new(),
            "proj: no\nfirmware-check: proj run: exit status: 1\n".into()
        )
    );
}
