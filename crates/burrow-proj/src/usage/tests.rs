// What a refused run looks like, as Claude Code wrote it on 2026-11-02 (its
// stream, cut down to what's read), and the machine's state from it: out
// until the reset, the owner told once, and back with the next run that
// isn't refused.

use std::fs;

use serde_json::json;

use super::*;
use crate::config::tests::example;

/// proj's config with its jobs in a dir of its own, the watchdog's waits
/// beside them, a kick port of its own that keeps what it's kicked
/// ([`kicked`]), and a notify that keeps what it's asked, a line each, and
/// answers `pending` with 0 (may still be open) while there's a `pending`
/// file, with the code in `pending-code` when there's one of those, killed
/// when there's a `pending-killed`, and 1 (closed) otherwise.
pub(crate) fn rig() -> (tempfile::TempDir, Config) {
    let d = tempfile::tempdir().unwrap();
    let notify = d.path().join("notify");
    let dir = d.path().display();
    burrow_test::executable(
        &notify,
        format!(
            "#!/bin/sh\nif [ \"$1\" = pending ]; then\n  [ -e {dir}/pending-killed ] && kill -9 $$\n  [ -e {dir}/pending-code ] && exit \"$(cat {dir}/pending-code)\"\n  [ -e {dir}/pending ]; exit\nfi\necho \"$1 $2 $3\" >>{dir}/told\n"
        ),
    );
    let kicks = d.path().join("kicks");
    let (port, _) = burrow_events::http::listen(
        0,
        std::sync::Arc::new(move |req: &burrow_events::http::Request| {
            let mut f = fs::OpenOptions::new()
                .append(true)
                .create(true)
                .open(&kicks)
                .unwrap();
            std::io::Write::write_all(&mut f, format!("{}\n", req.target).as_bytes()).unwrap();
            202
        }),
    )
    .unwrap();
    let mut cfg = example();
    cfg["jobs"] = json!(d.path());
    cfg["notify"] = json!(notify);
    cfg["waits"] = json!(d.path().join(".waits.json"));
    cfg["kick"] = json!(format!("http://127.0.0.1:{port}/kick"));
    (d, serde_json::from_value(cfg).unwrap())
}

/// What the rig's kick port has been kicked, and forgets it.
fn kicked(d: &tempfile::TempDir) -> Vec<String> {
    let f = d.path().join("kicks");
    let got = fs::read_to_string(&f).unwrap_or_default();
    let _ = fs::remove_file(&f);
    let mut got: Vec<String> = got.lines().map(String::from).collect();
    got.sort();
    got
}

pub(super) fn told(d: &tempfile::TempDir) -> Vec<String> {
    fs::read_to_string(d.path().join("told"))
        .unwrap_or_default()
        .lines()
        .map(String::from)
        .collect()
}

const SAID: &str = "You've hit your monthly spend limit";

#[test]
fn out_of_usage_till_it_resets_and_told_once() {
    let (d, cfg) = rig();
    let t = now();
    assert_eq!(out(&cfg), None);
    let s = record(&cfg, "web-1", Some(t + 7200), SAID, t).unwrap();
    assert_eq!(
        (s.until, s.at, s.job.as_str(), s.back),
        (t + 7200, t, "web-1", None)
    );
    assert_eq!(out(&cfg), Some(s.clone()));
    let open = told(&d);
    assert_eq!(open.len(), 1);
    assert!(
        open[0].starts_with("open proj-usage Claude is out of usage until "),
        "{open:?}"
    );
    // Every turn running is refused too: told once, and out till the latest
    // reset any of them gives, from the first refusal.
    assert_eq!(
        record(&cfg, "web-2", Some(t + 3600), SAID, t + 5)
            .unwrap()
            .until,
        t + 7200
    );
    let s3 = record(&cfg, "web-3", Some(t + 9000), SAID, t + 6).unwrap();
    assert_eq!((s3.until, s3.at, s3.job.as_str()), (t + 9000, t, "web-1"));
    assert_eq!(told(&d).len(), 1);
    // Told again only once it's been RETELL: GitHub may not have taken it.
    remind(&cfg);
    assert_eq!(told(&d).len(), 1);
    let mut old = kept(&cfg).unwrap();
    old.told = t - RETELL;
    keep(&cfg, &old).unwrap();
    remind(&cfg);
    assert_eq!(told(&d).len(), 2);
    assert!(told(&d)[1].starts_with("open proj-usage"));
    // A reset unsaid, or already past: an hour on.
    back(&cfg, None);
    fs::remove_file(cfg.jobs.join(FILE)).unwrap();
    assert_eq!(
        record(&cfg, "web-4", None, SAID, t).unwrap().until,
        t + UNSAID
    );
    back(&cfg, None);
    assert_eq!(
        record(&cfg, "web-4", Some(t - 5), SAID, t).unwrap().until,
        t + UNSAID
    );
    // Past its reset it's no longer out, though no run has said so yet.
    let mut past = kept(&cfg).unwrap();
    past.until = t - 1;
    keep(&cfg, &past).unwrap();
    assert_eq!(out(&cfg), None);
}

