//! Strip Rust inline `#[cfg(test)]` items and fields when counting lines for
//! `Rule::FileSize`.
//!
//! Scanning is grammar- and delimiter-aware so a `#[cfg(test)]` target can
//! never escape an enclosing `}`, `)`, or `]` or swallow production code
//! following a semicolon-terminated item that contains braces.

use crate::rules::line_count;

/// How many non-test code lines a Rust source file has for `Rule::FileSize`.
/// Lines belonging solely to inline `#[cfg(test)]` targets don't count.
pub fn code_lines(content: &[u8]) -> usize {
    if content.is_empty() {
        return 0;
    }
    let ranges = rust_cfg_test_ranges(content);
    if ranges.is_empty() {
        return line_count(content);
    }
    let mut kept = 0usize;
    let mut ls = 0usize;
    while ls < content.len() {
        let nl = content[ls..]
            .iter()
            .position(|b| *b == b'\n')
            .map_or(content.len(), |off| ls + off);
        let le = if nl > ls && content[nl - 1] == b'\r' {
            nl - 1
        } else {
            nl
        };
        let excluded = if ls == le {
            ranges.iter().any(|&(s, e)| s < ls && ls < e)
        } else {
            let mut has_non_ws = false;
            let mut all_in_test = true;
            for (idx, &byte) in content[ls..le].iter().enumerate() {
                if !byte.is_ascii_whitespace() {
                    has_non_ws = true;
                    let pos = ls + idx;
                    if !ranges.iter().any(|&(s, e)| s <= pos && pos < e) {
                        all_in_test = false;
                        break;
                    }
                }
            }
            if has_non_ws {
                all_in_test
            } else {
                ranges.iter().any(|&(s, e)| s < ls && le <= e)
            }
        };
        if !excluded {
            kept += 1;
        }
        ls = if nl < content.len() {
            nl + 1
        } else {
            content.len()
        };
    }
    kept
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn is_ident_start_byte(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_' || b >= 0x80
}

fn is_ident_continue_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80
}

fn skip_non_code(b: &[u8], i: usize) -> Option<usize> {
    if b[i..].starts_with(b"//") {
        let mut j = i + 2;
        while j < b.len() && b[j] != b'\n' {
            j += 1;
        }
        return Some(j);
    }
    if b[i..].starts_with(b"/*") {
        let mut j = i + 2;
        let mut depth = 1usize;
        while j < b.len() && depth > 0 {
            if b[j..].starts_with(b"/*") {
                depth += 1;
                j += 2;
            } else if b[j..].starts_with(b"*/") {
                depth -= 1;
                j += 2;
            } else {
                j += 1;
            }
        }
        return Some(j);
    }
    // Raw string literals: r"...", r#"..."#, br#"..."#, cr#"..."#.
    let prev_ident = i > 0 && is_ident_byte(b[i - 1]);
    if !prev_ident {
        let prefix = if b[i] == b'r' {
            Some(1)
        } else if (b[i] == b'b' || b[i] == b'c') && b.get(i + 1) == Some(&b'r') {
            Some(2)
        } else {
            None
        };
        if let Some(plen) = prefix {
            let mut k = i + plen;
            let mut hashes = 0usize;
            while b.get(k) == Some(&b'#') {
                hashes += 1;
                k += 1;
            }
            if b.get(k) == Some(&b'"') {
                k += 1;
                while k < b.len() {
                    if b[k] == b'"' {
                        let mut matched = 0usize;
                        while matched < hashes && b.get(k + 1 + matched) == Some(&b'#') {
                            matched += 1;
                        }
                        if matched == hashes {
                            return Some(k + 1 + hashes);
                        }
                    }
                    k += 1;
                }
                return Some(b.len());
            }
        }
    }
    if b[i] == b'"' {
        let mut j = i + 1;
        while j < b.len() {
            if b[j] == b'\\' {
                j = (j + 2).min(b.len());
            } else if b[j] == b'"' {
                return Some(j + 1);
            } else {
                j += 1;
            }
        }
        return Some(b.len());
    }
    if b[i] == b'\'' {
        if b.get(i + 1) == Some(&b'\\') {
            let mut j = (i + 3).min(b.len());
            let limit = (i + 14).min(b.len());
            while j < limit && b[j] != b'\n' {
                if b[j] == b'\'' {
                    return Some(j + 1);
                }
                j += 1;
            }
            return Some(i + 1);
        }
        if let Some(&first) = b.get(i + 1)
            && first != b'\''
            && first != b'\n'
        {
            let width = if first < 0x80 {
                1
            } else if first & 0xE0 == 0xC0 {
                2
            } else if first & 0xF0 == 0xE0 {
                3
            } else if first & 0xF8 == 0xF0 {
                4
            } else {
                1
            };
            if b.get(i + 1 + width) == Some(&b'\'') {
                return Some(i + 2 + width);
            }
        }
    }
    None
}

