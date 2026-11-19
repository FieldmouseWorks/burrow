//! The API credit meter: how much of an expiring API credit the
//! [`ENGINE`] engine may still spend, and whether it has room for another
//! turn now. Deterministic: the setting, the time and the recorded turn
//! costs decide it, and nothing about it is a model's call.
//!
//! The setting (`proj.apiCredit`, kept in [`FILE`] under `burrow.stateDir`
//! and written only by `settings credit`) says the credit's balance in USD
//! as of `since`, the day it renews (`renews`, the credit expiring at the
//! start of that day, UTC), what it renews to each month after, and a
//! reserve it never spends. Amounts are whole cents; spend is counted in
//! micro-dollars, so no comparison depends on float rounding.
//!
//! The cycle in force at `now` is worked out from the setting alone: before
//! `renews` it began at `since` with `budgetUsd`; from then it began at the
//! latest monthly renewal (counted from `renews`' own day of the month, so a
//! 31st stays the 31st where the month has one) with `monthlyUsd`, whatever
//! the old credit had left and however many renewals passed meanwhile.
//! Readers never write the setting back.
//!
//! The engine has room when both hold, each strictly:
//!
//! - spent + reserve < budget, spent being every turn of the cycle;
//! - today's spend < (budget - spent before today) / days left, today
//!   included, days being UTC calendar days to the expiry: what earlier days
//!   left unspent stays for later ones, and on the last day what's left,
//!   less the reserve, may all go.
//!
//! A missing, malformed or unreadable setting, or turn costs that can't be
//! read, is no room: [`Meter::at`] and the ledger behind it return an error
//! and the caller treats any error as none.

use std::collections::BTreeMap;
use std::path::PathBuf;

use jiff::civil::Date;
use jiff::tz::TimeZone;
use jiff::{Timestamp, ToSpan};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::{Config, live};

/// The engine whose turns spend the credit.
pub const ENGINE: &str = "claude-api";
/// Where the setting is kept, under `burrow.stateDir`: beside the
/// admission overlay, under its lock and log, but in a file of its own, so
/// a generation from before it still reads the admission overlay.
pub const FILE: &str = "settings/overlay.credit.json";
/// What `settings credit` takes.
pub const USAGE: &str =
    "settings credit BUDGET_USD RENEWS_YYYY-MM-DD MONTHLY_USD RESERVE_USD | settings credit off";

const DAY: i64 = 86_400;
const MICROS_PER_CENT: i64 = 10_000;
/// The most a setting may say: well past any credit, and far from overflow.
const MAX_CENTS: i64 = 10_000_000_000;

/// An amount in whole cents, kept as a JSON number of dollars.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cents(pub i64);

impl Cents {
    /// `214`, `214.5` or `214.37`: dollars, to the cent, and nothing else.
    pub fn parse(word: &str) -> Option<Cents> {
        let (whole, frac) = word.split_once('.').unwrap_or((word, ""));
        let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
        if whole.is_empty()
            || whole.len() > 10
            || !digits(whole)
            || frac.len() > 2
            || !digits(frac)
            || (word.contains('.') && frac.is_empty())
        {
            return None;
        }
        let frac: i64 = format!("{frac:0<2}").parse().ok()?;
        let cents = whole.parse::<i64>().ok()? * 100 + frac;
        (cents <= MAX_CENTS).then_some(Cents(cents))
    }

    fn from_dollars(n: f64) -> Option<Cents> {
        let cents = n * 100.0;
        let whole = cents.round();
        (n.is_finite() && n >= 0.0 && (cents - whole).abs() < 1e-6 && whole <= MAX_CENTS as f64)
            .then_some(Cents(whole as i64))
    }

    fn micros(self) -> i64 {
        self.0 * MICROS_PER_CENT
    }
}

impl Serialize for Cents {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_f64(self.0 as f64 / 100.0)
    }
}