/// A run that started before the refusal proves nothing: its last call may
/// have come before it. One that started after does, and so does `proj
/// usage back`; a refusal after that is a hold of its own, told again.
#[test]
fn back_with_a_run_that_started_after_it() {
    let (d, cfg) = rig();
    let t = now();
    record(&cfg, "web-1", Some(t + 7200), SAID, t).unwrap();
    back(&cfg, Some(t - 30));
    back(&cfg, Some(t));
    assert!(out(&cfg).is_some() && told(&d).len() == 1);
    back(&cfg, Some(t + 1));
    assert_eq!(out(&cfg), None);
    assert!(kept(&cfg).unwrap().back.is_some());
    assert_eq!(
        told(&d)[1..],
        ["close proj-usage Claude has usage again, and proj's jobs are running.".to_owned()]
    );
    let again = record(&cfg, "web-2", Some(t + 9000), SAID, t + 2).unwrap();
    assert_eq!(
        (again.at, again.job.as_str(), again.back),
        (t + 2, "web-2", None)
    );
    assert_eq!(told(&d).len(), 3);
    back(&cfg, None);
    assert_eq!(out(&cfg), None);
    assert_eq!(told(&d).len(), 4);
}

/// The close is sent again until HOST-notify says it got through, and the
/// record stays a stall's time after the release (the watchdog's deadlines
/// for turns in line count from it), then goes.
#[test]
fn the_close_until_it_got_through_and_the_record_a_stall_after() {
    let (d, cfg) = rig();
    let t = now();
    record(&cfg, "web-1", Some(t + 7200), SAID, t).unwrap();
    fs::write(d.path().join("pending"), "").unwrap();
    back(&cfg, None);
    assert_eq!(told(&d).len(), 2);
    let release = kept(&cfg).unwrap().release();
    assert!(release >= t && release <= now());
    // Still pending, but sent just now: not again yet; RETELL/12 on, again.
    back(&cfg, Some(t + 1));
    assert_eq!(told(&d).len(), 2);
    let mut s = kept(&cfg).unwrap();
    s.told -= RETELL / 12;
    keep(&cfg, &s).unwrap();
    back(&cfg, Some(t + 1));
    assert_eq!(told(&d).len(), 3);
    assert!(told(&d)[2].starts_with("close proj-usage"));
    // Through, but within a stall of the release: kept.
    fs::remove_file(d.path().join("pending")).unwrap();
    back(&cfg, Some(t + 1));
    assert!(kept(&cfg).is_some());
    // A stall on: gone.
    let mut s = kept(&cfg).unwrap();
    s.back = Some(t - cfg.stall_queued() - 1);
    keep(&cfg, &s).unwrap();
    back(&cfg, Some(t + 1));
    assert_eq!(kept(&cfg), None);
    assert_eq!(told(&d).len(), 3);
}

/// Every change of hold moves the deadline of every turn the watchdog
/// watches: each is looked at again (one whose job is gone, dropped).
#[test]
fn every_change_looks_at_every_watched_turn_again() {
    let (d, cfg) = rig();
    let waits = d.path().join(".waits");
    fs::create_dir_all(&waits).unwrap();
    for (when, change) in [
        (
            "record",
            Box::new(|c: &Config| {
                record(c, "web-1", Some(now() + 60), SAID, now()).unwrap();
            }) as Box<dyn Fn(&Config)>,
        ),
        ("back", Box::new(|c: &Config| back(c, None))),
    ] {
        fs::write(waits.join("gone-1"), "{}\n").unwrap();
        change(&cfg);
        assert!(!waits.join("gone-1").exists(), "{when}");
    }
}

/// Only a run that started after the last refusal says usage is back: one
/// that started between the first and a later one proves nothing, whether
/// the later one moved the reset on or not, and so after a reset passed with
/// no run to end the hold, when a run is refused again (and the owner told
/// again, its title's reset being past).
#[test]
fn back_only_with_a_run_after_the_last_refusal() {
    let (d, cfg) = rig();
    let t = now();
    record(&cfg, "web-1", Some(t + 7200), SAID, t).unwrap();
    // Refused again, the reset unchanged, then later with it moved on.
    let s = record(&cfg, "web-2", Some(t + 7200), SAID, t + 10).unwrap();
    assert_eq!((s.at, s.refused(), s.until), (t, t + 10, t + 7200));
    back(&cfg, Some(t + 5));
    back(&cfg, Some(t + 10));
    assert!(out(&cfg).is_some());
    record(&cfg, "web-3", Some(t + 9000), SAID, t + 20).unwrap();
    back(&cfg, Some(t + 15));
    assert!(out(&cfg).is_some());
    assert_eq!(told(&d).len(), 1);
    back(&cfg, Some(t + 21));
    assert_eq!(out(&cfg), None);
    // Its reset passed, no run having ended it, and a run is refused again:
    // out once more, told so, and only a run after that ends it.
    fs::remove_file(cfg.jobs.join(FILE)).unwrap();
    record(&cfg, "web-4", Some(t + 100), SAID, t).unwrap();
    let s = record(&cfg, "web-5", Some(t + 7200), SAID, t + 200).unwrap();
    assert_eq!((s.at, s.refused(), s.until), (t, t + 200, t + 7200));
    assert_eq!(told(&d).len(), 4, "{:?}", told(&d));
    assert!(told(&d)[3].starts_with("open proj-usage"));
    back(&cfg, Some(t + 150));
    assert!(kept(&cfg).unwrap().back.is_none());
    back(&cfg, Some(t + 201));
    assert!(kept(&cfg).unwrap().back.is_some());
}