fn skip_ws_and_comments(b: &[u8], mut i: usize) -> usize {
    while i < b.len() {
        if b[i].is_ascii_whitespace() {
            i += 1;
        } else if b[i..].starts_with(b"//") || b[i..].starts_with(b"/*") {
            i = skip_non_code(b, i).unwrap_or(i + 1);
        } else {
            break;
        }
    }
    i
}

/// Skip a balanced `( ... )`, `[ ... ]`, or `{ ... }` group starting at `i`
/// (where `b[i] == open`), returning the index right after the matching `close`.
fn skip_balanced(b: &[u8], mut i: usize, open: u8, close: u8) -> usize {
    if b.get(i) != Some(&open) {
        return i;
    }
    let mut depth = 1usize;
    i += 1;
    while i < b.len() && depth > 0 {
        if let Some(next) = skip_non_code(b, i) {
            i = next;
            continue;
        }
        if b[i] == open {
            depth += 1;
        } else if b[i] == close {
            depth -= 1;
        }
        i += 1;
    }
    i
}

/// Skip any stacked outer attributes `#[...]` (and surrounding whitespace /
/// comments) attached to the same target.
fn skip_attrs(b: &[u8], mut j: usize) -> usize {
    loop {
        j = skip_ws_and_comments(b, j);
        if b[j..].starts_with(b"#[") {
            j = skip_balanced(b, j + 1, b'[', b']');
        } else {
            break;
        }
    }
    j
}

/// Read an unescaped ASCII keyword/identifier at `i`, rejecting raw identifiers
/// (`r#...`) and string prefixes (`r"..."`, `b"..."`, etc.).
fn next_keyword(b: &[u8], i: usize) -> Option<(&[u8], usize)> {
    if b[i..].starts_with(b"r#") {
        return None;
    }
    let &first = b.get(i)?;
    if !(first.is_ascii_alphabetic() || first == b'_') {
        return None;
    }
    let mut j = i + 1;
    while b.get(j).is_some_and(|&c| is_ident_byte(c)) {
        j += 1;
    }
    let word = &b[i..j];
    if matches!(word, b"r" | b"b" | b"c" | b"br" | b"cr")
        && matches!(b.get(j), Some(b'"' | b'\'' | b'#'))
    {
        return None;
    }
    Some((word, j))
}

/// Skip any Rust identifier at `i` (including raw `r#ident` and non-ASCII
/// identifiers), returning the end offset.
fn skip_ident(b: &[u8], i: usize) -> Option<usize> {
    if b[i..].starts_with(b"r#") {
        let &first = b.get(i + 2)?;
        if !is_ident_start_byte(first) {
            return None;
        }
        let mut j = i + 3;
        while b.get(j).is_some_and(|&c| is_ident_continue_byte(c)) {
            j += 1;
        }
        return Some(j);
    }
    let &first = b.get(i)?;
    if !is_ident_start_byte(first) {
        return None;
    }
    let mut j = i + 1;
    while b.get(j).is_some_and(|&c| is_ident_continue_byte(c)) {
        j += 1;
    }
    let word = &b[i..j];
    if matches!(word, b"r" | b"b" | b"c" | b"br" | b"cr")
        && matches!(b.get(j), Some(b'"' | b'\'' | b'#'))
    {
        return None;
    }
    Some(j)
}

fn match_cfg_test_attr(b: &[u8], i: usize) -> Option<usize> {
    if !b[i..].starts_with(b"#[") {
        return None;
    }
    let mut j = skip_ws_and_comments(b, i + 2);
    if !b[j..].starts_with(b"cfg") || b.get(j + 3).is_some_and(|&c| is_ident_byte(c)) {
        return None;
    }
    j = skip_ws_and_comments(b, j + 3);
    if b.get(j) != Some(&b'(') {
        return None;
    }
    j = skip_ws_and_comments(b, j + 1);
    if !b[j..].starts_with(b"test") || b.get(j + 4).is_some_and(|&c| is_ident_byte(c)) {
        return None;
    }
    j = skip_ws_and_comments(b, j + 4);
    if b.get(j) != Some(&b')') {
        return None;
    }
    j = skip_ws_and_comments(b, j + 1);
    if b.get(j) != Some(&b']') {
        return None;
    }
    Some(j + 1)
}