impl<'de> Deserialize<'de> for Cents {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let n = f64::deserialize(d)?;
        Cents::from_dollars(n).ok_or_else(|| {
            serde::de::Error::custom(format!("{n} is not a dollar amount to the cent"))
        })
    }
}

/// A calendar day, kept as `YYYY-MM-DD`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Day(pub Date);

impl Day {
    pub fn parse(word: &str) -> Option<Day> {
        // jiff takes other spellings too; the setting is this one only.
        let shaped = word.len() == 10
            && word.bytes().enumerate().all(|(i, b)| match i {
                4 | 7 => b == b'-',
                _ => b.is_ascii_digit(),
            });
        shaped.then(|| word.parse().ok().map(Day)).flatten()
    }
}

impl Serialize for Day {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0.to_string())
    }
}

impl<'de> Deserialize<'de> for Day {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let text = String::deserialize(d)?;
        Day::parse(&text)
            .ok_or_else(|| serde::de::Error::custom(format!("{text} is not a YYYY-MM-DD date")))
    }
}

/// The setting.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Credit {
    /// The credit's balance at `since`.
    pub budget_usd: Cents,
    /// The day it renews: the credit expires at its start, UTC.
    pub renews: Day,
    /// What it renews to each month from `renews` on.
    pub monthly_usd: Cents,
    /// What is never spent.
    pub reserve_usd: Cents,
    /// When the balance was read (Unix seconds): the first cycle's start.
    #[serde(deserialize_with = "seconds")]
    pub since: i64,
}

fn seconds<'de, D: Deserializer<'de>>(d: D) -> Result<i64, D::Error> {
    let n = u64::deserialize(d)?;
    i64::try_from(n).map_err(serde::de::Error::custom)
}

/// The document the setting is kept in.
#[derive(Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Document {
    #[serde(default)]
    credit: Option<Credit>,
    #[serde(default)]
    receipts: BTreeMap<String, String>,
}

impl live::Document for Document {
    fn receipts(&mut self) -> &mut BTreeMap<String, String> {
        &mut self.receipts
    }
}

/// The setting from its file's bytes, for a reader that opens the file its
/// own way (the probe's confined reads).
pub fn parse(bytes: &[u8]) -> Result<Option<Credit>, String> {
    serde_json::from_slice::<Document>(bytes)
        .map(|d| d.credit)
        .map_err(|e| format!("{FILE}: {e}"))
}

/// One turn's recorded cost: when it started, and what it cost in
/// micro-dollars.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Spend {
    pub at: i64,
    pub micros: i64,
}

/// The cycle in force: when it began, when its credit expires, and what it
/// began with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Cycle {
    pub start: i64,
    pub expires: i64,
    /// Micro-dollars.
    pub budget: i64,
    /// Whether it's a renewal's, rather than the one the setting began.
    pub renewed: bool,
}

fn utc_day(at: i64) -> Result<Date, String> {
    Timestamp::from_second(at)
        .map(|t| t.to_zoned_datetime(TimeZone::UTC).date())
        .map_err(|e| format!("{at}: {e}"))
}

fn midnight(day: Date) -> Result<i64, String> {
    day.to_zoned_datetime(TimeZone::UTC)
        .map(|z| z.timestamp().as_second())
        .map_err(|e| format!("{day}: {e}"))
}

/// The K-th renewal after `renews` (0 is `renews` itself), from its own day
/// of the month: jiff clamps one that month lacks to its last day.
fn renewal(renews: Date, k: i64) -> Result<Date, String> {
    renews
        .checked_add(k.months())
        .map_err(|e| format!("{renews} + {k} months: {e}"))
}