/// A record from before `last` was kept has its first refusal stand for it.
#[test]
fn a_record_from_before_last() {
    let (_d, cfg) = rig();
    let t = now();
    fs::write(
        cfg.jobs.join(FILE),
        format!(
            r#"{{"until":{},"said":"x","job":"web-1","at":{t},"told":{t}}}"#,
            t + 60
        ),
    )
    .unwrap();
    assert_eq!(kept(&cfg).unwrap().refused(), t);
    back(&cfg, Some(t));
    assert!(out(&cfg).is_some());
    back(&cfg, Some(t + 1));
    assert_eq!(out(&cfg), None);
}

/// Only HOST-notify's exit 1 says the notice is closed: one that couldn't
/// run, was killed or answered anything else keeps the record, and the
/// close is sent again.
#[test]
fn only_pending_1_says_the_close_got_through() {
    let (d, cfg) = rig();
    let t = now();
    let gone = |c: &Config| {
        record(c, "web-1", Some(t + 7200), SAID, t).unwrap();
        back(c, None);
        let mut s = kept(c).unwrap();
        s.back = Some(t - c.stall_queued() - 1);
        s.told = t - RETELL;
        keep(c, &s).unwrap();
    };
    for (how, file) in [("exit 2", "pending-code"), ("killed", "pending-killed")] {
        gone(&cfg);
        fs::write(d.path().join(file), "2").unwrap();
        let before = told(&d).len();
        back(&cfg, Some(t + 1));
        assert!(kept(&cfg).is_some(), "{how}");
        assert_eq!(told(&d).len(), before + 1, "{how}");
        assert!(told(&d).last().unwrap().starts_with("close proj-usage"));
        fs::remove_file(d.path().join(file)).unwrap();
        back(&cfg, Some(t + 1));
        assert_eq!(kept(&cfg), None, "{how}");
    }
    // One that can't run at all.
    let mut missing = cfg.clone();
    missing.notify = Some(d.path().join("no-notify"));
    gone(&missing);
    back(&missing, Some(t + 1));
    assert!(kept(&missing).is_some());
}

/// With no run to ask, proj-watch's look tends the notice and books its
/// next look: the open again while it holds, the close until it got through,
/// and the record gone a stall after the release, then nothing to book.
#[test]
fn tended_with_no_run_to_ask() {
    let (d, cfg) = rig();
    let t = now();
    assert_eq!(tend(&cfg), None);
    record(&cfg, "web-1", Some(t + 7200), SAID, t).unwrap();
    let told_at = kept(&cfg).unwrap().told;
    assert_eq!(tend(&cfg), Some(told_at + RETELL));
    assert_eq!(told(&d).len(), 1);
    let mut s = kept(&cfg).unwrap();
    s.told = t - RETELL;
    keep(&cfg, &s).unwrap();
    let next = tend(&cfg).unwrap();
    assert_eq!(told(&d).len(), 2);
    assert!(next >= t + RETELL - 1, "{next}");
    // Ended; its close didn't get through.
    fs::write(d.path().join("pending"), "").unwrap();
    back(&cfg, None);
    assert_eq!(told(&d).len(), 3);
    let s = kept(&cfg).unwrap();
    assert_eq!(tend(&cfg), Some(s.told + RETELL / 12));
    assert_eq!(told(&d).len(), 3);
    let mut s = kept(&cfg).unwrap();
    s.told -= RETELL / 12;
    keep(&cfg, &s).unwrap();
    tend(&cfg);
    assert_eq!(told(&d).len(), 4);
    assert!(told(&d)[3].starts_with("close proj-usage"));
    // Through: kept till a stall after the release, then gone.
    fs::remove_file(d.path().join("pending")).unwrap();
    let s = kept(&cfg).unwrap();
    assert_eq!(tend(&cfg), Some(s.release() + cfg.stall_queued()));
    let mut s = kept(&cfg).unwrap();
    s.back = Some(t - cfg.stall_queued() - 1);
    keep(&cfg, &s).unwrap();
    assert_eq!(tend(&cfg), None);
    assert_eq!(kept(&cfg), None);
    // Past its reset with no run yet to end it: nothing to send till one does.
    record(&cfg, "web-2", Some(t + 60), SAID, t).unwrap();
    let mut s = kept(&cfg).unwrap();
    s.until = t - 1;
    keep(&cfg, &s).unwrap();
    let before = told(&d).len();
    assert_eq!(tend(&cfg), None);
    assert_eq!(told(&d).len(), before);
}

