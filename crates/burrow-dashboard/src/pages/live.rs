//! The live view: one snapshot of proj's state ([`burrow_proj::snapshot`]),
//! rendered whole, in this order: the front item's path, the slots, the
//! queue, deploy, meters, holds. Fields come from the snapshot's types,
//! whose schema is `crates/burrow-proj/state.schema.json`; nothing here
//! reads anything else. Live updates are not this page's job.

use burrow_proj::snapshot::{
    Holder, Holds, Meter, Priority, Ready, Slot, SlotKind, SlotState, Snapshot, Window,
};
use maud::{Markup, html};

use super::{Frame, ago_secs, page, span};

/// A slot in this state is taken and doing nothing (or waiting on a
/// person), so it stands out beside the busy ones.
fn stalled(s: SlotState) -> bool {
    matches!(
        s,
        SlotState::Parked | SlotState::Asking | SlotState::Ended | SlotState::Unknown
    )
}

fn state_word(s: SlotState) -> &'static str {
    match s {
        SlotState::Idle => "idle",
        SlotState::Running => "running",
        SlotState::Queued => "queued",
        SlotState::Parked => "parked",
        SlotState::Asking => "asking",
        SlotState::Ended => "ended",
        SlotState::Starting => "starting",
        SlotState::Busy => "busy",
        SlotState::Unknown => "unknown",
    }
}

fn kind_word(k: SlotKind) -> &'static str {
    match k {
        SlotKind::Claude => "claude",
        SlotKind::Unstick => "unstick",
        SlotKind::Codex => "codex",
        SlotKind::Drain => "drain",
        SlotKind::Sorting => "sorting",
        SlotKind::Heavy => "heavy",
    }
}

fn priority_word(p: Priority) -> &'static str {
    match p {
        Priority::High => "high",
        Priority::Medium => "medium",
        Priority::Low => "low",
    }
}

/// Why a queue item sits where it does: the fields the drain ranks by.
fn why(r: &Ready, place: usize) -> String {
    let mut parts = vec![format!("{} priority", priority_word(r.priority))];
    if r.pipeline {
        parts.push("in a pipeline repo, which goes ahead of the rest".into());
    }
    if r.repair {
        parts.push("a repair child, which takes no ordinary drain slot".into());
    }
    if place == 0 {
        parts.insert(0, "next".into());
    }
    parts.join(" · ")
}

fn time(secs: Option<i64>) -> String {
    match secs {
        Some(s) => ago_secs(s.max(0) as u64),
        None => "unknown".into(),
    }
}

fn holder_line(h: &Holder) -> String {
    let mut parts = Vec::new();
    if let Some(j) = &h.job {
        parts.push(j.clone());
    }
    if let Some(p) = &h.project {
        parts.push(p.clone());
    }
    if let Some(i) = &h.issue {
        parts.push(i.clone());
    }
    if parts.is_empty() {
        "unknown holder".into()
    } else {
        parts.join(" · ")
    }
}

/// Where the front item stands: the head of the line, else of the queue,
/// and what the slots are doing, state by state. It says nothing of whether
/// the item can start: the snapshot doesn't establish that (engine routing,
/// reservations, holds and drain capacity decide it).
fn front(snap: &Snapshot) -> Markup {
    html! {
        section.front aria-labelledby="front" {
            h2 #front { "Front of the line" }
            @if let Some(w) = snap.line.first() {
                p.path {
                    strong { (w.job) }
                    " (" (w.project) ", " (w.class) ") has waited "
                    (span(w.wait.max(0) as u64)) " for a slot."
                }
            } @else if let Some(r) = snap.queue.first() {
                p.path {
                    strong { (r.repo) "#" (r.number) }
                    " is next in the queue (" (why(r, 0)) ")."
                }
            } @else {
                p.path { "Nothing is waiting." }
            }
            p.tally { (tally(snap)) }
        }
    }
}

/// Slots by state, each state named, in a fixed order.
fn tally(snap: &Snapshot) -> Markup {
    const ORDER: [SlotState; 9] = [
        SlotState::Running,
        SlotState::Queued,
        SlotState::Starting,
        SlotState::Parked,
        SlotState::Asking,
        SlotState::Ended,
        SlotState::Busy,
        SlotState::Unknown,
        SlotState::Idle,
    ];
    html! {
        @for st in ORDER {
            @let n = snap.slots.iter().filter(|s| s.state == st).count();
            @if n > 0 {
                span.count class=(if stalled(st) { "warn" } else { "" }) {
                    (n) " " (state_word(st))
                }
                " "
            }
        }
    }
}

