//! A record's check run (Conary's `docs/m6-plugins.md`, "Who produces
//! what" and "Restart and cancellation"): started in progress, then
//! completed once, either with the record's signed result as its output
//! text, or as `cancelled` with nothing when its work is abandoned or never
//! finished. Startup does the same for whatever a crash left
//! ([`reconcile`]). It goes out through the machine's GitHub door
//! ([`burrow_github::Api`]); that App isn't the trust root, the signature
//! is.
use crate::records::{End, Record, Records};
use crate::serve::Work;
use crate::sign::{Signable, Signed, Signer};
use burrow_github::{Api, call};
use serde::Serialize;
use serde_json::{Value, json};

/// Start `record`'s check run, in progress, carrying the record's id as its
/// `external_id`, so startup can find it again after a crash.
pub fn start<T: Serialize + Clone>(
    api: &mut dyn Api,
    records: &Records,
    record: &mut Record<T>,
) -> Result<(), String> {
    if record.run().is_some() || record.end().is_some() {
        return Err(format!("record {} has started its run", record.id()));
    }
    let target = record.target();
    let body = json!({"name": target.check, "head_sha": target.head,
        "status": "in_progress", "external_id": record.id()});
    let got = call(
        api,
        "POST",
        &format!("repos/{}/check-runs", target.repo),
        Some(&body),
        None,
    )?;
    let id = got.json()?["id"]
        .as_u64()
        .ok_or("GitHub's answer names no check run")?;
    records.started(record, id)
}

/// Publish `record`'s result on its check run, signed, and end it as
/// published. If `work` was abandoned and the record isn't committed, or
/// it holds no result, it's withdrawn instead, and that's an error.
pub fn complete<T: Signable>(
    api: &mut dyn Api,
    records: &Records,
    signer: &Signer,
    work: &Work,
    record: &mut Record<T>,
) -> Result<Signed, String> {
    finish(api, records, signer, work.abandoned(), record)
}

/// Publish committed `record`'s result on its check run, signed, with no
/// request's work behind it: a result its producer publishes on its own,
/// after the request that made it has ended (the review producer signs a
/// review once the flow has published it). Only a committed record: one
/// that isn't could still be withdrawn.
pub fn publish<T: Signable>(
    api: &mut dyn Api,
    records: &Records,
    signer: &Signer,
    record: &mut Record<T>,
) -> Result<Signed, String> {
    if !record.committed() {
        return Err(format!(
            "record {} isn't committed, so it isn't published on its own",
            record.id()
        ));
    }
    finish(api, records, signer, false, record)
}

/// Withdraw `record`: complete its check run, if it started one, as
/// `cancelled`, and end it so that nothing ever publishes it. A committed
/// record is never withdrawn: what went out stands, and startup publishes
/// it.
pub fn withdraw<T: Serialize + Clone>(
    api: &mut dyn Api,
    records: &Records,
    record: &mut Record<T>,
) -> Result<(), String> {
    if record.committed() || record.end() == Some(End::Published) {
        return Err(format!(
            "record {} is committed, so it's never withdrawn",
            record.id()
        ));
    }
    if let Some(run) = record.run() {
        patch(
            api,
            &record.target().repo,
            run,
            json!({"status": "completed", "conclusion": "cancelled",
                "output": {"title": "Cancelled",
                    "summary": "Its work stopped before it finished, so it says nothing."}}),
        )?;
    }
    records.ended(record, End::Withdrawn)
}

/// At startup, finish every record a crash left: a committed one is
/// published from what it holds, and any other is withdrawn. No work runs
/// again. Every record is tried; the errors come back together, and their
/// records are left for the next start.
pub fn reconcile<T: Signable>(
    api: &mut dyn Api,
    records: &Records,
    signer: &Signer,
) -> Result<usize, String> {
    reconcile_with::<T>(api, records, signer, &mut |_, _| Ok(()))
}

