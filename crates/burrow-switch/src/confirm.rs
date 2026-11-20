//! HOST-confirm: the system HOST-switch put live kept, and booted into from
//! now on. With trial boots, HOST-boot makes it the boot default: a new
//! kernel or initrd boots once on trial first.

use std::path::Path;

use crate::switch::PENDING;
use crate::{Broke, Config, as_recorded, canonical, complain, say};

/// What it exits with, or why it broke. Told the system it means (`mine`),
/// it confirms only that: another pending in its place replaced it, and is
/// someone's, mid-change and unchecked.
pub fn confirm(cfg: &Config, mine: Option<&Path>) -> Result<u8, Broke> {
    // Any switch held off until this is done.
    let _lock = cfg.lock()?;
    let activating = |c: &Config| c.show(&c.service(), "ActiveState") == "activating";
    if activating(cfg) {
        complain("too late: the revert is already running");
        return Ok(1);
    }
    let pending = cfg.recorded("pending")?;
    // Decided under the lock, which a switch holds as it writes `pending`.
    if let Some(mine) = mine.map(as_recorded)
        && pending.as_ref() != Some(&mine)
    {
        // Nothing pending: someone confirmed it already, or it's gone.
        if pending.is_none() && canonical(&cfg.profile)? == mine {
            say(&format!("already confirmed: {}", mine.display()));
            return Ok(0);
        }
        let what = pending.map_or("nothing is".into(), |p| format!("{} is", p.display()));
        complain(&format!(
            "{} isn't pending ({what}); not confirming it",
            mine.display()
        ));
        return Ok(PENDING);
    }
    let Some(new) = pending else {
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