fn slot_card(s: &Slot) -> Markup {
    let cls = if s.state == SlotState::Idle {
        "slot idle"
    } else if stalled(s.state) {
        "slot stalled"
    } else {
        "slot"
    };
    html! {
        li class=(cls) data-state=(state_word(s.state)) {
            div.slot-head {
                span.slot-name { (kind_word(s.kind)) @if s.name != kind_word(s.kind) { " " (s.name) } }
                span.state { (state_word(s.state)) }
            }
            @if let Some(h) = &s.holder {
                p.holder { (holder_line(h)) }
                p.engine {
                    @if let Some(e) = h.engine { (e.name()) } @else { "engine unknown" }
                    " · " (h.model.as_deref().unwrap_or("engine's own model"))
                    @if let Some(x) = &h.effort { " · " (x) }
                }
                @if !h.awaits.is_empty() {
                    p.awaits { "waiting on " (h.awaits.join(", ")) }
                }
            } @else {
                p.holder {
                    @match s.state {
                        SlotState::Idle => { "Nothing holds it." }
                        SlotState::Unknown => { "Its lock couldn't be looked at." }
                        _ => { "Taken; no holder is known." }
                    }
                }
            }
            @if s.state != SlotState::Idle && s.age.is_some() { p.age { "for " (time(s.age).trim_end_matches(" ago")) } }
        }
    }
}

fn queue(snap: &Snapshot) -> Markup {
    html! {
        section.queue aria-labelledby="queue" {
            h2 #queue { "Queue" }
            @if snap.queue.is_empty() && snap.line.is_empty() {
                p.empty { "Empty." }
            }
            @if !snap.line.is_empty() {
                h3 { "Turns waiting for a slot" }
                ol.rows {
                    @for w in &snap.line {
                        li {
                            span.what { (w.job) " · " (w.project) }
                            span.why { (w.class) " class · waited " (span(w.wait.max(0) as u64)) }
                        }
                    }
                }
            }
            @if !snap.queue.is_empty() {
                h3 { "Ready issues" }
                ol.rows {
                    @for (i, r) in snap.queue.iter().enumerate() {
                        li {
                            span.what { (r.repo) "#" (r.number) }
                            span.why { (why(r, i)) " · opened " (time(r.age)) }
                        }
                    }
                }
            }
        }
    }
}

/// What's deployed isn't in the snapshot; the jobs it lists, and where each
/// stands, are the nearest the snapshot says of the pipeline's work.
fn deploy(snap: &Snapshot) -> Markup {
    html! {
        section.deploy aria-labelledby="deploy" {
            h2 #deploy { "Deploy" }
            @if snap.jobs.is_empty() {
                p.empty { "No job has changed lately." }
            } @else {
                ul.rows {
                    @for j in &snap.jobs {
                        li {
                            span.what { (j.id) " · " (j.state) }
                            span.why {
                                @if !j.awaits.is_empty() { "awaits " (j.awaits.join(", ")) " · " }
                                @if !j.ends.is_empty() { "ends " (j.ends.join(", ")) " · " }
                                @if let Some(d) = &j.demoted { "demoted: " (d) " · " }
                                (j.answer)
                            }
                        }
                    }
                }
            }
        }
    }
}

fn window(name: &str, w: &Option<Window>, at: i64) -> Markup {
    html! {
        @if let Some(w) = w {
            div.window {
                span.name { (name) }
                meter min="0" max="100" value=(w.used) { (w.used) "%" }
                span.pct { (format!("{:.0}", w.used)) "% used · resets in "
                    (span(w.resets_at.saturating_sub(at).max(0) as u64)) }
            }
        }
    }
}