/// Each change of hold wakes the stall watchdog and proj-watch, whether or
/// not any turn's wait changed (none is watched here); a refusal that moves
/// nothing on wakes neither.
#[test]
fn every_change_kicks_the_watchdog_and_proj_watch() {
    let (d, cfg) = rig();
    let t = now();
    let both = ["/kick/proj-watch".to_owned(), "/kick/watchdog".to_owned()];
    record(&cfg, "web-1", Some(t + 7200), SAID, t).unwrap();
    assert_eq!(kicked(&d), both);
    record(&cfg, "web-2", Some(t + 7200), SAID, t + 1).unwrap();
    assert_eq!(kicked(&d), Vec::<String>::new());
    record(&cfg, "web-3", Some(t + 9000), SAID, t + 2).unwrap();
    assert_eq!(kicked(&d), both);
    back(&cfg, None);
    assert_eq!(kicked(&d), both);
    // Without the watchdog, only proj-watch.
    let mut alone = cfg.clone();
    alone.waits = None;
    fs::remove_file(cfg.jobs.join(FILE)).unwrap();
    record(&alone, "web-4", Some(t + 7200), SAID, t).unwrap();
    assert_eq!(kicked(&d), ["/kick/proj-watch".to_owned()]);
}

/// What #428's proj wrote of a usage limit, N.resets for a job's latest
/// turn (an --owned one's too), is taken into the hold once its turn has
/// ended, and removed once the hold has it; an earlier turn's is no longer
/// the job's, one already past holds nothing, nor does one written before a
/// release ended the hold it came from.
#[test]
fn an_earlier_projs_resets_are_the_hold_now() {
    let (_d, cfg) = rig();
    let t = now();
    let job = |id: &str, turns: &str, ended: bool, files: &[(&str, i64)]| {
        let dir = cfg.jobs.join(id);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("turns"), format!("{turns}\n")).unwrap();
        if ended {
            fs::write(dir.join(format!("{turns}.outcome")), "error\n").unwrap();
        }
        for (n, at) in files {
            fs::write(dir.join(format!("{n}.resets")), format!("{at}\n")).unwrap();
        }
    };
    job("web-1", "2", true, &[("1", t + 9000), ("2", t + 3600)]);
    job("owned-1", "1", true, &[("1", t + 5400)]);
    fs::write(cfg.jobs.join("owned-1/owned"), "").unwrap();
    job("web-2", "1", true, &[("1", t - 60)]);
    // Its turn still going: maybe half written, so left for later.
    job("web-3", "1", false, &[("1", t + 9999)]);
    adopt(&cfg).unwrap();
    let s = out(&cfg).unwrap();
    assert_eq!((s.until, s.said.as_str()), (t + 5400, EARLIER));
    for gone in ["web-1/2.resets", "owned-1/1.resets", "web-2/1.resets"] {
        assert!(!cfg.jobs.join(gone).exists(), "{gone}");
    }
    assert!(cfg.jobs.join("web-1/1.resets").exists());
    assert!(cfg.jobs.join("web-3/1.resets").exists());
    adopt(&cfg).unwrap();
    assert_eq!(out(&cfg).unwrap().until, t + 5400);
    // Released early (`proj usage back`): web-3's, written before that, ends
    // up taken and dropped, never holding again.
    back(&cfg, None);
    let mut released = kept(&cfg).unwrap();
    released.back = Some(t - 3);
    keep(&cfg, &released).unwrap();
    fs::write(cfg.jobs.join(RELEASED), format!("{}\n", t - 3)).unwrap();
    fs::write(cfg.jobs.join("web-3/1.outcome"), "error\n").unwrap();
    let past = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs((t - 5) as u64);
    fs::File::options()
        .write(true)
        .open(cfg.jobs.join("web-3/1.resets"))
        .unwrap()
        .set_modified(past)
        .unwrap();
    adopt(&cfg).unwrap();
    assert_eq!(out(&cfg), None);
    assert!(!cfg.jobs.join("web-3/1.resets").exists());
    // One written since the release is a hold of its own.
    job("web-4", "1", true, &[("1", t + 7200)]);
    adopt(&cfg).unwrap();
    assert_eq!(out(&cfg).unwrap().until, t + 7200);
}

/// A hold that can't be written isn't one: the record it would replace
/// stays where it was, and so does the caller's own.
#[test]
fn a_hold_that_cant_be_written_takes_nothing() {
    let (_d, cfg) = rig();
    let t = now();
    let dir = cfg.jobs.join("web-1");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("turns"), "1\n").unwrap();
    fs::write(dir.join("1.outcome"), "error\n").unwrap();
    fs::write(dir.join("1.resets"), format!("{}\n", t + 3600)).unwrap();
    // The lock exists already; the record can't be: the jobs dir is shut.
    fs::write(cfg.jobs.join(".usage.lock"), "").unwrap();
    burrow_test::permissions(&cfg.jobs, 0o555);
    let adopted = adopt(&cfg);
    let held = earlier(&cfg, "the stall watchdog", t + 3600, Some(t));
    let recorded = record(&cfg, "web-2", Some(t + 3600), SAID, t);
    burrow_test::permissions(&cfg.jobs, 0o755);
    assert!(adopted.is_err() && held.is_err() && recorded.is_err());
    assert!(dir.join("1.resets").exists());
    assert_eq!(kept(&cfg), None);
}

