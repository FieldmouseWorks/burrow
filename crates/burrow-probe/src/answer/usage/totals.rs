//! The usage answer's table: turns, tokens and cost by engine, role and
//! project, with cost recovery matched by run identity across selected turns
//! (token recovery, for engines whose streams carry no cost).
use super::accounting::Report;
use std::collections::{BTreeMap, BTreeSet};

/// What a group of turns adds up to.
#[derive(Default, Clone)]
struct Tally {
    turns: u64,
    tokens: [u64; 4],
    cost: f64,
    missing: BTreeSet<String>,
    recovered: BTreeSet<String>,
    tokens_partial: u64,
    /// Turns whose engine's stream carries tokens but no cost.
    unpriced: u64,
    /// Turns whose engine's stream isn't read here: no cost or tokens.
    unread: u64,
}

impl Tally {
    fn add(&mut self, other: &Tally) {
        self.turns += other.turns;
        for (sum, v) in self.tokens.iter_mut().zip(other.tokens) {
            *sum += v;
        }
        self.cost += other.cost;
        self.unpriced += other.unpriced;
        self.unread += other.unread;
        self.missing.extend(other.missing.iter().cloned());
        self.recovered.extend(other.recovered.iter().cloned());
        self.tokens_partial += other.tokens_partial;
    }

    fn turn(report: Option<&Report>) -> Tally {
        match report {
            Some(r) => Tally {
                turns: 1,
                tokens: r.tokens,
                cost: r.cost,
                unpriced: u64::from(r.unpriced),
                unread: 0,
                missing: r.missing.clone(),
                recovered: r.recovered.clone(),
                tokens_partial: u64::from(r.tokens_partial),
            },
            None => Tally {
                turns: 1,
                unread: 1,
                ..Tally::default()
            },
        }
    }

    /// The row's cells after its key: tokens read `-` where no turn of it
    /// was read, and cost where none was priced.
    fn cells(&self) -> [String; 11] {
        let none = self.unread == self.turns;
        let n = |v: u64| if none { "-".into() } else { v.to_string() };
        let unpriced = self.unread + self.unpriced == self.turns;
        [
            self.turns.to_string(),
            n(self.tokens[0]),
            n(self.tokens[1]),
            n(self.tokens[2]),
            n(self.tokens[3]),
            if unpriced {
                "-".into()
            } else {
                format!("${:.2}", self.cost)
            },
            self.unread.to_string(),
            self.missing.difference(&self.recovered).count().to_string(),
            self.recovered
                .intersection(&self.missing)
                .count()
                .to_string(),
            self.recovered.difference(&self.missing).count().to_string(),
            self.tokens_partial.to_string(),
        ]
    }
}

/// Every turn in the span, by engine, role and project.
#[derive(Default)]
pub struct Totals {
    rows: BTreeMap<(String, String, String), Tally>,
}

const ALL: &str = "all";

impl Totals {
    /// One turn: its stream's report, or None when its engine's isn't read.
    pub fn add(&mut self, engine: &str, role: &str, project: &str, report: Option<&Report>) {
        self.rows
            .entry((engine.into(), role.into(), project.into()))
            .or_default()
            .add(&Tally::turn(report));
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
        let missing = whole.missing.difference(&whole.recovered).count();
        let recovered = whole.recovered.intersection(&whole.missing).count();
        let outside = whole.recovered.difference(&whole.missing).count();
        let mut out = format!(
            "Cost: ${:.2} recorded over {} turns; {missing} runs with cost still missing; {recovered} runs with cost recovered in this span; {outside} earlier runs outside the selected turns whose cost may be included; {} turns whose engine's stream carries tokens but no cost (their runs' missing and recovered tokens count above); {} turns whose engine's cost and tokens aren't read.\nTokens: {} turns with partial counts (claude output counts completed runs only; a codex run that reported no total counts nothing).\n",
            whole.cost, whole.turns, whole.unpriced, whole.unread, whole.tokens_partial
        );
        out += "Columns: missing-runs excludes recovered cost (tokens, for an engine whose stream carries no cost); recovered-runs matches gaps in selected turns; outside-span counts recovered gaps outside selected turns; partial-tokens counts turns with incomplete tokens. Unread turns have unknown cost and tokens.\n";
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
            "unread",
            "missing-runs",
            "recovered-runs",
            "outside-span",
            "partial-tokens",
        ]
        .map(String::from);
        let row = |e: &str, r: &str, p: &str, t: &Tally| {
            let mut cells = vec![display(e), display(r), display(p)];
            cells.extend(t.cells());
            cells
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