impl Credit {
    /// The cycle in force at `now`.
    pub fn cycle(&self, now: i64) -> Result<Cycle, String> {
        let today = utc_day(now)?;
        let first = self.renews.0;
        if midnight(first)? <= self.since {
            return Err(format!(
                "{FILE}: since {} is not before renews {first}",
                self.since
            ));
        }
        if today < first {
            return Ok(Cycle {
                start: self.since,
                expires: midnight(first)?,
                budget: self.budget_usd.micros(),
                renewed: false,
            });
        }
        let mut k = (i64::from(today.year()) - i64::from(first.year())) * 12
            + i64::from(today.month())
            - i64::from(first.month());
        if renewal(first, k)? > today {
            k -= 1;
        }
        Ok(Cycle {
            start: midnight(renewal(first, k)?)?,
            expires: midnight(renewal(first, k + 1)?)?,
            budget: self.monthly_usd.micros(),
            renewed: true,
        })
    }
}

/// The credit at a moment: its cycle, what it has spent, and whether the
/// engine has room. Amounts are micro-dollars.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Meter {
    pub now: i64,
    pub cycle: Cycle,
    /// UTC days to the expiry, today included: 1 on the last day.
    pub days_left: i64,
    pub spent: i64,
    pub today: i64,
    pub reserve: i64,
    /// Today's allowance: (budget - spent before today) / days left.
    pub allowance: i64,
    /// A turn whose cost isn't recorded yet, when there is one: its spend
    /// can't be known, so there's no room until it is.
    pub pending: Option<String>,
}

impl Meter {
    /// The meter at `now`, from the setting and the cycle's recorded turn
    /// costs (turns before the cycle's start are left out here).
    pub fn at(credit: &Credit, now: i64, spends: &[Spend]) -> Result<Meter, String> {
        let cycle = credit.cycle(now)?;
        let today = utc_day(now)?;
        let day_start = midnight(today)?.max(cycle.start);
        let days_left = (cycle.expires - midnight(today)?).div_euclid(DAY);
        let mut spent = 0i64;
        let mut before = 0i64;
        for s in spends.iter().filter(|s| s.at >= cycle.start) {
            if s.micros < 0 {
                return Err(format!("a turn's cost of {} is negative", s.micros));
            }
            spent = spent.checked_add(s.micros).ok_or("spend overflows")?;
            if s.at < day_start {
                before += s.micros;
            }
        }
        Ok(Meter {
            now,
            cycle,
            days_left,
            spent,
            today: spent - before,
            reserve: credit.reserve_usd.micros(),
            allowance: (cycle.budget - before).div_euclid(days_left.max(1)),
            pending: None,
        })
    }

    /// What the credit has left, reserve included.
    pub fn remaining(&self) -> i64 {
        self.cycle.budget - self.spent
    }

    /// Whether the engine may start a turn now.
    pub fn room(&self) -> bool {
        self.pending.is_none()
            && self.spent + self.reserve < self.cycle.budget
            && self.today < self.allowance
    }

    /// Why it has none, when it hasn't.
    pub fn short(&self) -> Option<String> {
        if self.spent + self.reserve >= self.cycle.budget {
            Some("spent down to the reserve".into())
        } else if self.today >= self.allowance {
            Some("today's allowance spent".into())
        } else {
            self.pending.clone()
        }
    }

    /// Its lines for a report: no credential is in any of it.
    pub fn text(&self) -> String {
        format!(
            "{ENGINE} credit: {}\n\
             cycle {} to {} (credit expires then), {} UTC days left with today; {}\n\
             budget {}, spent {}, reserve {}, remaining {}\n\
             today spent {} of {} allowed\n",
            match self.short() {
                None => "room".to_owned(),
                Some(why) => format!("no room ({why})"),
            },
            stamp(self.cycle.start),
            stamp(self.cycle.expires),
            self.days_left,
            if self.cycle.renewed {
                "renewed to the monthly amount"
            } else {
                "the balance as set"
            },
            usd(self.cycle.budget),
            usd(self.spent),
            usd(self.reserve),
            usd(self.remaining()),
            usd(self.today),
            usd(self.allowance),
        )
    }
}

fn stamp(at: i64) -> String {
    Timestamp::from_second(at).map_or_else(
        |_| at.to_string(),
        |t| t.strftime("%Y-%m-%d %H:%M:%SZ").to_string(),
    )
}

