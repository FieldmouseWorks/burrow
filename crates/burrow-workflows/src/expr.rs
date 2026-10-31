//! GitHub Actions expressions (`${{ ... }}`, or text with some in it), worked
//! out as GitHub works them out, for what the workflows' `if:`s and
//! concurrency groups use: context paths, string, number, boolean and null
//! literals, `format()`, `cancelled()`, `!`, `<`, `<=`, `>`, `>=`, `==`, `!=`,
//! `&&` and `||` (which give an operand's value), and parentheses.

use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq)]
pub enum V {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
}

impl V {
    pub fn truthy(&self) -> bool {
        match self {
            V::Null => false,
            V::Bool(b) => *b,
            V::Num(n) => *n != 0.0 && !n.is_nan(),
            V::Str(s) => !s.is_empty(),
        }
    }

    /// As GitHub puts it in a string.
    pub fn text(&self) -> String {
        match self {
            V::Null => String::new(),
            V::Bool(b) => b.to_string(),
            V::Num(n) => n.to_string(),
            V::Str(s) => s.clone(),
        }
    }

    fn num(&self) -> f64 {
        match self {
            V::Null => 0.0,
            V::Bool(b) => f64::from(u8::from(*b)),
            V::Num(n) => *n,
            V::Str(s) if s.trim().is_empty() => 0.0,
            V::Str(s) => s.trim().parse().unwrap_or(f64::NAN),
        }
    }

    /// `==`: strings ignore case, and values of different types compare as
    /// numbers.
    fn equals(&self, other: &V) -> bool {
        match (self, other) {
            (V::Str(a), V::Str(b)) => a.to_lowercase() == b.to_lowercase(),
            (V::Null, V::Null) => true,
            (V::Bool(a), V::Bool(b)) => a == b,
            (a, b) => a.num() == b.num(),
        }
    }

    /// `<`, `<=`, `>` and `>=`: strings by their letters, ignoring case, and
    /// values of different types as numbers, where NaN is never in order.
    fn order(&self, other: &V) -> Option<std::cmp::Ordering> {
        match (self, other) {
            (V::Str(a), V::Str(b)) => Some(a.to_lowercase().cmp(&b.to_lowercase())),
            (a, b) => a.num().partial_cmp(&b.num()),
        }
    }
}

impl From<&str> for V {
    fn from(s: &str) -> V {
        V::Str(s.to_owned())
    }
}

impl From<bool> for V {
    fn from(b: bool) -> V {
        V::Bool(b)
    }
}

impl From<u64> for V {
    fn from(n: u64) -> V {
        V::Num(n as f64)
    }
}

/// The contexts an expression reads, by their whole path
/// (`github.event_name`, `needs.lane.result`, `cancelled()`). An expression
/// that reads one not given fails the test.
pub type Ctx = HashMap<String, V>;

/// `value`, worked out: a whole `${{ ... }}` gives its value, and text with
/// expressions in it gives the text with each one's.
pub fn value(text: &str, ctx: &Ctx) -> V {
    let t = text.trim();
    if let Some(inner) = t.strip_prefix("${{").and_then(|t| t.strip_suffix("}}"))
        && !inner.contains("${{")
    {
        return eval(&parse(inner), ctx);
    }
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find("${{") {
        out.push_str(&rest[..at]);
        let end = rest[at..].find("}}").expect("an unclosed ${{") + at;
        out.push_str(&eval(&parse(&rest[at + 3..end]), ctx).text());
        rest = &rest[end + 2..];
    }
    out.push_str(rest);
    V::Str(out)
}

/// A bare expression (an `if:` without `${{ }}`), worked out.
pub fn condition(expr: &str, ctx: &Ctx) -> bool {
    value(&format!("${{{{ {expr} }}}}"), ctx).truthy()
}

#[derive(Debug)]
enum E {
    Lit(V),
    Ctx(String),
    Not(Box<E>),
    Eq(Box<E>, Box<E>, bool),
    /// An order and whether it holds when the two are equal: `<=`, `>=`.
    Ord(Box<E>, Box<E>, std::cmp::Ordering, bool),
    And(Box<E>, Box<E>),
    Or(Box<E>, Box<E>),
    Call(String, Vec<E>),
}

fn eval(e: &E, ctx: &Ctx) -> V {
    match e {
        E::Lit(v) => v.clone(),
        E::Ctx(path) => ctx
            .get(path)
            .cloned()
            .unwrap_or_else(|| panic!("the test gives no {path}")),
        E::Not(e) => V::Bool(!eval(e, ctx).truthy()),
        E::Eq(a, b, equal) => V::Bool(eval(a, ctx).equals(&eval(b, ctx)) == *equal),
        E::Ord(a, b, want, or_equal) => {
            let got = eval(a, ctx).order(&eval(b, ctx));
            V::Bool(got == Some(*want) || (*or_equal && got == Some(std::cmp::Ordering::Equal)))
        }
        E::And(a, b) => {
            let a = eval(a, ctx);
            if a.truthy() { eval(b, ctx) } else { a }
        }
        E::Or(a, b) => {
            let a = eval(a, ctx);
            if a.truthy() { a } else { eval(b, ctx) }
        }
        E::Call(f, args) if f == "format" => {
            let mut args = args.iter().map(|a| eval(a, ctx));
            let template = args.next().expect("format's string").text();
            let args: Vec<String> = args.map(|a| a.text()).collect();
            let mut out = template;
            for (i, a) in args.iter().enumerate() {
                out = out.replace(&format!("{{{i}}}"), a);
            }
            V::Str(out)
        }
        E::Call(f, args) if args.is_empty() => eval(&E::Ctx(format!("{f}()")), ctx),
        E::Call(f, _) => panic!("no function {f} here"),
    }
}