/// An earlier proj's limit counts its refusal from when it was written, not
/// from when it's taken: a run started since then ends it, one started
/// before doesn't. Taken again, it changes nothing. #428's watchdog's says
/// no time: it counts from 0, so any run ends it.
#[test]
fn an_earlier_limit_counts_from_when_it_was_written() {
    let (_d, cfg) = rig();
    let t = now();
    earlier(&cfg, "web-1", t + 3600, Some(t - 600)).unwrap();
    let s = kept(&cfg).unwrap();
    assert_eq!((s.until, s.refused()), (t + 3600, t - 600));
    earlier(&cfg, "web-1", t + 3600, Some(t - 600)).unwrap();
    assert_eq!(kept(&cfg).unwrap(), s);
    back(&cfg, Some(t - 601));
    assert!(out(&cfg).is_some());
    back(&cfg, Some(t - 300));
    assert_eq!(out(&cfg), None);

    let (_d, cfg) = rig();
    earlier(&cfg, "the stall watchdog", t + 3600, None).unwrap();
    assert_eq!(kept(&cfg).unwrap().refused(), 0);
    earlier(&cfg, "the stall watchdog", t + 3600, None).unwrap();
    assert_eq!(kept(&cfg).unwrap().refused(), 0);
    back(&cfg, Some(1));
    assert_eq!(out(&cfg), None);
    // A live refusal on top of one taken keeps the latest refusal.
    let (_d, cfg) = rig();
    record(&cfg, "web-2", Some(t + 3600), SAID, t).unwrap();
    earlier(&cfg, "web-1", t + 7200, Some(t - 600)).unwrap();
    let s = kept(&cfg).unwrap();
    assert_eq!((s.until, s.refused()), (t + 7200, t));
}

/// A release outlives its record: once the record is gone (its notice
/// settled) or replaced by a new refusal, an earlier proj's limit written
/// before the release, an N.resets, a retained await's or the watchdog's,
/// still never holds again; one written after it does.
#[test]
fn lifecycle_can_read_actual_release_past_the_notice() {
    let (_dir, cfg) = rig();
    let t = now();
    assert_eq!(last_release(&cfg), None);
    record(&cfg, "web-1", Some(t + 7200), SAID, t - 100).unwrap();
    assert_eq!(last_release(&cfg), None);
    back(&cfg, Some(t - 50));
    let actual = kept(&cfg).unwrap().release();
    assert_eq!(last_release(&cfg), Some(actual));
    fs::remove_file(cfg.jobs.join(FILE)).unwrap();
    assert_eq!(last_release(&cfg), Some(actual));
}

#[test]
fn a_release_outlives_its_record() {
    let (d, cfg) = rig();
    let t = now();
    record(&cfg, "web-1", Some(t + 7200), SAID, t - 100).unwrap();
    back(&cfg, Some(t - 50));
    assert_eq!(released(&cfg).unwrap(), kept(&cfg).unwrap().back.unwrap());
    // Settled: the record goes a stall after the release.
    let mut s = kept(&cfg).unwrap();
    s.back = Some(t - cfg.stall_queued() - 1);
    keep(&cfg, &s).unwrap();
    assert!(!d.path().join("pending").exists());
    assert_eq!(tend(&cfg), None);
    assert_eq!(kept(&cfg), None);
    earlier(&cfg, "web-1", t + 7200, Some(t - 100)).unwrap();
    earlier(&cfg, "the stall watchdog", t + 7200, None).unwrap();
    let dir = cfg.jobs.join("web-1");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("turns"), "1\n").unwrap();
    fs::write(dir.join("1.outcome"), "error\n").unwrap();
    fs::write(dir.join("1.resets"), format!("{}\n", t + 7200)).unwrap();
    let past = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs((t - 100) as u64);
    fs::File::options()
        .write(true)
        .open(dir.join("1.resets"))
        .unwrap()
        .set_modified(past)
        .unwrap();
    adopt(&cfg).unwrap();
    assert_eq!(kept(&cfg), None);
    // Replaced by a new refusal: the old ones don't move its reset on.
    record(&cfg, "web-2", Some(t + 600), SAID, t).unwrap();
    earlier(&cfg, "web-1", t + 7200, Some(t - 100)).unwrap();
    earlier(&cfg, "the stall watchdog", t + 7200, None).unwrap();
    assert_eq!(out(&cfg).unwrap().until, t + 600);
    // One written after the release is a hold of its own.
    earlier(&cfg, "web-3", t + 7200, Some(t + 60)).unwrap();
    assert_eq!(out(&cfg).unwrap().until, t + 7200);
}