fn meter(m: &Meter, at: i64) -> Markup {
    html! {
        div.meter {
            h3 { (m.engine.name()) }
            @if let Some(r) = &m.reading {
                (window("5 hours", &r.five_hour, at))
                (window("week", &r.weekly, at))
                p.note {
                    @if r.credits { "credits carry on past the windows · " }
                    "read " (ago_secs(r.age.max(0) as u64))
                    @if !r.fresh { " · " strong.warn { "too old to move a turn" } }
                }
            } @else {
                p.note { "No reading." }
            }
            @if let Some(h) = &m.hold {
                p.hold {
                    strong.warn { @if h.owner { "Held by the owner" } @else { "Held: refused" } }
                    " for " (span(h.until.saturating_sub(at).max(0) as u64)) " more: " (h.reason)
                }
            }
        }
    }
}

fn holds(h: &Holds) -> Markup {
    html! {
        section.holds aria-labelledby="holds" {
            h2 #holds { "Holds" }
            dl.facts {
                div { dt { "Paused repos" } dd {
                    @if h.paused.is_empty() { "none" } @else { (h.paused.join(", ")) }
                } }
                div { dt { "Pipeline repos" } dd { (h.pipeline.repos.join(", ")) } }
                div { dt { "Pipeline lead" } dd {
                    @if h.pipeline.lead.is_empty() { "none" } @else { (h.pipeline.lead.join(", ")) }
                } }
                div { dt { "Pipeline-only slots" } dd { (h.pipeline.slots) } }
                @match &h.triage {
                    Some(t) => {
                        div { dt { "Drain" } dd {
                            @if let Some(e) = t.drain { (e.name()) ", " (t.drain_jobs) " at once" }
                            @else { "hands none on" }
                        } }
                        div { dt { "Sorting" } dd { (t.sorting_at_once) " at once" } }
                    }
                    None => { div { dt { "Triage" } dd { "off" } } }
                }
            }
        }
    }
}

pub fn live(f: &Frame, snap: &Snapshot, now: u64) -> Markup {
    let age = now.saturating_sub(snap.at.max(0) as u64);
    page(
        f,
        "live",
        "Live",
        true,
        html! {
            div.live {
                p.taken { "Snapshot taken " (ago_secs(age)) "." }
                (front(snap))
                div.board {
                    section.slots aria-labelledby="slots" {
                        h2 #slots { "Slots" }
                        ul.slot-list {
                            @for s in &snap.slots { (slot_card(s)) }
                        }
                    }
                    (queue(snap))
                }
                (deploy(snap))
                section.meters aria-labelledby="meters" {
                    h2 #meters { "Meters" }
                    div.meter-list {
                        @for m in &snap.meters { (meter(m, snap.at)) }
                    }
                }
                (holds(&snap.holds))
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assets::Assets;

    pub const FIXTURE: &str = include_str!("../../tests/stall-2026-11-11.json");

    #[test]
    fn the_fixture_reads_as_the_schema_says() {
        let schema: serde_json::Value =
            serde_json::from_str(include_str!("../../../burrow-proj/state.schema.json")).unwrap();
        let fixture: serde_json::Value = serde_json::from_str(FIXTURE).unwrap();
        for key in schema["required"].as_array().unwrap() {
            assert!(fixture.get(key.as_str().unwrap()).is_some(), "{key}");
        }
        serde_json::from_str::<Snapshot>(FIXTURE).unwrap();
    }

    #[test]
    fn sections_come_in_order_and_parked_slots_stand_out() {
        let snap: Snapshot = serde_json::from_str(FIXTURE).unwrap();
        let fonts = crate::assets::tests::fonts();
        let assets = Assets::load(crate::assets::tests::shell(), fonts.path()).unwrap();
        let f = Frame {
            title: "box",
            host: "box.ts.net",
            assets: &assets,
        };
        let html = live(&f, &snap, snap.at as u64 + 60).into_string();
        let at = |id: &str| {
            html.find(&format!("id=\"{id}\""))
                .unwrap_or_else(|| panic!("{id}"))
        };
        let order = ["front", "slots", "queue", "deploy", "meters", "holds"].map(at);
        assert!(order.windows(2).all(|w| w[0] < w[1]), "{order:?}");
        assert_eq!(html.matches("slot stalled").count(), 3);
        assert!(html.contains("slot idle"));
        assert!(
            html.contains("3 parked") && html.contains("1 busy"),
            "{html}"
        );
        assert!(!html.contains("Nothing holds it.</p><p class=\"age")); // busy slot isn't called empty
        assert!(html.contains("Taken; no holder is known."));
    }
}