fn parse(text: &str) -> E {
    let mut p = Parser {
        s: text.as_bytes(),
        at: 0,
    };
    let e = p.or();
    p.space();
    assert_eq!(p.at, p.s.len(), "can't read {text:?} past {}", p.at);
    e
}

struct Parser<'a> {
    s: &'a [u8],
    at: usize,
}

impl Parser<'_> {
    fn space(&mut self) {
        while self.s.get(self.at).is_some_and(u8::is_ascii_whitespace) {
            self.at += 1;
        }
    }

    fn eat(&mut self, token: &str) -> bool {
        self.space();
        let ok = self.s[self.at..].starts_with(token.as_bytes());
        if ok {
            self.at += token.len();
        }
        ok
    }

    fn or(&mut self) -> E {
        let mut e = self.and();
        while self.eat("||") {
            e = E::Or(Box::new(e), Box::new(self.and()));
        }
        e
    }

    fn and(&mut self) -> E {
        let mut e = self.cmp();
        while self.eat("&&") {
            e = E::And(Box::new(e), Box::new(self.cmp()));
        }
        e
    }

    fn cmp(&mut self) -> E {
        let mut e = self.order();
        loop {
            let equal = if self.eat("==") {
                true
            } else if self.eat("!=") {
                false
            } else {
                return e;
            };
            e = E::Eq(Box::new(e), Box::new(self.order()), equal);
        }
    }

    /// The orders bind tighter than `==` and `!=`, as GitHub's do.
    fn order(&mut self) -> E {
        use std::cmp::Ordering::{Greater, Less};
        let mut e = self.unary();
        loop {
            let (want, or_equal) = if self.eat("<=") {
                (Less, true)
            } else if self.eat(">=") {
                (Greater, true)
            } else if self.eat("<") {
                (Less, false)
            } else if self.eat(">") {
                (Greater, false)
            } else {
                return e;
            };
            e = E::Ord(Box::new(e), Box::new(self.unary()), want, or_equal);
        }
    }

    fn unary(&mut self) -> E {
        if self.eat("!") {
            return E::Not(Box::new(self.unary()));
        }
        self.primary()
    }

    fn primary(&mut self) -> E {
        if self.eat("(") {
            let e = self.or();
            assert!(self.eat(")"), "an unclosed (");
            return e;
        }
        self.space();
        let start = self.at;
        if self.eat("'") {
            let mut s = String::new();
            loop {
                let rest = &self.s[self.at..];
                let end = rest
                    .iter()
                    .position(|&c| c == b'\'')
                    .expect("an unclosed string");
                s.push_str(std::str::from_utf8(&rest[..end]).unwrap());
                self.at += end + 1;
                if self.s.get(self.at) == Some(&b'\'') {
                    s.push('\'');
                    self.at += 1;
                } else {
                    return E::Lit(V::Str(s));
                }
            }
        }
        let word = |c: &u8| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.');
        while self.s.get(self.at).is_some_and(word) {
            self.at += 1;
        }
        let w = std::str::from_utf8(&self.s[start..self.at]).unwrap();
        assert!(!w.is_empty(), "can't read past {start}");
        if w.starts_with(|c: char| c.is_ascii_digit()) {
            return E::Lit(V::Num(w.parse().expect("a number")));
        }
        match w {
            "true" => return E::Lit(V::Bool(true)),
            "false" => return E::Lit(V::Bool(false)),
            "null" => return E::Lit(V::Null),
            _ => {}
        }
        if self.eat("(") {
            let mut args = Vec::new();
            if !self.eat(")") {
                loop {
                    args.push(self.or());
                    if self.eat(")") {
                        break;
                    }
                    assert!(self.eat(","), "a call's arguments");
                }
            }
            return E::Call(w.to_owned(), args);
        }
        E::Ctx(w.to_owned())
    }
}

#[test]
fn expressions_work_out_as_githubs_do() {
    let ctx: Ctx = [
        ("a.b", V::from("x")),
        ("n", V::from(12)),
        ("t", V::from(true)),
        ("nil", V::Null),
        ("cancelled()", V::from(false)),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v))
    .collect();
    let v = |e: &str| value(e, &ctx);
    assert_eq!(v("${{ a.b == 'X' }}"), V::Bool(true));
    assert_eq!(v("${{ a.b != 'x' }}"), V::Bool(false));
    assert_eq!(v("${{ nil && 'y' || 'z' }}"), V::from("z"));
    assert_eq!(v("${{ t && 'y' || 'z' }}"), V::from("y"));
    assert_eq!(v("${{ !cancelled() && n == '12' }}"), V::Bool(true));
    assert_eq!(v("${{ format('{0}-{1}{0}', n, t) }}"), V::from("12-true12"));
    assert_eq!(v("${{ 'it''s' }}"), V::from("it's"));
    assert_eq!(v("pr ${{ n }} of ${{ a.b }}"), V::from("pr 12 of x"));
    assert_eq!(v("${{ !(t && nil) }}"), V::Bool(true));
    assert_eq!(
        v("${{ n > 1 && n <= 12 && !(n < 12) && n >= '12' }}"),
        V::Bool(true)
    );
    assert_eq!(
        v("${{ n > 12 || n < 12 || n >= 13 || n <= 11 }}"),
        V::Bool(false)
    );
    assert_eq!(v("${{ a.b < 'Y' && 'b' > 'A' }}"), V::Bool(true));
    assert_eq!(v("${{ a.b > 1 || a.b <= 1 }}"), V::Bool(false));
    assert_eq!(
        v("${{ nil < 1 && t > nil && n > 1 == true }}"),
        V::Bool(true)
    );
    assert!(!condition("nil", &ctx));
}