/// A notice that can't be tended (the lock not had, the record unreadable)
/// is booked again in RETRY, never sooner and never forgotten, and tended
/// once it can be.
#[test]
fn a_look_that_cant_tend_books_another() {
    let (d, cfg) = rig();
    let t = now();
    record(&cfg, "web-1", Some(t + 7200), SAID, t).unwrap();
    fs::write(d.path().join("pending"), "").unwrap();
    back(&cfg, None);
    let mut s = kept(&cfg).unwrap();
    s.told -= RETELL;
    keep(&cfg, &s).unwrap();
    let before = told(&d).len();
    // No lock to be had: the jobs dir is shut, and the lock not there yet.
    let _ = fs::remove_file(cfg.jobs.join(".usage.lock"));
    burrow_test::permissions(&cfg.jobs, 0o555);
    let next = tend(&cfg);
    burrow_test::permissions(&cfg.jobs, 0o755);
    assert!(next.is_some_and(|n| n >= t + RETRY), "{next:?}");
    assert_eq!(told(&d).len(), before);
    tend(&cfg);
    assert_eq!(told(&d).len(), before + 1);
    // Holding, its open due and the lock not had: not booked in the past.
    let mut s = kept(&cfg).unwrap();
    s.back = None;
    s.until = t + 7200;
    s.told = t - RETELL;
    keep(&cfg, &s).unwrap();
    let _ = fs::remove_file(cfg.jobs.join(".usage.lock"));
    burrow_test::permissions(&cfg.jobs, 0o555);
    let next = tend(&cfg);
    burrow_test::permissions(&cfg.jobs, 0o755);
    assert!(next.is_some_and(|n| n >= t + RETRY), "{next:?}");
    // An unreadable record: looked at again too.
    fs::write(cfg.jobs.join(FILE), "{").unwrap();
    assert!(tend(&cfg).is_some_and(|n| n >= t + RETRY));
}

/// Job `id`'s latest turn ended on a usage limit an earlier proj wrote,
/// resetting at `reset`, its N.resets written at `written`; `owned` for an
/// --owned job's.
pub(super) fn earlier_resets(cfg: &Config, id: &str, reset: i64, written: i64, owned: bool) {
    let dir = cfg.jobs.join(id);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("turns"), "1\n").unwrap();
    fs::write(dir.join("1.outcome"), "error\n").unwrap();
    if owned {
        fs::write(dir.join("owned"), "").unwrap();
    }
    fs::write(dir.join("1.resets"), format!("{reset}\n")).unwrap();
    let at = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(written as u64);
    fs::File::options()
        .write(true)
        .open(dir.join("1.resets"))
        .unwrap()
        .set_modified(at)
        .unwrap();
}

/// The owner's release (`proj usage back`) with no record to end, the
/// machine never out by this proj's lights (an earlier proj's limits still
/// to be taken in), is kept all the same: every limit written before it
/// (an owned or unowned job's N.resets, a usage-limit await's, the
/// watchdog's, which says no time) never holds; one written after it, or a
/// run refused after it, does. The watchdog and proj-watch are kicked, to
/// drop the ones they keep.
#[test]
fn the_owners_release_with_no_record_still_ends_earlier_limits() {
    let (d, cfg) = rig();
    let t = now();
    assert_eq!(released(&cfg).unwrap(), 0);
    back(&cfg, None);
    assert!(released(&cfg).unwrap() >= t);
    assert_eq!(kept(&cfg), None);
    assert_eq!(kicked(&d), ["/kick/proj-watch", "/kick/watchdog"]);
    // A run that wasn't refused, with no hold, keeps nothing.
    let (_d2, other) = rig();
    back(&other, Some(t));
    assert_eq!(released(&other).unwrap(), 0);

    earlier_resets(&cfg, "web-1", t + 7200, t - 100, false);
    earlier_resets(&cfg, "owned-1", t + 7200, t - 100, true);
    adopt(&cfg).unwrap();
    for f in ["web-1/1.resets", "owned-1/1.resets"] {
        assert!(!cfg.jobs.join(f).exists(), "{f}");
    }
    earlier(&cfg, "web-2", t + 7200, Some(t - 100)).unwrap();
    earlier(&cfg, "the stall watchdog", t + 7200, None).unwrap();
    assert_eq!(kept(&cfg), None);
    assert_eq!(out(&cfg), None);
    // Written since: a hold of its own, each.
    earlier_resets(&cfg, "web-3", t + 3600, t + 60, false);
    adopt(&cfg).unwrap();
    assert_eq!(out(&cfg).unwrap().until, t + 3600);
    let (_d, cfg) = rig();
    back(&cfg, None);
    earlier(&cfg, "web-4", t + 3600, Some(t + 60)).unwrap();
    assert_eq!(out(&cfg).unwrap().until, t + 3600);
    let (_d, cfg) = rig();
    back(&cfg, None);
    record(&cfg, "web-5", Some(t + 3600), SAID, t + 1).unwrap();
    assert_eq!(out(&cfg).unwrap().until, t + 3600);
}