/// Micro-dollars as `$1.20` or `$0.123456`: cents always, more digits only
/// where there are any.
pub fn usd(micros: i64) -> String {
    let sign = if micros < 0 { "-" } else { "" };
    let m = micros.unsigned_abs();
    let frac = format!("{:06}", m % 1_000_000);
    let frac = frac.trim_end_matches('0');
    format!("{sign}${}.{frac:0<2}", m / 1_000_000)
}

impl Config {
    /// The file the credit setting is kept in.
    pub fn credit_file(&self) -> PathBuf {
        self.overlay.with_extension("credit.json")
    }

    /// The setting; None when there's none.
    pub fn credit(&self) -> Result<Option<Credit>, String> {
        Ok(live::read::<Document>(&self.credit_file())?.credit)
    }

    /// `credit BUDGET RENEWS MONTHLY RESERVE` or `credit off`.
    pub(crate) fn change_credit(
        &self,
        args: &[String],
        actor: &str,
        receipt: Option<&str>,
    ) -> Result<String, String> {
        let words: Vec<&str> = args.iter().map(String::as_str).collect();
        let now = crate::now();
        let asked = match words.as_slice() {
            ["credit", "off"] => None,
            ["credit", ..] => Some(asked(&words[1..], now)?),
            _ => return Err(format!("usage: {USAGE}")),
        };
        let path = self.credit_file();
        let answer = live::write(
            &path,
            &self.overlay.with_extension("lock"),
            &self.overlay.with_extension("log"),
            actor,
            receipt,
            args,
            |doc: &mut Document| {
                let next = asked.map(|mut c| {
                    // The balance as of an earlier reading still holds while
                    // it's the same and its cycle hasn't renewed: a repeated
                    // or reserve-only change mustn't forget what it spent.
                    if let Some(old) = &doc.credit
                        && old.budget_usd == c.budget_usd
                        && utc_day(now).is_ok_and(|d| d < old.renews.0)
                        && midnight(c.renews.0).is_ok_and(|r| old.since < r)
                    {
                        c.since = old.since;
                    }
                    c
                });
                let said = describe(next.as_ref());
                if doc.credit == next {
                    return Ok(receipt.is_some().then_some(said));
                }
                doc.credit = next;
                Ok(Some(said))
            },
        )?;
        match answer {
            Some(said) => Ok(said),
            None => Ok(describe(self.credit()?.as_ref())),
        }
    }
}

/// The setting a `credit` command asks for, read at `now`.
fn asked(words: &[&str], now: i64) -> Result<Credit, String> {
    let [budget, renews, monthly, reserve] = words else {
        return Err(format!("usage: {USAGE}"));
    };
    let amount =
        |w: &str| Cents::parse(w).ok_or(format!("{w} is not a dollar amount, like 214.37"));
    let renews = Day::parse(renews).ok_or(format!("{renews} is not a YYYY-MM-DD date"))?;
    if renews.0 <= utc_day(now)? {
        return Err(format!("{} is not after today (UTC)", renews.0));
    }
    Ok(Credit {
        budget_usd: amount(budget)?,
        renews,
        monthly_usd: amount(monthly)?,
        reserve_usd: amount(reserve)?,
        since: now,
    })
}

fn describe(credit: Option<&Credit>) -> String {
    match credit {
        None => format!("No {ENGINE} credit is set: the engine has no room.\n"),
        Some(c) => format!(
            "{ENGINE} credit: {} as of {}, expiring {} (UTC), then {} a month; {} kept in reserve.\n",
            usd(c.budget_usd.micros()),
            stamp(c.since),
            c.renews.0,
            usd(c.monthly_usd.micros()),
            usd(c.reserve_usd.micros()),
        ),
    }
}

/// Whether the door may take these words.
pub(crate) fn valid(words: &[&str]) -> bool {
    match words {
        ["off"] => true,
        _ => asked(words, crate::now()).is_ok(),
    }
}

#[cfg(test)]
mod tests;
