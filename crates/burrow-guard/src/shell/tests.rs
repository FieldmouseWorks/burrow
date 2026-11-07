//! The shell reading's own tests.

use super::{DEPTH, TooDeep, UNKNOWN, commands};

/// The commands, with `%` for a substitution's [`UNKNOWN`].
fn want(cmds: &[&[&str]]) -> Result<Vec<Vec<String>>, TooDeep> {
    Ok(cmds
        .iter()
        .map(|c| {
            c.iter()
                .map(|w| w.replace('%', &UNKNOWN.to_string()))
                .collect()
        })
        .collect())
}

#[test]
fn an_unquoted_glob_or_brace_is_unknown_and_a_quoted_one_is_text() {
    // `%` is UNKNOWN, placed ahead of the glob character or at the
    // closing brace of a list.
    assert_eq!(
        commands(r#"a b* c? d[1] {e,f} {g..h} {i} 'j*' k\? "{l,m}" ${n:-*}"#),
        want(&[&[
            "a", "b%*", "c%?", "d%[1]", "{e,f%}", "{g..h%}", "{i}", "j*", "k?", "{l,m}", "${n:-*}"
        ]])
    );
}

#[test]
fn a_brace_list_is_unknown_at_every_depth() {
    // An inner pair that isn't a list leaves the outer one's open; each
    // list nested in another is marked, and so is the one holding it.
    assert_eq!(
        commands(r"{{},a,b} x{a,{b,c}}y {{a..c},d} {a,{b}} {{}} {a,'{'b} \{a,b} {'a,b'} {a,b\}"),
        want(&[&[
            "{{},a,b%}",
            "x{a,{b,c%}%}y",
            "{{a..c%},d%}",
            "{a,{b}%}",
            "{{}}",
            "{a,{b%}",
            "{a,b}",
            "{a,b}",
            "{a,b}"
        ]])
    );
    // In options and their values alike.
    assert_eq!(
        commands(
            "gh api -q {{},repos/o/r/git/refs/heads/main,-X,DELETE,--hostname} api.github.com"
        ),
        want(&[&[
            "gh",
            "api",
            "-q",
            "{{},repos/o/r/git/refs/heads/main,-X,DELETE,--hostname%}",
            "api.github.com"
        ]])
    );
}

#[test]
fn a_here_documents_end_word_is_its_own_text() {
    // A glob or brace list in it isn't expanded, so the body ends at the
    // same text, and the continued command after it is one command.
    for (doc, end) in [
        ("<<EOF?", "EOF?"),
        ("<<E*", "E*"),
        ("<<[E]", "[E]"),
        ("<<{E,F}", "{E,F}"),
        ("<<'E'?", "E?"),
        ("<<E\\*", "E*"),
        ("<<\"E\"{a,b}", "E{a,b}"),
        ("<<-E?", "\tE?"),
        ("<<$(x)", "$(x)"),
        ("<<${x:-a}b", "${x:-a}b"),
        ("<<E`x`", "E`x`"),
    ] {
        let text = format!("cat {doc}\nbody\n{end}\ngit push \\\n-f origin main");
        let read = commands(&text).unwrap();
        assert!(
            read.contains(
                &["git", "push", "-f", "origin", "main"]
                    .map(String::from)
                    .to_vec()
            ),
            "{text:?}: {read:?}"
        );
        assert!(
            !read.contains(&vec![end.trim_start().to_owned()]),
            "{text:?}: {read:?}"
        );
    }
    // In a substitution, where the end word can close it too.
    assert_eq!(
        commands("a \"$(cat <<E?\nb\nE?\n)\" c\ngit push \\\n-f\nd \"$(e <<{F,G}\n{F,G})\" f"),
        want(&[
            &["cat"],
            &["b"],
            &["a", "%", "c"],
            &["git", "push", "-f"],
            &["e"],
            &["d", "%", "f"]
        ])
    );
}

#[test]
fn words_split_as_the_shell_splits_them() {
    assert_eq!(
        commands("git push -f  origin\tmain"),
        want(&[&["git", "push", "-f", "origin", "main"]])
    );
    assert_eq!(
        commands(r#"a 'b c' "d e" f\ g ''"#),
        want(&[&["a", "b c", "d e", "f g", ""]])
    );
    assert_eq!(commands(r#"a'b'"c"d"#), want(&[&["abcd"]]));
    assert_eq!(
        commands(r#""a \"b\" \$c \x""#),
        want(&[&[r#"a "b" $c \x"#]])
    );
    assert_eq!(commands(r"'a \' b"), want(&[&[r"a \", "b"]]));
    assert_eq!(commands("a\\\nb"), want(&[&["ab"]]));
    // An unclosed quote runs to the end.
    assert_eq!(commands("a 'b c"), want(&[&["a", "b c"]]));
    // Only a space, a tab or a newline splits words.
    assert_eq!(
        commands("a\u{a0}b\u{b}c\u{c}d e"),
        want(&[&["a\u{a0}b\u{b}c\u{c}d", "e"]])
    );
}

#[test]
fn dollar_quotes_are_decoded() {
    assert_eq!(
        commands(r"$'git push\t-f' a$'\x6d\141i'n"),
        want(&[&["git push\t-f", "amain"]])
    );
    assert_eq!(
        commands(r#"$'\'\\\"\?\e\cA\q\x\777' $'a b"#),
        want(&[&["'\\\"?\x1b\x01\\q\\x\u{fffd}", "a b"]])
    );
    assert_eq!(commands(r#"$"a b"c"#), want(&[&["a bc"]]));
    // A byte, and a NUL ending only its own string.
    assert_eq!(commands(r"$'git\440push'"), want(&[&["git push"]]));
    assert_eq!(commands(r"a$'b\0c'd e"), want(&[&["abd", "e"]]));
    // A backslash takes the quote after it, whatever it escapes.
    assert_eq!(commands(r"$'\c' b"), want(&[&[r"\c", "b"]]));
    // Not inside double quotes, and not as a plain `$`.
    assert_eq!(commands(r#""$'a'" $ $x"#), want(&[&["$'a'", "$", "$x"]]));
}

#[test]
fn separators_end_a_command_unless_quoted() {
    assert_eq!(
        commands("a; b && c || d | e & f\ng"),
        want(&[&["a"], &["b"], &["c"], &["d"], &["e"], &["f"], &["g"]])
    );
    assert_eq!(
        commands("a 'b;c|d&e' \"f\ng\""),
        want(&[&["a", "b;c|d&e", "f\ng"]])
    );
    assert_eq!(commands("(a; (b)) c"), want(&[&["a"], &["b"], &["c"]]));
}

#[test]
fn a_substitution_is_commands_of_its_own() {
    // The words around it stay with their command.
    assert_eq!(
        commands("a $(b c) d `e f` g"),
        want(&[&["b", "c"], &["e", "f"], &["a", "%", "d", "%", "g"]])
    );
    assert_eq!(commands("x=$(a b)y"), want(&[&["a", "b"], &["x=%y"]]));
    // Its quotes are its own, inside double quotes too.
    assert_eq!(
        commands(r#"a "x $(b "c d" e) y" f"#),
        want(&[&["b", "c d", "e"], &["a", "x % y", "f"]])
    );
    assert_eq!(
        commands(r#"a "`b \"c d\"`" e"#),
        want(&[&["b", "c d"], &["a", "%", "e"]])
    );
    // Nested, with subshells and arithmetic inside.
    assert_eq!(
        commands("a $(b $(c); (d); echo $((1+2))) e"),
        want(&[
            &["c"],
            &["b", "%"],
            &["d"],
            &["1+2"],
            &["echo", "%"],
            &["a", "%", "e"]
        ])
    );
    assert_eq!(
        commands(r"a `b \`c\` \$d`"),
        want(&[&["c"], &["b", "%", "$d"], &["a", "%"]])
    );
    // A process substitution is a word, not a redirect.
    assert_eq!(
        commands("a <(b c) >(d) e"),
        want(&[&["b", "c"], &["d"], &["a", "%", "%", "e"]])
    );
}

#[test]
fn redirects_and_their_targets_arent_words() {
    assert_eq!(commands("a b > log c"), want(&[&["a", "b", "c"]]));
    assert_eq!(
        commands("a 2>/dev/null b 2>&1 c >>log <in d"),
        want(&[&["a", "b", "c", "d"]])
    );
    assert_eq!(commands("a x2>y b"), want(&[&["a", "x2", "b"]]));
    assert_eq!(commands("a '2'>y b"), want(&[&["a", "2", "b"]]));
    // `-` is the target that closes a descriptor.
    assert_eq!(commands("a 3>&- b <&- c"), want(&[&["a", "b", "c"]]));
    assert_eq!(
        commands("a &>log b &>>log c <<<x d <<EOF e"),
        want(&[&["a", "b", "c", "d", "e"], &["x"]])
    );
    // What a target runs is read all the same.
    assert_eq!(
        commands(r#"a <"$(b c)" d > `e` f < <(g)"#),
        want(&[&["b", "c"], &["e"], &["g"], &["a", "d", "f"]])
    );
    // A here-string's text is no word of its command, and stands apart.
    assert_eq!(
        commands("a <<< 'b c' d; e"),
        want(&[&["a", "d"], &["b c"], &["e"]])
    );
    assert_eq!(commands("<<<a"), want(&[&["a"]]));
    // A redirect left without a target takes nothing from the next
    // command.
    assert_eq!(commands("a >; b"), want(&[&["a"], &["b"]]));
}

#[test]
fn a_case_pattern_doesnt_end_a_substitution() {
    let inner = ["case", "x", "in", "x"];
    for (text, words) in [
        ("a $(case x in x) b ;; esac) c", vec!["a", "%", "c"]),
        // Its optional `(`, its alternatives, and every way to the next.
        ("a $(case x in (x) b ;; esac) c", vec!["a", "%", "c"]),
        ("a $(case x in x|y) b ;; esac) c", vec!["a", "%", "c"]),
        ("a $(case x in x) b ;& esac) c", vec!["a", "%", "c"]),
        ("a $(case x in x) b ;;& esac) c", vec!["a", "%", "c"]),
        ("a $(case x in x) b; esac) c", vec!["a", "%", "c"]),
        ("a $(case x in\nx)\nb\nesac\n) c", vec!["a", "%", "c"]),
        ("a <(case x in x) b ;; esac) c", vec!["a", "%", "c"]),
    ] {
        let read = commands(text).unwrap();
        let last: Vec<_> = read
            .last()
            .unwrap()
            .iter()
            .map(|w| w.replace(UNKNOWN, "%"))
            .collect();
        assert_eq!(last, words, "{text}");
        assert!(read.contains(&vec!["b".to_owned()]), "{text}: {read:?}");
        assert_eq!(read[0][..2], inner[..2], "{text}");
    }
    // A second pattern after the first's body, and `esac` as a pattern.
    for text in [
        "a $(case x in y) d ;; z) b ;; esac) c",
        "a $(case x in y|esac) d ;; (esac) b ;; esac) c",
        "a $(case x in @(y|z)) b ;; esac) c",
        "a $(case x in z) (d) ; b ;; esac) c",
        "a $(if t; then case x in z) b ;; esac; fi) c",
        "a $(function f { case x in z) b ;; esac; }) c",
        "a $(case x in z) case y in w) b ;; esac ;; esac) c",
        "a $(case x in esac; b) c",
        "a $(case ')' in ')') b ;; esac) c",
    ] {
        let read = commands(text).unwrap();
        assert_eq!(read.last().unwrap().len(), 3, "{text}: {read:?}");
        assert!(read.contains(&vec!["b".to_owned()]), "{text}: {read:?}");
    }
    // Not the keyword: a quoted or an argument `case` opens nothing.
    assert_eq!(
        commands("a $('case' x in x) b"),
        want(&[&["case", "x", "in", "x"], &["a", "%", "b"]])
    );
    assert_eq!(
        commands("a $(echo case x in x) b"),
        want(&[&["echo", "case", "x", "in", "x"], &["a", "%", "b"]])
    );
}

#[test]
fn a_comment_holds_no_quotes_or_parentheses() {
    // `)` and `'` in a comment are its own; what it says is read too.
    assert_eq!(
        commands("a $(b # c) d\ne) f"),
        want(&[&["c"], &["d"], &["b"], &["e"], &["a", "%", "f"]])
    );
    assert_eq!(
        commands("a # don't\nb c"),
        want(&[&["dont"], &["a"], &["b", "c"]])
    );
    // Only at the start of a word.
    assert_eq!(commands("a b#c $#"), want(&[&["a", "b#c", "$#"]]));
}

#[test]
fn a_here_document_holds_no_quotes_or_parentheses() {
    assert_eq!(
        commands("a <<EOF\ndon't\n)\nEOF\nb c"),
        want(&[&["a"], &["dont"], &["b", "c"]])
    );
    // Closing a substitution only after its body, or with its end
    // word, as bash does.
    assert_eq!(
        commands("a $(b <<EOF\n)\nEOF\nc) d"),
        want(&[&["b"], &["c"], &["a", "%", "d"]])
    );
    assert_eq!(
        commands("a \"$(b <<EOF\nc\nEOF)\" d $(e <<-EOF\n\tEOF) f"),
        want(&[&["b"], &["c"], &["e"], &["a", "%", "d", "%", "f"]])
    );
    // Its body is read, its end word's quotes come off, `<<-` strips tabs.
    assert_eq!(
        commands("bash <<'E F'\ngit push\nE F\nb"),
        want(&[&["bash"], &["git", "push"], &["b"]])
    );
    assert_eq!(
        commands("a <<-EOF c\n\tb\n\tEOF\nd"),
        want(&[&["a", "c"], &["b"], &["d"]])
    );
    // Each body in turn, and one that never ends runs out with the text.
    assert_eq!(
        commands("a <<X <<Y\nb\nX\nc\nY\nd"),
        want(&[&["a"], &["b"], &["c"], &["d"]])
    );
    assert_eq!(commands("a <<X\nb"), want(&[&["a"], &["b"]]));
}

#[test]
fn a_parameter_holds_its_parentheses() {
    assert_eq!(
        commands("a $(b ${x:-)} c) d"),
        want(&[&["b", "${x:-)}", "c"], &["a", "%", "d"]])
    );
    assert_eq!(commands("${x:-$(a b)}"), want(&[&["a", "b"], &["${x:-%}"]]));
}

#[test]
fn a_parameters_text_comes_after_its_operator() {
    for (word, texts) in [
        ("${x:-a b}", vec!["a b"]),
        ("${x-a}", vec!["a"]),
        ("${x:=a}", vec!["a"]),
        ("${x:+a}", vec!["a"]),
        ("${x:--f}", vec!["-f"]),
        ("${x/a/b c}", vec!["a/b c"]),
        ("${a[0]:-b}", vec!["b"]),
        ("${@:1}", vec!["1"]),
        ("p${x:-a}q${y:-b}r", vec!["a", "b"]),
        ("${x:-${y:-a}}", vec!["${y:-a}"]),
        ("${x:-a", vec!["a"]),
        // No text: nothing but what it names.
        ("${x}", vec![]),
        ("${#x}", vec![]),
        ("${!x}", vec![]),
        ("${@}", vec![]),
        ("${#}", vec![]),
        ("${x:-}", vec![]),
        ("${a[@]}", vec![]),
        ("$x", vec![]),
    ] {
        assert_eq!(super::parameter_texts(word), texts, "{word}");
    }
    assert!(super::is_only_a_parameter_text("${x:-a}"));
    assert!(!super::is_only_a_parameter_text("${x}"));
    assert!(!super::is_only_a_parameter_text("p${x:-a}"));
    assert!(!super::is_only_a_parameter_text("${x:-a}${y:-b}"));
}

#[test]
fn nesting_past_the_limit_is_refused() {
    assert_eq!(commands(&"$(".repeat(DEPTH + 1)), Err(TooDeep));
    assert!(commands(&"$(".repeat(DEPTH)).is_ok());
    assert_eq!(commands(&"${".repeat(DEPTH + 1)), Err(TooDeep));
}