/// The owner's release once the hold has ended already (its record kept,
/// its notice still to settle) is a release of its own, later than the
/// hold's: a limit written between the two never holds; one written after
/// it does.
#[test]
fn the_owners_release_after_a_settled_hold_moves_the_release_on() {
    let (d, cfg) = rig();
    let t = now();
    record(&cfg, "web-1", Some(t + 7200), SAID, t - 1000).unwrap();
    back(&cfg, Some(t - 900));
    let mut s = kept(&cfg).unwrap();
    s.back = Some(t - 800);
    keep(&cfg, &s).unwrap();
    fs::write(cfg.jobs.join(RELEASED), format!("{}\n", t - 800)).unwrap();
    fs::write(d.path().join("pending"), "").unwrap();
    kicked(&d);
    back(&cfg, None);
    assert!(released(&cfg).unwrap() >= t);
    assert_eq!(kept(&cfg).unwrap().back, Some(t - 800));
    assert_eq!(kicked(&d), ["/kick/proj-watch", "/kick/watchdog"]);
    earlier_resets(&cfg, "web-2", t + 7200, t - 100, false);
    earlier_resets(&cfg, "owned-2", t + 7200, t - 100, true);
    adopt(&cfg).unwrap();
    earlier(&cfg, "web-3", t + 7200, Some(t - 100)).unwrap();
    earlier(&cfg, "the stall watchdog", t + 7200, None).unwrap();
    assert_eq!(out(&cfg), None);
    earlier(&cfg, "web-4", t + 3600, Some(t + 60)).unwrap();
    assert_eq!(out(&cfg).unwrap().until, t + 3600);
}

#[test]
fn unpaid_usage_is_a_wait_not_a_billing_question() {
    let mut cfg: crate::config::Config =
        serde_json::from_value(crate::config::tests::example()).unwrap();
    assert!(!cfg.usage_paid);
    assert!(super::policy(&cfg).contains("never ask the owner"));
    cfg.usage_paid = true;
    assert!(!super::policy(&cfg).contains("disabled"));
}

#[test]
fn old_holds_are_normalized_for_status_reminders_and_extensions() {
    let (d, cfg) = rig();
    let t = now();
    let raw = "You've hit your monthly spend limit · raise it at https://claude.ai/settings/usage · your weekly limit resets Nov 4, 12pm (America/Los_Angeles)";
    let original = Spent {
        owner: false,
        until: t + 7200,
        said: raw.into(),
        job: "web-1".into(),
        at: t - 10,
        back: None,
        admission: None,
        told: t - RETELL,
        last: Some(t - 5),
    };
    // Simulate the previous binary's disk record, bypassing record().
    fs::write(
        cfg.jobs.join(FILE),
        serde_json::to_string(&original).unwrap(),
    )
    .unwrap();
    let mut expected = original.clone();
    expected.said = "Claude reached its weekly allowance".into();
    assert_eq!(kept(&cfg), Some(expected.clone()));
    assert_eq!(out(&cfg), Some(expected.clone()));
    assert_eq!(expected.release(), original.release());
    assert!(line(&expected).contains("weekly allowance"));
    assert!(!line(&expected).contains("monthly spend limit"));
    let capture = d.path().join("notice-body");
    fs::write(
        cfg.notify.as_ref().unwrap(),
        format!("#!/bin/sh\nprintf '%s' \"$4\" >'{}'\n", capture.display()),
    )
    .unwrap();
    remind(&cfg);
    let notice = fs::read_to_string(&capture).unwrap();
    assert!(notice.contains("weekly allowance"));
    assert!(notice.contains("never ask the owner"));
    assert!(!notice.contains("https://claude.ai/settings/usage"));
    assert!(!notice.contains("monthly spend limit"));
    let extended = record(
        &cfg,
        "web-2",
        Some(t + 9000),
        "Claude reached its weekly allowance",
        t,
    )
    .unwrap();
    assert_eq!(extended.said, expected.said);
    assert_eq!(
        (extended.until, extended.at, extended.last, extended.back),
        (t + 9000, original.at, Some(t), None)
    );
    assert_eq!(extended.job, original.job);
    assert!(
        !fs::read_to_string(cfg.jobs.join(FILE))
            .unwrap()
            .contains("https://")
    );
    // Released historical holds retain release bookkeeping as well.
    let mut released = original;
    released.back = Some(t - 1);
    fs::write(
        cfg.jobs.join(FILE),
        serde_json::to_string(&released).unwrap(),
    )
    .unwrap();
    let loaded = kept(&cfg).unwrap();
    assert_eq!(
        (loaded.back, loaded.release(), loaded.last),
        (released.back, released.release(), released.last)
    );
    // Direct callers of record() cannot introduce the old instructions either.
    let fresh = record(&cfg, "web-3", Some(t + 10000), raw, t).unwrap();
    assert_eq!(fresh.said, expected.said);
}