/// What [`reconcile_with`] finishes for a committed record before its
/// check run.
pub type Before<'a, T> = dyn FnMut(&mut dyn Api, &Record<T>) -> Result<(), String> + 'a;

/// [`reconcile`], finishing first, for each committed record, what went
/// out before its check run (`before`: a judgment's sealed comment, posted
/// if a crash kept it from going out, and left alone if it's there), and
/// only then its check run. A record whose `before` fails is left for the
/// next start.
pub fn reconcile_with<T: Signable>(
    api: &mut dyn Api,
    records: &Records,
    signer: &Signer,
    before: &mut Before<'_, T>,
) -> Result<usize, String> {
    let mut finished = 0;
    let mut failed = Vec::new();
    for mut record in records.unfinished::<T>().map_err(|e| e.to_string())? {
        let done = found(api, records, &mut record).and_then(|()| {
            if record.committed() {
                before(api, &record)?;
                finish(api, records, signer, false, &mut record).map(drop)
            } else {
                withdraw(api, records, &mut record)
            }
        });
        match done {
            Ok(()) => finished += 1,
            Err(e) => failed.push(format!("{}: {e}", record.id())),
        }
    }
    if failed.is_empty() {
        Ok(finished)
    } else {
        Err(failed.join("; "))
    }
}

pub(crate) fn finish<T: Signable>(
    api: &mut dyn Api,
    records: &Records,
    signer: &Signer,
    abandoned: bool,
    record: &mut Record<T>,
) -> Result<Signed, String> {
    if record.end().is_some() {
        return Err(format!("record {} has ended", record.id()));
    }
    if abandoned && !record.committed() {
        withdraw(api, records, record)?;
        return Err("its work was abandoned, so it publishes nothing".into());
    }
    let signed = match signer.signed(record) {
        Ok(signed) if signed.check() == record.target().check => signed,
        _ if record.committed() => {
            return Err(format!(
                "record {} is committed but can't be signed; startup tries again",
                record.id()
            ));
        }
        _ => {
            withdraw(api, records, record)?;
            return Err(format!("record {} holds nothing to publish", record.id()));
        }
    };
    // The point of no return, before anything goes out: a crash after it
    // is finished at startup by publishing the same result again (signing
    // is deterministic), never by withdrawing what may already be out.
    if !record.committed() {
        records.commit(record)?;
    }
    if record.run().is_none() {
        start(api, records, record)?;
    }
    let run = record.run().ok_or("a started run with no id")?;
    patch(
        api,
        &record.target().repo,
        run,
        json!({"status": "completed", "conclusion": signed.conclusion(),
            "output": {"title": signed.check(), "summary": "A producer's signed result.",
                "text": signed.text()}}),
    )?;
    records.ended(record, End::Published)?;
    Ok(signed)
}

/// Find the run a crash may have started for `record` before it kept the
/// run's id: by its `external_id`, among the runs on its head.
fn found<T: Serialize + Clone>(
    api: &mut dyn Api,
    records: &Records,
    record: &mut Record<T>,
) -> Result<(), String> {
    if record.run().is_some() {
        return Ok(());
    }
    let target = record.target();
    let path = format!(
        "repos/{}/commits/{}/check-runs?check_name={}&filter=all&per_page=100",
        target.repo,
        target.head,
        burrow_github::encode(&target.check)
    );
    let pages = burrow_github::pages(api, &path)?;
    let run = pages
        .iter()
        .flat_map(|p| p["check_runs"].as_array().cloned().unwrap_or_default())
        .find(|r| r["external_id"] == record.id() && r["name"] == target.check.as_str())
        .and_then(|r| r["id"].as_u64());
    match run {
        Some(id) => records.started(record, id),
        None => Ok(()),
    }
}

fn patch(api: &mut dyn Api, repo: &str, run: u64, body: Value) -> Result<(), String> {
    call(
        api,
        "PATCH",
        &format!("repos/{repo}/check-runs/{run}"),
        Some(&body),
        None,
    )?;
    Ok(())
}
