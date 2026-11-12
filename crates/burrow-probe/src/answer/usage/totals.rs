//! The usage answer's table: turns, tokens and cost by engine, role and
//! project, totals first, so an answer cut to fit a comment keeps every
//! total and loses only per-turn detail.
use super::accounting::Report;
use std::collections::BTreeMap;

/// What a group of turns adds up to.
#[derive(Default, Clone)]
struct Tally {
    turns: u64,
    tokens: [u64; 4],
    cost: f64,
    /// Turns whose stream says only part of their cost (`known-cost`).
    partial: u64,
    /// Turns whose engine's stream isn't priced here.
    unread: u64,
}

impl Tally {
    fn add(&mut self, other: &Tally) {
        self.turns += other.turns;
        for (sum, v) in self.tokens.iter_mut().zip(other.tokens) {
            *sum += v;
        }
        self.cost += other.cost;
        self.partial += other.partial;
        self.unread += other.unread;
    }

    fn turn(report: Option<&Report>) -> Tally {
        match report {
            Some(r) => Tally {
                turns: 1,
                tokens: r.tokens,
                cost: r.cost,
                partial: u64::from(r.cost_partial),
                unread: 0,
            },
            None => Tally {
                turns: 1,
                unread: 1,
                ..Tally::default()
            },
        }
    }

    /// The row's cells after its key: tokens and cost read `-` where no
    /// turn of it was priced.
    fn cells(&self) -> [String; 7] {
        let none = self.unread == self.turns;
        let n = |v: u64| if none { "-".into() } else { v.to_string() };
        [
            self.turns.to_string(),
            n(self.tokens[0]),
            n(self.tokens[1]),
            n(self.tokens[2]),
            n(self.tokens[3]),
            if none {
                "-".into()
            } else {
                format!("${:.2}", self.cost)
            },
            (self.partial + self.unread).to_string(),
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
        let mut out = if whole.partial + whole.unread == 0 {
            format!(
                "Cost: ${:.2} over {} turns, every turn's cost known.\n",
                whole.cost, whole.turns
            )
        } else {
            format!(
                "Cost: ${:.2} known over {} turns; {} of them unpriced ({} with part of their cost unavailable, the known-cost turns; {} whose engine's stream isn't priced here), so the true total is higher.\n",
                whole.cost,
                whole.turns,
                whole.partial + whole.unread,
                whole.partial,
                whole.unread
            )
        };
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
            "unpriced",
        ]
        .map(String::from);
        let row = |e: &str, r: &str, p: &str, t: &Tally| {
            let mut cells = vec![e.to_string(), r.to_string(), p.to_string()];
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