mod admission;
#[test]
fn scheduling_releases_only_a_fresh_meter_after_the_refusal() {
    use burrow_usage_reset::meter::{FRESH, Snapshot, Window};
    let (d, mut cfg) = rig();
    cfg.meters = Some(d.path().join("meters"));
    cfg.pipeline_slots = 0;
    cfg.min_free_memory = 0;
    let t = now();
    record(&cfg, "held", Some(t + 7200), SAID, t - FRESH as i64 - 100).unwrap();
    let mut meter = Snapshot {
        windows: [
            (
                "seven_day".into(),
                Window {
                    used: 1.0,
                    resets_at: 0.0,
                },
            ),
            (
                "five_hour".into(),
                Window {
                    used: 6.0,
                    resets_at: 0.0,
                },
            ),
        ]
        .into(),
        credits: false,
        read_at: t as f64,
        requested_at: Some(t as f64),
    };
    // A stale reading after the hold began still proves nothing.
    meter.read_at = t as f64 - FRESH - 1.0;
    meter.requested_at = Some(meter.read_at);
    meter
        .record(cfg.meters.as_ref().unwrap(), "claude")
        .unwrap();
    assert!(!crate::slot::room(&cfg, crate::engine::Engine::Claude));
    // A later refusal supersedes the original hold's start.
    record(&cfg, "held", Some(t + 7200), SAID, t - 10).unwrap();
    for at in [(t - 11) as f64, (t - 10) as f64, (t - 10) as f64 + 0.2] {
        meter.read_at = at;
        meter.requested_at = Some(at);
        meter
            .record(cfg.meters.as_ref().unwrap(), "claude")
            .unwrap();
        assert!(!crate::slot::room(&cfg, crate::engine::Engine::Claude));
        assert!(out(&cfg).is_some());
        assert_eq!(told(&d).len(), 1);
    }
    meter.read_at = t as f64;
    meter.requested_at = Some(meter.read_at);
    // Every window must have room, and another engine cannot release it.
    meter.windows.get_mut("five_hour").unwrap().used = 100.0;
    meter
        .record(cfg.meters.as_ref().unwrap(), "claude")
        .unwrap();
    assert!(!crate::slot::room(&cfg, crate::engine::Engine::Claude));
    meter.windows.get_mut("five_hour").unwrap().used = 6.0;
    meter.record(cfg.meters.as_ref().unwrap(), "codex").unwrap();
    assert!(!crate::slot::room(&cfg, crate::engine::Engine::Claude));
    meter
        .record(cfg.meters.as_ref().unwrap(), "claude")
        .unwrap();
    assert!(
        crate::slot::room(&cfg, crate::engine::Engine::Claude),
        "next scheduling pass releases the held queue"
    );
    let job = Job::new(&cfg, "held");
    fs::create_dir_all(&job.dir).unwrap();
    let _slot = crate::slot::take(&cfg, "held", "1", crate::pick::Class::Normal, false).unwrap();
    assert!(job.turn("1", "slot").exists());
    assert!(out(&cfg).is_none());
    assert!(told(&d).last().unwrap().starts_with("close proj-usage"));
}

#[test]
fn a_fractional_meter_in_the_initial_refusal_second_keeps_the_hold() {
    let (d, mut cfg) = rig();
    cfg.meters = Some(d.path().join("meters"));
    let t = now() - 10;
    record(&cfg, "held", Some(t + 7200), SAID, t).unwrap();
    fs::create_dir_all(cfg.meters.as_ref().unwrap()).unwrap();
    fs::write(
        cfg.meters.as_ref().unwrap().join("claude.json"),
        json!({
            "windows": {"seven_day": {"used": 1.0, "resets_at": 0.0}},
            "credits": false, "read_at": t as f64 + 0.2, "requested_at": t as f64 + 0.2
        })
        .to_string(),
    )
    .unwrap();
    tend(&cfg);
    assert!(out(&cfg).is_some(), "same-second ordering is ambiguous");
    assert_eq!(told(&d).len(), 1);
}

#[test]
fn refusal_between_meter_check_and_locked_release_keeps_the_hold() {
    let (d, mut cfg) = rig();
    cfg.meters = Some(d.path().join("meters"));
    let t = now() - 10;
    record(&cfg, "held", Some(t + 7200), SAID, t).unwrap();
    fs::create_dir_all(cfg.meters.as_ref().unwrap()).unwrap();
    fs::write(
        cfg.meters.as_ref().unwrap().join("claude.json"),
        json!({
            "windows": {"seven_day": {"used": 1.0, "resets_at": 0.0}},
            "credits": false, "read_at": (t + 5) as f64 + 0.2, "requested_at": (t + 5) as f64 + 0.2
        })
        .to_string(),
    )
    .unwrap();
    let started = meter_start(&cfg).unwrap();
    // Another turn is refused after the initial check but before back locks.
    record(&cfg, "other", Some(t + 7200), SAID, t + 5).unwrap();
    back(&cfg, Some(started));
    assert!(out(&cfg).is_some());
    assert_eq!(told(&d).len(), 1);
}

#[test]
fn a_completed_read_does_not_prove_when_its_request_began() {
    let (d, mut cfg) = rig();
    cfg.meters = Some(d.path().join("meters"));
    let t = now();
    record(&cfg, "held", Some(t + 7200), SAID, t - 10).unwrap();
    fs::create_dir_all(cfg.meters.as_ref().unwrap()).unwrap();
    for requested in [
        None,
        Some((t - 11) as f64),
        Some(t as f64 - 3601.0),
        Some((t + 1) as f64),
    ] {
        // Older producer, in-flight request, stale request, or invalid ordering.
        fs::write(
            cfg.meters.as_ref().unwrap().join("claude.json"),
            json!({
                "windows": {"seven_day": {"used": 1.0, "resets_at": 0.0}},
                "credits": false, "read_at": t, "requested_at": requested
            })
            .to_string(),
        )
        .unwrap();
        tend(&cfg);
        assert!(out(&cfg).is_some());
        assert_eq!(told(&d).len(), 1);
    }
}
