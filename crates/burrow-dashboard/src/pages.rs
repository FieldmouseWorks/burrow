//! The pages, rendered here. Each is the whole page: the shell around it
//! and its content, with nothing for a script to fill in.

use maud::{DOCTYPE, Markup, html};

use crate::assets::Assets;
use crate::core::{Enrolled, Live};
use crate::enroll::CODE_TTL;
use crate::session::Limits;

/// What every page knows.
pub struct Frame<'a> {
    pub title: &'a str,
    /// The ts.net name the dashboard is reached at.
    pub host: &'a str,
    pub assets: &'a Assets,
}

/// The command that prints an enrolment code.
const ENROL_COMMAND: &str = "sudo burrow-dashboard enroll";

fn page(f: &Frame, name: &str, heading: &str, signed_in: bool, body: Markup) -> Markup {
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover";
                title { (heading) " · " (f.title) }
                meta name="color-scheme" content="dark";
                meta name="theme-color" content="#0b0b0b";
                meta name="mobile-web-app-capable" content="yes";
                meta name="apple-mobile-web-app-capable" content="yes";
                meta name="apple-mobile-web-app-title" content=(f.title);
                meta name="apple-mobile-web-app-status-bar-style" content="black-translucent";
                link rel="manifest" href="/manifest.webmanifest";
                link rel="icon" type="image/png" href=(f.assets.url("favicon-32.png"));
                link rel="apple-touch-icon" href=(f.assets.url("apple-touch-icon.png"));
                link rel="preload" as="font" type="font/woff2" crossorigin
                    href=(f.assets.url("fonts/big-shoulders-display.woff2"));
                link rel="stylesheet" href=(f.assets.url("app.css"));
                script src=(f.assets.url("app.js")) defer {}
            }
            body data-page=(name) {
                p.offline data-offline hidden role="status" {
                    "This device is offline. What's on screen may be out of date."
                }
                header.masthead {
                    a.brand href="/" aria-label={ (f.title) ", home" } {
                        img src=(f.assets.url("mark-128.png")) alt="" width="40" height="40";
                        span { (f.title) }
                    }
                    @if signed_in {
                        form.inline method="post" action="/logout" {
                            button.button.quiet type="submit" { "Sign out" }
                        }
                    } @else {
                        p.where { (f.host) }
                    }
                }
                main { (body) }
                footer.colophon {
                    p { "Reached over the tailnet only · burrow, by Fieldmouse Works" }
                }
            }
        }
    }
}

fn status() -> Markup {
    html! { p.status data-status role="alert" hidden {} }
}

pub fn login(f: &Frame, any_passkeys: bool) -> Markup {
    page(
        f,
        "login",
        "Sign in",
        false,
        html! {
            section.hero.tall {
                div.shaft aria-hidden="true" {}
                div.hero-inner {
                    p.eyebrow { "Dashboard" }
                    h1 { "Sign in" }
                    @if any_passkeys {
                        p.lede { "Use the passkey on this device. Nothing else opens "
                            (f.title) "'s dashboard." }
                        form data-login {
                            button.button type="submit" { "Sign in with a passkey" }
                            (status())
                        }
                        p.aside { "A device without a passkey yet? " a href="/enroll" { "Add it with a code" } "." }
                    } @else {
                        p.lede { "No device has a passkey here yet. Add this one first: it takes a code from "
                            (f.title) " and a moment." }
                        p { a.button href="/enroll" { "Add this device" } }
                    }
                }
            }
        },
    )
}

pub fn enrol(f: &Frame) -> Markup {
    page(
        f,
        "enrol",
        "Add this device",
        false,
        html! {
            section.hero.tall {
                div.shaft aria-hidden="true" {}
                div.hero-inner {
                    p.eyebrow { "New passkey" }
                    h1 { "Add this device" }
                    p.lede {
                        "On " (f.title) ", run " code { (ENROL_COMMAND) } ". It prints a code that adds one device, within "
                        (span(CODE_TTL)) "."
                    }
                    form data-enrol {
                        div.field {
                            label for="code" { "Code" }
                            input #code .code name="code" required autocomplete="one-time-code"
                                autocapitalize="characters" spellcheck="false" inputmode="text"
                                maxlength="16";
                        }
                        div.field {
                            label for="name" { "This device's name" }
                            input #name name="name" maxlength="40" autocomplete="off";
                            span.hint { "So you can tell your passkeys apart: Laptop, say, or Phone." }
                        }
                        button.button type="submit" { "Create the passkey" }
                        (status())
                    }
                    p.aside { a href="/login" { "Back to sign in" } }
                }
            }
        },
    )
}

