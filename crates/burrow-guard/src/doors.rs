//! R2: firmware settings that can stop the box booting, and Hetzner Robot
//! calls with no undo.

use std::sync::LazyLock;

use regex::Regex;

use crate::{any_line, pattern};

/// efibootmgr with a flag that changes a boot entry, the order or the
/// timeout; reading them is fine.
static EFIBOOTMGR: LazyLock<Regex> = LazyLock::new(|| {
    pattern(
        r"(^|[;&|[:space:]])efibootmgr([[:space:]][^;&|]*)?[[:space:]](-[a-zA-Z]*[cCBbAaoOnNTt]|--(create|delete|bootorder|bootnext|active|inactive|delete-bootorder|timeout|delete-timeout))",
    )
});

const EFIVARS: &str = "/sys/firmware/efi/efivars";

/// A command that writes, moves or deletes files, or a redirect into the
/// EFI variables.
static EFIVARS_WRITE: LazyLock<Regex> = LazyLock::new(|| {
    pattern(
        r"(^|[;&|[:space:]])(rm|chattr|tee|dd|cp|mv|install|efivar)[[:space:]]|>[[:space:]]*/sys/firmware/efi/efivars",
    )
});

/// The Robot API. `robot` (modules/robot.nix) offers only status, rescue,
/// reset and wake-on-LAN; a raw call is what can cancel a server, order
/// one, or arm a reinstall that wipes the disks on the next reset.
static ROBOT: LazyLock<Regex> = LazyLock::new(|| pattern(r"robot-ws\.your-server\.de"));
static ROBOT_ONE_WAY: LazyLock<Regex> = LazyLock::new(|| {
    pattern(r"cancellation|/boot/[^/[:space:]]+/(linux|windows|vnc|plesk|cpanel)|/order/")
});

pub fn asks(cmd: &str) -> Vec<String> {
    let mut asks = Vec::new();
    if any_line(&EFIBOOTMGR, cmd) {
        asks.push("Changes UEFI boot entries (R2: a bad one can stop the box booting).".to_owned());
    }
    if cmd.contains(EFIVARS) && any_line(&EFIVARS_WRITE, cmd) {
        asks.push("Writes EFI variables (R2: a bad one can stop the box booting).".to_owned());
    }
    if any_line(&ROBOT, cmd) && any_line(&ROBOT_ONE_WAY, cmd) {
        asks.push(
            "Hetzner Robot call that cancels, orders or reinstalls a server (R2: no undo)."
                .to_owned(),
        );
    }
    asks
}
