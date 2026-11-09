//! The monthly review: a Claude job looks at the firmware LVFS doesn't
//! carry (board BIOS, NVMe, Intel ME or AMD PSP), told what the machine
//! runs. It looks and reports; flashing stays the owner's call.

use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

use serde_json::Value;

use crate::{Broke, Config, shown};

pub fn run(cfg: &Config) -> Result<(), Broke> {
    let unknown = || "unknown".to_owned();
    let bios = dmi(cfg, "bios-version").unwrap_or_else(unknown);
    let bios_date = dmi(cfg, "bios-release-date").unwrap_or_else(unknown);
    let board = dmi(cfg, "baseboard-product-name").unwrap_or_else(unknown);
    let nvme = said(&cfg.tools.nvme, &["list", "-o", "json"])
        .and_then(|s| drives(&s))
        .unwrap_or_else(unknown);
    let cpu = fs::read(&cfg.cpuinfo)
        .ok()
        .and_then(|s| model(&s))
        .unwrap_or_else(unknown);
    let (host, notify, owner) = (&cfg.host, &cfg.notify, &cfg.owner);
    let hints = &cfg.review_hints;
    let today = jiff::Zoned::now().strftime("%F").to_string();
    let task = format!(
        "Monthly firmware review. Don't flash or change anything; look and report.
{host}: board {board}, BIOS {bios} ({bios_date}), NVMe: {nvme}. CPU: {cpu}. fwupd's view: `fwupdmgr get-devices` and `fwupdmgr get-updates`.
Find the newest firmware the vendors publish for each ({hints}; microcode itself arrives with NixOS). Say what's out of date and whether any of it fixes a security problem that matters for a server on the internet. If something is worth flashing, run `{notify} open firmware-review \"title\" \"what, why, and how it would be flashed on {host}\"` so {owner} sees it; flashing is their call. If everything is current, `{notify} close firmware-review \"current as of {today}\"`."
    );
    // As the user, whose job it is; proj finds the user's runtime dir itself.
    let runuser = &cfg.tools.runuser;
    let mut c = Command::new(runuser);
    c.args(["-u", &cfg.user, "--"])
        .arg(&cfg.tools.proj)
        .args(["run", "--role", "firmware"]);
    if let Some(app) = &cfg.app {
        c.args(["--app", app]);
    }
    match c
        .arg(&cfg.project)
        .arg(&task)
        .env_remove("XDG_RUNTIME_DIR")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .status()
    {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(Broke(format!("proj run: {s}"))),
        Err(e) => Err(Broke(format!("{}: {e}", runuser.display()))),
    }
}

/// What `program args` said, when it worked.
fn said(program: &Path, args: &[&str]) -> Option<Vec<u8>> {
    Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| o.stdout)
}

/// What dmidecode says the board's `key` is, without its newline.
fn dmi(cfg: &Config, key: &str) -> Option<String> {
    said(&cfg.tools.dmidecode, &["-s", key]).map(|s| {
        let s = String::from_utf8_lossy(&s);
        s.trim_end_matches('\n').to_owned()
    })
}

/// The NVMe drives `nvme list -o json` lists: "MODEL firmware FIRMWARE"
/// for each, in byte order, each once, joined with ";". Nothing when what
/// it said isn't a list of drives.
pub fn drives(said: &[u8]) -> Option<String> {
    let mut drives = Vec::new();
    for value in serde_json::Deserializer::from_slice(said).into_iter::<Value>() {
        let Some(Value::Array(devices)) = value.ok()?.get("Devices").cloned() else {
            return None;
        };
        for d in &devices {
            if !(d.is_object() || d.is_null()) {
                return None;
            }
            drives.push(format!(
                "{} firmware {}",
                shown(d.get("ModelNumber")),
                shown(d.get("Firmware"))
            ));
        }
    }
    drives.sort();
    drives.dedup();
    Some(drives.join(";"))
}

/// The CPU's model, from /proc/cpuinfo's first "model name" line: what's
/// after its first colon, without the spaces before it. Nothing when it
/// has no such line.
pub fn model(cpuinfo: &[u8]) -> Option<String> {
    let line = cpuinfo
        .split(|&b| b == b'\n')
        .find(|l| l.starts_with(b"model name"))?;
    let value = match line.iter().position(|&b| b == b':') {
        Some(i) => &line[i + 1..],
        None => line,
    };
    let start = value.iter().position(|&b| b != b' ').unwrap_or(value.len());
    Some(String::from_utf8_lossy(&value[start..]).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_drive_is_its_model_and_firmware_once_in_byte_order() {
        let said = br#"{"Devices": [
            {"ModelNumber": "Samsung SSD 990 PRO 2TB", "Firmware": "4B2QJXD7"},
            {"ModelNumber": "SAMSUNG MZVL21T0HCLR", "Firmware": "GXA7801Q"},
            {"ModelNumber": "Samsung SSD 990 PRO 2TB", "Firmware": "4B2QJXD7"},
            {"ModelNumber": "Bare"}
        ]}"#;
        assert_eq!(
            drives(said).as_deref(),
            Some(
                "Bare firmware null;SAMSUNG MZVL21T0HCLR firmware GXA7801Q;Samsung SSD 990 PRO 2TB firmware 4B2QJXD7"
            )
        );
        assert_eq!(drives(br#"{"Devices": []}"#).as_deref(), Some(""));
        assert_eq!(drives(b"").as_deref(), Some(""));
        assert_eq!(
            drives(br#"{"Devices": [null]}"#).as_deref(),
            Some("null firmware null")
        );
    }

    #[test]
    fn what_isnt_a_list_of_drives_is_none() {
        for said in [
            &b"{}"[..],
            b"nvme: not found",
            br#"{"Devices": "x"}"#,
            br#"{"Devices": [1]}"#,
            br#"{"Devices": []} oops"#,
            br#"[]"#,
        ] {
            assert_eq!(drives(said), None, "{}", String::from_utf8_lossy(said));
        }
    }

    #[test]
    fn the_cpu_is_its_first_model_name() {
        let cpuinfo = b"processor\t: 0\nvendor_id\t: AuthenticAMD\nmodel\t\t: 117\nmodel name\t: AMD Ryzen 7 PRO 8700GE\nprocessor\t: 1\nmodel name\t: Other\n";
        assert_eq!(model(cpuinfo).as_deref(), Some("AMD Ryzen 7 PRO 8700GE"));
        // Spaces before it go, tabs and what's after it stay; a line
        // without a colon is all of it.
        assert_eq!(
            model(b"model name:   \tX: Y  ").as_deref(),
            Some("\tX: Y  ")
        );
        assert_eq!(model(b"model nameless").as_deref(), Some("model nameless"));
        assert_eq!(model(b"  model name: X"), None);
        assert_eq!(model(b"processor\t: 0\n"), None);
        assert_eq!(model(b""), None);
    }
}