/// If `start` begins a macro invocation (`path!(...)`, `path![...]`,
/// `path!{...}`), return the byte offset of its opening delimiter.
fn match_macro_call(b: &[u8], mut j: usize) -> Option<usize> {
    if b[j..].starts_with(b"::") {
        j = skip_ws_and_comments(b, j + 2);
    }
    j = skip_ident(b, j)?;
    loop {
        let next = skip_ws_and_comments(b, j);
        if b[next..].starts_with(b"::") {
            let after_sep = skip_ws_and_comments(b, next + 2);
            j = skip_ident(b, after_sep)?;
        } else {
            j = next;
            break;
        }
    }
    if b.get(j) != Some(&b'!') || b.get(j + 1) == Some(&b'=') {
        return None;
    }
    let open = skip_ws_and_comments(b, j + 1);
    matches!(b.get(open), Some(b'(' | b'[' | b'{')).then_some(open)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    /// Always ends at `;` at delimiter depth 0 (`const`, `static`, `type`,
    /// `use`, `let`, `extern crate`), regardless of any `{ ... }` inside.
    Semicolon,
    /// Item that ends at its body `{ ... }` or `;` at delimiter depth 0 (`mod`,
    /// `fn`, `struct`, `enum`, `union`, `trait`, `impl`, `extern { ... }`,
    /// `macro_rules!`).
    ItemBraceOrSemicolon,
    /// Macro invocation `path!(...)`, `path![...]`, or `path!{...}`.
    MacroCall { open_pos: usize },
    /// Anything else: struct/tuple field, enum variant, function parameter,
    /// match arm, expression, or statement. Ends at `,`, `;`, `{ ... }`, or
    /// before an enclosing closing delimiter.
    NonItem,
}

fn classify_target(b: &[u8], after_attr: usize) -> (Target, usize) {
    let start = skip_attrs(b, after_attr);
    let mut j = start;

    // Optional visibility: `pub` or `pub(...)`.
    if let Some((b"pub", next)) = next_keyword(b, j) {
        j = skip_ws_and_comments(b, next);
        if b.get(j) == Some(&b'(') {
            j = skip_ws_and_comments(b, skip_balanced(b, j, b'(', b')'));
        }
    }

    // Optional specialization qualifier `default` (weak keyword: only a
    // qualifier before an item keyword, never a field named `default`).
    if let Some((b"default", next)) = next_keyword(b, j) {
        let after = skip_ws_and_comments(b, next);
        if matches!(
            next_keyword(b, after).map(|(w, _)| w),
            Some(b"fn" | b"const" | b"type" | b"impl" | b"unsafe" | b"async" | b"extern" | b"auto")
        ) {
            j = after;
        }
    }

    // Optional `async` qualifier before `fn` / `unsafe` / `extern`.
    if let Some((b"async", next)) = next_keyword(b, j) {
        let after = skip_ws_and_comments(b, next);
        if matches!(
            next_keyword(b, after).map(|(w, _)| w),
            Some(b"fn" | b"unsafe" | b"extern")
        ) {
            j = after;
        }
    }

    // Optional `const` qualifier before `fn` / `unsafe` / `extern` / `trait` / `impl`.
    if let Some((b"const", next)) = next_keyword(b, j) {
        let after = skip_ws_and_comments(b, next);
        if matches!(
            next_keyword(b, after).map(|(w, _)| w),
            Some(b"fn" | b"unsafe" | b"extern" | b"trait" | b"impl")
        ) {
            j = after;
        }
    }

    // Optional `unsafe` qualifier before `fn` / `trait` / `impl` / `extern`.
    if let Some((b"unsafe", next)) = next_keyword(b, j) {
        let after = skip_ws_and_comments(b, next);
        if matches!(
            next_keyword(b, after).map(|(w, _)| w),
            Some(b"fn" | b"trait" | b"impl" | b"extern")
        ) {
            j = after;
        }
    }

    // Optional `auto` qualifier before `trait`.
    if let Some((b"auto", next)) = next_keyword(b, j) {
        let after = skip_ws_and_comments(b, next);
        if matches!(next_keyword(b, after).map(|(w, _)| w), Some(b"trait")) {
            j = after;
        }
    }

    let target = match next_keyword(b, j) {
        Some((b"extern", next)) => {
            let mut k = skip_ws_and_comments(b, next);
            if matches!(next_keyword(b, k).map(|(w, _)| w), Some(b"crate")) {
                Target::Semicolon
            } else {
                if matches!(b.get(k), Some(b'"' | b'r'))
                    && let Some(after_abi) = skip_non_code(b, k)
                {
                    k = skip_ws_and_comments(b, after_abi);
                }
                if b.get(k) == Some(&b'{') {
                    Target::ItemBraceOrSemicolon
                } else if let Some((b"fn", after_fn)) = next_keyword(b, k)
                    && skip_ident(b, skip_ws_and_comments(b, after_fn)).is_some()
                {
                    Target::ItemBraceOrSemicolon
                } else {
                    Target::NonItem
                }
            }
        }
        Some((b"const", next)) => {
            // `const NAME: T = ...;` is an item; `const { ... }` is an inline
            // const block expression.
            let k = skip_ws_and_comments(b, next);
            if skip_ident(b, k).is_some() {
                Target::Semicolon
            } else {
                Target::NonItem
            }
        }
        Some((b"static" | b"type" | b"use" | b"let", _)) => Target::Semicolon,
        Some((b"mod" | b"fn" | b"struct" | b"enum" | b"union" | b"trait", next)) => {
            // Require a following identifier so `fn(i32)` types and fields
            // named `union` aren't misclassified as items.
            let k = skip_ws_and_comments(b, next);
            if skip_ident(b, k).is_some() {
                Target::ItemBraceOrSemicolon
            } else {
                Target::NonItem
            }
        }
        Some((b"impl", _)) => Target::ItemBraceOrSemicolon,
        Some((b"macro_rules", next)) => {
            let k = skip_ws_and_comments(b, next);
            if b.get(k) == Some(&b'!') && b.get(k + 1) != Some(&b'=') {
                Target::ItemBraceOrSemicolon
            } else {
                Target::NonItem
            }
        }
        _ => match match_macro_call(b, start) {
            Some(open_pos) => Target::MacroCall { open_pos },
            None => Target::NonItem,
        },
    };
    (target, start)
}

