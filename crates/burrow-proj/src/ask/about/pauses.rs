//! Paused time is credited with the deadline, in the same atomic record.
//! Intervals make overlapping members and interrupted retries idempotent;
//! member receipts travel across moves without replacing the selected policy.
use super::*;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct Pauses(Vec<(i64, i64)>);

impl Pauses {
    pub(super) fn inherit(&mut self, other: &Self) {
        self.0.extend_from_slice(&other.0);
        self.normalize();
    }

    fn normalize(&mut self) {
        self.0.sort_unstable();
        let mut spans: Vec<(i64, i64)> = Vec::new();
        for &(start, end) in &self.0 {
            if end <= start {
                continue;
            }
            if let Some(last) = spans.last_mut()
                && start <= last.1
            {
                last.1 = last.1.max(end);
            } else {
                spans.push((start, end));
            }
        }
        self.0 = spans;
    }

    fn seconds(&self) -> i64 {
        self.0.iter().fold(0i64, |sum, (start, end)| {
            sum.saturating_add(end.saturating_sub(*start))
        })
    }

    /// Credit only time this member hasn't already received elsewhere.
    /// Existing group coverage then deduplicates overlapping paused members.
    fn credit(&mut self, start: i64, end: i64, elsewhere: &Self) -> i64 {
        let before = self.seconds();
        let mut cursor = start;
        for &(a, b) in &elsewhere.0 {
            if a > cursor {
                self.0.push((cursor, a.min(end)));
            }
            cursor = cursor.max(b);
            if cursor >= end {
                break;
            }
        }
        if cursor < end {
            self.0.push((cursor, end));
        }
        self.normalize();
        self.seconds().saturating_sub(before)
    }
}

pub(crate) fn restore(job: &Job, since: i64, until: i64) -> Result<(), String> {
    let _lock = locked(job.cfg)?;
    let Some(mut q) = load(job)? else {
        return Ok(());
    };
    let mut state = State::load(job.cfg)?;
    if let Some(key) = state.standing(job)?.map(|(key, _)| key.to_owned()) {
        let group = state.0.get_mut(&key).expect("standing names a group");
        let member = group
            .members
            .iter_mut()
            .find(|m| m.id == job.id)
            .expect("standing names a member");
        let delta = group.pauses.credit(since, until, &member.pauses);
        group.due = group.due.saturating_add(delta);
        member.pauses.0.push((since, until));
        member.pauses.normalize();
        return state.save(job.cfg);
    }
    let delta = q.pauses.credit(since, until, &Pauses::default());
    q.due = q.due.saturating_add(delta);
    swap(
        &host_path(job, "question"),
        &serde_json::to_string(&q).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}
