//! The daily check: what firmware fwupd (LVFS) offers for the machine. An
//! offer the owner hasn't been told about opens the `firmware` notice;
//! nothing on offer closes it.

use std::fs;
use std::io;
use std::process::{Command, Stdio};

use serde_json::Value;

use crate::{Broke, Config, shown};

/// The notice an offer opens.
const NOTICE: &str = "firmware";

/// What both of fwupdmgr's runs are told: every device, whether or not its
/// vendor reports back, and metadata however old.
const FLAGS: [&str; 2] = ["--no-unreported-check", "--no-metadata-check"];

pub fn run(cfg: &Config) -> Result<(), Broke> {
    let fwupdmgr = &cfg.tools.fwupdmgr;
    // A refresh that fails leaves the metadata that was there.
    let _ = Command::new(fwupdmgr)
        .args(["refresh", "--force"])
        .args(FLAGS)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    // What it says it offers, whatever it exits with: 2 is nothing on offer.
    let said = Command::new(fwupdmgr)
        .args(["get-updates", "--json"])
        .args(FLAGS)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map(|o| o.stdout)
        .unwrap_or_default();
    let list = offers(&said);
    let offered = &cfg.offered;
    if list.is_empty() {
        match fs::remove_file(offered) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => {
                return Err(Broke(format!("{}: {e}", offered.display())));
            }
            _ => {}
        }
        return notify(
            cfg,
            &["close", NOTICE, "No firmware updates on offer any more."],
        );
    }
    let told = match fs::read(offered) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(Broke(format!("{}: {e}", offered.display()))),
    };
    if trimmed(&told) == list.as_bytes() {
        return Ok(());
    }
    // Told first, then kept: an offer whose notice didn't open is told again
    // the next day.
    let host = &cfg.host;
    notify(
        cfg,
        &[
            "open",
            NOTICE,
            "Firmware update available",
            &format!(
                "fwupd has firmware for {host}:\n{list}\n\nFlashing is your call. To go ahead, tell the {host} operator \"flash it\"; it will check the release notes, snapshot, and run `fwupdmgr update`."
            ),
        ],
    )?;
    fs::write(offered, format!("{list}\n"))
        .map_err(|e| Broke(format!("{}: {e}", offered.display())))
}

/// What `fwupdmgr get-updates --json` said is on offer: a line for each
/// device with a release, "- NAME: VERSION -> RELEASE SUMMARY", as jq's
/// interpolation shows each (with no summary, nothing after the space). It
/// reads what was said as jq does, one JSON value after another, and stops
/// at the first that isn't one, keeping what came before.
pub fn offers(said: &[u8]) -> String {
    let mut lines = Vec::new();
    for value in serde_json::Deserializer::from_slice(said).into_iter::<Value>() {
        let Ok(value) = value else { break };
        let Some(Value::Array(devices)) = value.get("Devices") else {
            continue;
        };
        for device in devices {
            let Some(Value::Array(releases)) = device.get("Releases") else {
                continue;
            };
            let Some(release) = releases.first() else {
                continue;
            };
            let summary = match release.get("Summary") {
                None | Some(Value::Null | Value::Bool(false)) => String::new(),
                s => shown(s),
            };
            lines.push(format!(
                "- {}: {} -> {} {summary}",
                shown(device.get("Name")),
                shown(device.get("Version")),
                shown(release.get("Version"))
            ));
        }
    }
    let list = lines.join("\n");
    list.trim_end_matches('\n').to_owned()
}

/// `text` without its trailing newlines, as a shell's $(…) has it.
fn trimmed(text: &[u8]) -> &[u8] {
    let end = text.iter().rposition(|&b| b != b'\n').map_or(0, |i| i + 1);
    &text[..end]
}

/// HOST-notify, told `words`: whether the notice opened or closed.
fn notify(cfg: &Config, words: &[&str]) -> Result<(), Broke> {
    let notify = &cfg.tools.notify;
    match Command::new(notify)
        .args(words)
        .stdin(Stdio::null())
        .status()
    {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(Broke(format!("{} {}: {s}", notify.display(), words[0]))),
        Err(e) => Err(Broke(format!("{}: {e}", notify.display()))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_offer_is_a_line_for_each_device_with_a_release() {
        let said = br#"{"Devices": [
            {"Name": "System Firmware", "Version": "1.2", "Releases": [
                {"Version": "1.4", "Summary": "Fixes the fans"}, {"Version": "1.3"}]},
            {"Name": "Nothing new", "Version": "7", "Releases": []},
            {"Name": "No releases", "Version": "8"},
            {"Name": "Null releases", "Version": "8", "Releases": null},
            {"Name": "SSD", "Version": 3, "Releases": [{"Version": "4"}]},
            {"Version": "5", "Releases": [{"Version": "6", "Summary": null}]},
            {"Name": "Odd", "Version": "1", "Releases": [{"Version": "2", "Summary": false}]},
            {"Name": "Counted", "Version": "1", "Releases": [{"Version": "2", "Summary": 9}]}
        ]}"#;
        assert_eq!(
            offers(said),
            "- System Firmware: 1.2 -> 1.4 Fixes the fans\n- SSD: 3 -> 4 \n- null: 5 -> 6 \n- Odd: 1 -> 2 \n- Counted: 1 -> 2 9"
        );
    }

    #[test]
    fn what_isnt_fwupds_json_offers_nothing_after_it() {
        let one =
            br#"{"Devices": [{"Name": "A", "Version": "1", "Releases": [{"Version": "2"}]}]}"#;
        assert_eq!(offers(b""), "");
        assert_eq!(offers(b"No updates available"), "");
        assert_eq!(offers(b"{}"), "");
        assert_eq!(offers(br#"{"Devices": "none"}"#), "");
        assert_eq!(offers(br#"[{"Devices": []}]"#), "");
        // Each value, one after another, until one isn't.
        let two = [&one[..], b"\n", &one[..], b" oops ", &one[..]].concat();
        assert_eq!(offers(&two), "- A: 1 -> 2 \n- A: 1 -> 2 ");
        // A summary ending in newlines, as $(…) cut them.
        let nl = br#"{"Devices": [{"Name": "A", "Version": "1", "Releases": [{"Version": "2", "Summary": "x\n\n"}]}]}"#;
        assert_eq!(offers(nl), "- A: 1 -> 2 x");
    }

    #[test]
    fn trailing_newlines_are_cut_as_a_shell_cuts_them() {
        assert_eq!(trimmed(b"a\nb\n\n"), b"a\nb");
        assert_eq!(trimmed(b"\n\n"), b"");
        assert_eq!(trimmed(b"a "), b"a ");
    }
}