fn scan_semicolon(b: &[u8], mut j: usize) -> usize {
    let mut brace_depth = 0usize;
    let mut bracket_depth = 0usize;
    let mut paren_depth = 0usize;
    while j < b.len() {
        if let Some(next) = skip_non_code(b, j) {
            j = next;
            continue;
        }
        match b[j] {
            b'[' => {
                bracket_depth += 1;
                j += 1;
            }
            b']' => {
                if bracket_depth == 0 {
                    return j;
                }
                bracket_depth -= 1;
                j += 1;
            }
            b'(' => {
                paren_depth += 1;
                j += 1;
            }
            b')' => {
                if paren_depth == 0 {
                    return j;
                }
                paren_depth -= 1;
                j += 1;
            }
            b'{' => {
                brace_depth += 1;
                j += 1;
            }
            b'}' => {
                if brace_depth == 0 {
                    return j;
                }
                brace_depth -= 1;
                j += 1;
            }
            b';' if brace_depth == 0 && bracket_depth == 0 && paren_depth == 0 => {
                return j + 1;
            }
            _ => j += 1,
        }
    }
    b.len()
}

fn scan_item_brace_or_semicolon(b: &[u8], mut j: usize) -> usize {
    let mut brace_depth = 0usize;
    let mut bracket_depth = 0usize;
    let mut paren_depth = 0usize;
    while j < b.len() {
        if let Some(next) = skip_non_code(b, j) {
            j = next;
            continue;
        }
        match b[j] {
            b'[' => {
                bracket_depth += 1;
                j += 1;
            }
            b']' => {
                if bracket_depth == 0 {
                    return j;
                }
                bracket_depth -= 1;
                j += 1;
            }
            b'(' => {
                paren_depth += 1;
                j += 1;
            }
            b')' => {
                if paren_depth == 0 {
                    return j;
                }
                paren_depth -= 1;
                j += 1;
            }
            b'{' => {
                brace_depth += 1;
                j += 1;
            }
            b'}' => {
                if brace_depth == 0 {
                    return j;
                }
                brace_depth -= 1;
                j += 1;
                if brace_depth == 0 && bracket_depth == 0 && paren_depth == 0 {
                    let k = skip_ws_and_comments(b, j);
                    // A const-generic `{ expr }` block inside `<...>` is always
                    // followed by `>` or `,`; an item body never is.
                    if matches!(b.get(k), Some(b'>' | b',')) {
                        continue;
                    }
                    if b.get(k) == Some(&b';') {
                        return k + 1;
                    }
                    return j;
                }
            }
            b';' if brace_depth == 0 && bracket_depth == 0 && paren_depth == 0 => {
                return j + 1;
            }
            _ => j += 1,
        }
    }
    b.len()
}