pub fn home(
    f: &Frame,
    owner: &str,
    live: &Live,
    limits: Limits,
    keys: &[Enrolled],
    now: u64,
) -> Markup {
    let s = &live.session;
    let capped = s.began.saturating_add(limits.max);
    page(
        f,
        "home",
        "Home",
        true,
        html! {
            section.hero {
                div.shaft aria-hidden="true" {}
                div.hero-inner {
                    p.eyebrow { (f.host) }
                    h1 { (f.title) }
                    p.lede { "Signed in as " strong { (owner) } ", with the passkey on " (s.passkey) "." }
                }
            }
            section.panels {
                div.panel aria-labelledby="session" {
                    h2 #session { "This session" }
                    dl.facts {
                        div { dt { "Began" } dd { (ago(now, s.began)) } }
                        div { dt { "Idle limit" } dd { "Ends after " (span(limits.idle)) " unused" } }
                        div { dt { "Ends" } dd {
                            @if live.ends < capped { (until(now, live.ends)) ", unless it's used before then" }
                            @else { (until(now, live.ends)) ", however much it's used" }
                        } }
                    }
                }
                div.panel aria-labelledby="passkeys" {
                    h2 #passkeys { "Passkeys" }
                    ul.keys {
                        @for k in keys {
                            li {
                                div.name {
                                    span { (k.name) }
                                    @if k.name == s.passkey { span.this { "This one" } }
                                }
                                p.when {
                                    "Added " (ago(now, k.added))
                                    @if let Some(used) = k.used { " · last used " (ago(now, used)) }
                                }
                            }
                        }
                    }
                    p { "To add another device, run " code { (ENROL_COMMAND) } " on " (f.title)
                        " and open " a href="/enroll" { "Add a device" } " on it." }
                }
            }
        },
    )
}

pub fn offline(f: &Frame) -> Markup {
    page(
        f,
        "offline",
        "Offline",
        false,
        html! {
            section.hero.tall {
                div.shaft aria-hidden="true" {}
                div.hero-inner {
                    p.eyebrow { "No connection" }
                    h1 { "Offline" }
                    p.lede { (f.title) " can't be reached from this device just now. "
                        "This page reloads by itself once the connection is back." }
                    p { button.button type="button" data-retry { "Try again" } }
                }
            }
        },
    )
}

pub fn not_found(f: &Frame) -> Markup {
    page(
        f,
        "missing",
        "Not here",
        false,
        html! {
            section.hero.tall {
                div.shaft aria-hidden="true" {}
                div.hero-inner {
                    p.eyebrow { "404" }
                    h1 { "Not here" }
                    p.lede { "There's no page at this address." }
                    p { a.button href="/" { "Go home" } }
                }
            }
        },
    )
}

/// A length of time, in the largest unit that fits it whole.
pub fn span(secs: u64) -> String {
    let (n, unit) = match secs {
        s if s < 60 => (s, "second"),
        s if s < 3600 => (s / 60, "minute"),
        s if s < 86400 => (s / 3600, "hour"),
        s => (s / 86400, "day"),
    };
    format!("{n} {unit}{}", if n == 1 { "" } else { "s" })
}

pub fn ago(now: u64, then: u64) -> String {
    match now.saturating_sub(then) {
        s if s < 60 => "just now".into(),
        s => format!("{} ago", span(s)),
    }
}

pub fn until(now: u64, then: u64) -> String {
    match then.saturating_sub(now) {
        s if s < 60 => "in under a minute".into(),
        s => format!("in {}", span(s)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_read_plainly() {
        assert_eq!(span(1), "1 second");
        assert_eq!(span(15 * 60), "15 minutes");
        assert_eq!(span(86400 * 7), "7 days");
        assert_eq!(ago(100, 90), "just now");
        assert_eq!(ago(7200, 0), "2 hours ago");
        assert_eq!(until(0, 3600), "in 1 hour");
        assert_eq!(until(10, 5), "in under a minute");
    }
}
