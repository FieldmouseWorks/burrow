//! The usage answer's table: turns, tokens and cost by engine, role and
//! project, with cost recovery matched by run identity across selected turns
//! (token recovery, for engines whose streams carry no cost).
use burrow_accounting::{Tally, Turn};
use std::collections::BTreeMap;

/// The row's cells after its key: tokens read `-` where no turn of it was
/// read, cost where none was measured, and estimate where none was
/// estimated.
fn cells(t: &Tally) -> [String; 12] {
    let none = t.unread == t.turns;
    let n = |v: u64| if none { "-".into() } else { v.to_string() };
    let money = |count: u64, usd: f64| {
        if count == 0 {
            "-".into()
        } else {
            format!("${usd:.2}")
        }
    };
    [
        t.turns.to_string(),
        n(t.tokens[0]),
        n(t.tokens[1]),
        n(t.tokens[2]),
        n(t.tokens[3]),
        money(t.measured, t.cost),
        money(t.estimated, t.estimate),
        t.unread.to_string(),
        t.still_missing().to_string(),
        t.recovered_here().to_string(),
        t.recovered_outside().to_string(),
        t.tokens_partial.to_string(),
    ]
}

/// Every turn in the span, by engine, role and project.
#[derive(Default)]
pub struct Totals {
    rows: BTreeMap<(String, String, String), Tally>,
}

const ALL: &str = "all";

impl Totals {
    pub fn add(&mut self, engine: &str, role: &str, project: &str, turn: &Turn) {
        self.rows
            .entry((engine.into(), role.into(), project.into()))
            .or_default()
            .add_turn(turn);
    }

    /// The table, totals first: by engine, by engine and role, by engine and
    /// project, then by all three; a line before it says how much of the
    /// cost is unknown.
    pub fn show(&self) -> String {
        let mut whole = Tally::default();
        let mut engines: BTreeMap<&str, Tally> = BTreeMap::new();
        let mut roles: BTreeMap<(&str, &str), Tally> = BTreeMap::new();
        let mut projects: BTreeMap<(&str, &str), Tally> = BTreeMap::new();
        for ((engine, role, project), t) in &self.rows {
            whole.add(t);
            engines.entry(engine).or_default().add(t);
            roles.entry((engine, role)).or_default().add(t);
            projects.entry((engine, project)).or_default().add(t);
        }
        let missing = whole.still_missing();
        let recovered = whole.recovered_here();
        let outside = whole.recovered_outside();
        let mut out = format!(
            "Cost: ${:.2} recorded over {} turns; {missing} runs with cost still missing; {recovered} runs with cost recovered in this span; {outside} earlier runs outside the selected turns whose cost may be included; {} turns whose engine's stream carries tokens but no cost (their runs' missing and recovered tokens count above); {} turns whose engine's cost and tokens aren't read.\nTokens: {} turns with partial counts (claude output counts completed runs only; a codex run that reported no total counts nothing).\nEstimate: ${:.2} over {} of those turns at configured prices, not measured; {} turns with cost unavailable.\n",
            whole.cost,
            whole.turns,
            whole.estimated + whole.unpriced,
            whole.unread,
            whole.tokens_partial,
            whole.estimate,
            whole.estimated,
            whole.unpriced + whole.unread,
        );
        out += "Columns: cost is measured by the engine; estimate is configured prices times tokens, only for turns without measured cost; missing-runs excludes recovered cost (tokens, for an engine whose stream carries no cost); recovered-runs matches gaps in selected turns; outside-span counts recovered gaps outside selected turns; partial-tokens counts turns with incomplete tokens. Unread turns have unknown cost and tokens.\n";
        let head = [
            "engine",
            "role",
            "project",
            "turns",
            "input",
            "output",
            "cache-read",
            "cache-write",
            "cost",
            "estimate",
            "unread",
            "missing-runs",
            "recovered-runs",
            "outside-span",
            "partial-tokens",
        ]
        .map(String::from);
        let row = |e: &str, r: &str, p: &str, t: &Tally| {
            let mut row = vec![display(e), display(r), display(p)];
            row.extend(cells(t));
            row
        };
        let mut sections: Vec<(&str, Vec<Vec<String>>)> = vec![
            (
                "By engine:",
                engines.iter().map(|(e, t)| row(e, ALL, ALL, t)).collect(),
            ),
            (
                "By engine and role:",
                roles.iter().map(|((e, r), t)| row(e, r, ALL, t)).collect(),
            ),
            (
                "By engine and project:",
                projects
                    .iter()
                    .map(|((e, p), t)| row(e, ALL, p, t))
                    .collect(),
            ),
            (
                "By engine, role and project:",
                self.rows
                    .iter()
                    .map(|((e, r, p), t)| row(e, r, p, t))
                    .collect(),
            ),
        ];
        sections.retain(|(_, rows)| !rows.is_empty());
        if sections.is_empty() {
            return out;
        }
        let mut width = head.clone().map(|h| h.len());
        for (_, rows) in &sections {
            for r in rows {
                for (w, c) in width.iter_mut().zip(r) {
                    *w = (*w).max(c.len());
                }
            }
        }
        // Names left, numbers right; no trailing spaces.
        let line = |cells: &[String]| {
            let mut s = String::new();
            for (i, (c, w)) in cells.iter().zip(width).enumerate() {
                if i > 0 {
                    s += "  ";
                }
                if i < 3 {
                    s += &format!("{c:<w$}");
                } else {
                    s += &format!("{c:>w$}");
                }
            }
            s.trim_end().to_string() + "\n"
        };
        out += &line(&head);
        for (title, rows) in sections {
            out += title;
            out += "\n";
            for r in rows {
                out += &line(&r);
            }
        }
        out
    }
}

// Keep grouping keys verbatim; escape only the presentation so names cannot
// create table rows or change terminal direction.
fn display(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c == ' ' {
                "\\x20".into()
            } else {
                c.escape_default().to_string()
            }
        })
        .collect()
}