fn scan_macro_call(b: &[u8], open_pos: usize) -> usize {
    let close = match b.get(open_pos) {
        Some(b'(') => b')',
        Some(b'[') => b']',
        Some(b'{') => b'}',
        _ => return open_pos,
    };
    let after = skip_balanced(b, open_pos, b[open_pos], close);
    let k = skip_ws_and_comments(b, after);
    if matches!(b.get(k), Some(b';' | b',')) {
        return k + 1;
    }
    after
}

fn scan_non_item(b: &[u8], mut j: usize) -> usize {
    let mut brace_depth = 0usize;
    let mut bracket_depth = 0usize;
    let mut paren_depth = 0usize;
    while j < b.len() {
        if let Some(next) = skip_non_code(b, j) {
            j = next;
            continue;
        }
        match b[j] {
            b'[' => {
                bracket_depth += 1;
                j += 1;
            }
            b']' => {
                if bracket_depth == 0 {
                    return j;
                }
                bracket_depth -= 1;
                j += 1;
            }
            b'(' => {
                paren_depth += 1;
                j += 1;
            }
            b')' => {
                if paren_depth == 0 {
                    return j;
                }
                paren_depth -= 1;
                j += 1;
            }
            b'{' => {
                brace_depth += 1;
                j += 1;
            }
            b'}' => {
                if brace_depth == 0 {
                    return j;
                }
                brace_depth -= 1;
                j += 1;
                if brace_depth == 0 && bracket_depth == 0 && paren_depth == 0 {
                    let k = skip_ws_and_comments(b, j);
                    if b[k..].starts_with(b"=>") {
                        j = k + 2;
                        continue;
                    }
                    if let Some((b"else", after_else)) = next_keyword(b, k) {
                        j = after_else;
                        continue;
                    }
                    if matches!(b.get(k), Some(b',' | b';')) {
                        return k + 1;
                    }
                    return j;
                }
            }
            b',' | b';' if brace_depth == 0 && bracket_depth == 0 && paren_depth == 0 => {
                return j + 1;
            }
            _ => j += 1,
        }
    }
    b.len()
}

fn find_target_end(b: &[u8], after_attr: usize) -> usize {
    let (target, start) = classify_target(b, after_attr);
    match target {
        Target::Semicolon => scan_semicolon(b, start),
        Target::ItemBraceOrSemicolon => scan_item_brace_or_semicolon(b, start),
        Target::MacroCall { open_pos } => scan_macro_call(b, open_pos),
        Target::NonItem => scan_non_item(b, start),
    }
}

fn rust_cfg_test_ranges(b: &[u8]) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut i = 0usize;
    while i < b.len() {
        if let Some(next) = skip_non_code(b, i) {
            i = next;
            continue;
        }
        if let Some(after) = match_cfg_test_attr(b, i) {
            let end = find_target_end(b, after);
            ranges.push((i, end));
            i = end;
            continue;
        }
        i += 1;
    }
    ranges
}

#[cfg(test)]
mod tests {
    use super::code_lines;

    #[test]
    fn counts_exact_production_lines_around_cfg_test_targets() {
        let struct_with_field = "\
struct Config {
    #[cfg(test)]
    fake: bool,
    real: String,
}
impl Config {
    fn run(&self) {}
}
";
        assert_eq!(code_lines(struct_with_field.as_bytes()), 6);

        let last_field_no_comma = "\
struct Config {
    real: String,
    #[cfg(test)]
    fake: bool
}
fn prod() {}
";
        assert_eq!(code_lines(last_field_no_comma.as_bytes()), 4);

        let const_slice_with_braces = "\
#[cfg(test)]
const ITEMS: &[S] = &[S { a: 1 }];
fn prod() {
    let x = 1;
}
";
        assert_eq!(code_lines(const_slice_with_braces.as_bytes()), 3);

        let doc_comment_after_cfg_test_mod = "\
#[cfg(test)]
mod tests {
    #[test]
    fn t() {}
}
// prod doc comment
fn prod() {}
";
        assert_eq!(code_lines(doc_comment_after_cfg_test_mod.as_bytes()), 2);
    }
}
