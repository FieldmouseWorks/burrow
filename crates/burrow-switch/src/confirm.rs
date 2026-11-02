//! HOST-confirm: the system HOST-switch put live kept, and booted into from
//! now on. With trial boots, HOST-boot makes it the boot default: a new
//! kernel or initrd boots once on trial first.

use crate::{Broke, Config, complain, say};

/// What it exits with, or why it broke.
pub fn confirm(cfg: &Config) -> Result<u8, Broke> {
    // Any switch held off until this is done.
    let _lock = cfg.lock()?;
    let activating = |c: &Config| c.show(&c.service(), "ActiveState") == "activating";
    if activating(cfg) {
        complain("too late: the revert is already running");
        return Ok(1);
    }
    let Some(new) = cfg.recorded("pending")? else {
        say("nothing pending");
        return Ok(0);
    };
    if cfg.running()? != new {
        complain("running system isn't the pending one (did it revert?); not confirming");
        return Ok(1);
    }
    // The boot default first, and only then the revert disarmed: if this
    // fails halfway, the change still reverts on schedule.
    if let Err(Broke(why)) = cfg.set_profile(&new).and_then(|()| cfg.boot_default(&new)) {
        complain(&format!("{}: {why}", cfg.confirm));
        complain("couldn't make it the boot default; still pending, and it reverts on schedule");
        return Ok(1);
    }
    cfg.systemctl_quiet(&["stop", &cfg.timer()]);
    // The timer may have fired just before it was stopped: that revert
    // lands before what's live is looked at.
    cfg.wait_while(activating);
    let running = cfg.running()?;
    if running != new {
        // The revert fired while it was confirming: what's actually running
        // boots.
        cfg.set_profile(&running)?;
        cfg.boot_default(&running)?;
        cfg.remove("pending")?;
        cfg.remove("prev")?;
        complain(&format!(
            "too late: it reverted while confirming; {} is running",
            running.display()
        ));
        return Ok(1);
    }
    cfg.remove("pending")?;
    cfg.remove("prev")?;
    // With trial boots, HOST-boot said above what boots next (it may be
    // queued for a trial boot).
    say(&format!("confirmed: {}", new.display()));
    Ok(0)
}
