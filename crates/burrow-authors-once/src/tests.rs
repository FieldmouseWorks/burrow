use super::*;

fn one(path: &str, text: &str) -> Vec<String> {
    scan(&[File {
        path: path.into(),
        text: text.into(),
    }])
}

fn refused(path: &str, text: &str) {
    assert!(
        rnix::Root::parse(text).errors().is_empty(),
        "invalid test Nix: {text}"
    );
    assert!(!one(path, text).is_empty(), "accepted in {path}: {text}");
}

fn accepted(path: &str, text: &str) {
    let found = one(path, text);
    assert!(found.is_empty(), "refused in {path}: {found:?}");
}

#[test]
fn doors_read_the_one_list() {
    accepted(
        "modules/probe.nix",
        "{ x = { inherit (config.burrow) authors; repo = 1; }; }",
    );
    accepted(
        "modules/probe.nix",
        "{ x = { authors = config.burrow.authors; }; }",
    );
    accepted("modules/probe.nix", "{ a = config.burrow.authors != [ ]; }");
    accepted(
        "modules/probe.nix",
        "# authors = [ ] in a comment\n{ m = \"authors = [ ]\"; }",
    );
}

#[test]
fn a_second_option_is_refused() {
    let ty = "lib.types.listOf (import ../lib/github-author.nix { inherit lib; })";
    refused(
        "modules/probe.nix",
        &format!("{{ options.burrow.probe.authors = lib.mkOption {{ type = {ty}; }}; }}"),
    );
    // Qualified, not through `options`, and not named for the type.
    refused(
        "modules/x.nix",
        "{ options.burrow.x.authors = lib.mkOption { type = lib.types.listOf lib.types.attrs; }; }",
    );
    refused(
        "modules/x.nix",
        "{ options.burrow.x.\"authors\" = lib.mkOption { }; }",
    );
    refused(
        "modules/x.nix",
        "{ options.burrow.x.allowedAuthors = mkOption { }; }",
    );
    refused(
        "modules/x.nix",
        "{ options = { authors = mkOption { }; }; }",
    );
    refused(
        "modules/x.nix",
        "let t = import ../lib/github-author.nix { inherit lib; }; in { }",
    );
}

#[test]
fn a_door_taking_authors_from_elsewhere_is_refused() {
    for text in [
        "{ x = { inherit (cfg) authors; }; }",
        "{ x = { inherit (config.burrow.probe) authors; }; }",
        "{ x = { inherit authors; }; }",
        "{ x = { authors = cfg.authors; }; }",
        "{ x = { authors = [ { login = \"other\"; id = 7; } ]; }; }",
        "{ x = { authors = [\n  { login = \"other\";\n    id = 7; }\n]; }; }",
        "let a = [ ]; in { x = { authors = a; }; }",
        "{ x = { authors = config.burrow.authors ++ [ { login = \"o\"; id = 7; } ]; }; }",
        "{ x = { authors = lib.mkForce config.burrow.authors; }; }",
        "{ x = { authors = config.burrow.\"authors\"; }; }",
        "{ x = { \"authors\" = [ ]; }; }",
        "{ x = { inherit (config.burrow) authors extra; }; }",
        "{ x = { inherit (config.burrow) authors; authors = [ ]; }; }",
        "{ x = cfg.authors; }",
        "{ authors, ... }: { }",
    ] {
        for path in [
            "modules/probe.nix",
            "modules/ledger.nix",
            "modules/proj.nix",
            "modules/settings.nix",
            "modules/autoland.nix",
        ] {
            refused(path, text);
        }
    }
}

#[test]
fn the_steward_own_authors_are_exempt_and_only_those() {
    let own = "{ options.burrow.autoland = { authors = mkOption { type = types.listOf types.str; }; }; settings = { inherit (cfg) authors; }; }";
    accepted("modules/autoland.nix", own);
    refused("modules/probe.nix", own);
    // A door payload in the exempt file is not the steward's.
    refused(
        "modules/autoland.nix",
        "{ probe = { authors = [ { login = \"o\"; id = 7; } ]; }; }",
    );
    refused(
        "modules/autoland.nix",
        "{ probe = { inherit (cfg.other) authors; }; }",
    );
    refused(
        "modules/autoland.nix",
        "{ options.authors = mkOption { type = import ../lib/github-author.nix { }; }; }",
    );
    refused(
        "modules/autoland/shadow.nix",
        "{ a = cfg.authors; b = cfg.door.authors; }",
    );
}

#[test]
fn where_it_is_declared_is_scanned_too() {
    let decl = "imports = [ { options.burrow = { authors = mkOption { type = types.listOf (import ../lib/github-author.nix { inherit lib; }); default = [ ]; }; }; } ];";
    accepted("modules/common.nix", &format!("{{ {decl} }}"));
    // The removed option, declared in the home file or under an exempt path.
    refused(
        "modules/common.nix",
        &format!(
            "{{ {decl} options.burrow.probe.authors = mkOption {{ type = types.listOf types.attrs; }}; }}"
        ),
    );
    refused(
        "modules/common.nix",
        &format!("{{ {decl} options.burrow.probe = {{ authors = mkOption {{ }}; }}; }}"),
    );
    refused(
        "modules/autoland.nix",
        "{ options.burrow.probe = { authors = mkOption { type = types.listOf types.str; }; }; }",
    );
    refused(
        "modules/watchdog/degradations.nix",
        "{ options.burrow.autoland = { authors = mkOption { type = types.listOf types.str; }; }; }",
    );
}

#[test]
fn interpolated_code_is_scanned() {
    for text in [
        "{ x = \"${builtins.toJSON { authors = [ { login = \"o\"; id = 7; } ]; }}\"; }",
        "{ x = ''${builtins.toJSON { authors = [ ]; }}''; }",
        "{ x = \"a ${\"b ${cfg.authors}\"} c\"; }",
        "{ x = import \"${./y}/github-author.nix\"; }",
    ] {
        refused("modules/probe.nix", text);
    }
    accepted(
        "modules/probe.nix",
        "{ x = \"authors ${toString 1} data\"; }",
    );
}

#[test]
fn exemptions_are_scoped_to_their_bindings() {
    // The inbox's payload is accounts, not the steward's logins.
    refused(
        "modules/autoland.nix",
        "{ settings = { inbox = { inherit (cfg) authors; }; }; }",
    );
    accepted(
        "modules/autoland.nix",
        "{ settings = { inherit (cfg) authors; }; }",
    );
    refused(
        "modules/watchdog/degradations.nix",
        "{ other = { inherit (cfg) repos prThreshold authors; }; }",
    );
}

#[test]
fn quoted_names_and_comments_in_syntax() {
    refused("modules/x.nix", "{ \"authors\".allowed = mkOption { }; }");
    refused("modules/x.nix", "{ x.\"authors\".y = 1; }");
    refused(
        "modules/x.nix",
        "{ x = \"${ # }\n builtins.toJSON { authors = []; }\n}\"; }",
    );
    refused(
        "modules/x.nix",
        "{ x = \"${ /* } */ builtins.toJSON { authors = []; } }\"; }",
    );
    accepted(
        "modules/x.nix",
        "{ description = \"reads the github-author type, not imports it\"; }",
    );
}

#[test]
fn value_expressions_keep_every_owning_binding() {
    let wrappers = [
        "let unused = null; in PAYLOAD",
        "with builtins; PAYLOAD",
        "assert true; PAYLOAD",
        "f PAYLOAD",
        "f (g PAYLOAD)",
        "[ (let unused = null; in PAYLOAD) ]",
        "if true then PAYLOAD else {}",
        "x: PAYLOAD",
        "let unused = with builtins; null; in assert true; f (PAYLOAD)",
    ];
    for (path, payload) in [
        ("modules/autoland.nix", "{ inherit (cfg) authors; }"),
        (
            "modules/watchdog/degradations.nix",
            "{ inherit (cfg) repos prThreshold authors; }",
        ),
    ] {
        for wrapper in wrappers {
            let value = wrapper.replace("PAYLOAD", payload);
            // A wrapper cannot make an inbox payload look like its parent.
            refused(path, &format!("{{ settings = {{ inbox = {value}; }}; }}"));
            // Nor can it discard any other binding in the owning chain.
            refused(path, &format!("{{ other = {{ settings = {value}; }}; }}"));
            accepted(path, &format!("{{ settings = {value}; }}"));
        }
    }
    for wrapper in wrappers {
        let value = wrapper.replace("PAYLOAD", "{ inherit (settings) stateDir owner proj projectsDir authors bots app jobTimeout renovate; }");
        refused(
            "modules/autoland.nix",
            &format!("{{ upkeepConfig = {{ inbox = {value}; }}; }}"),
        );
        accepted(
            "modules/autoland.nix",
            &format!("{{ upkeepConfig = {value}; }}"),
        );
    }
}

#[test]
fn quoted_attributes_ignore_trivia_and_string_data_does_not_import() {
    for gap in [" ", "\n", " /* accounts */ ", " # accounts\n "] {
        for text in [
            format!("{{ \"authors\"{gap}= []; }}"),
            format!("{{ \"authors\"{gap}.{gap}allowed = mkOption {{}}; }}"),
            format!("{{ x{gap}.{gap}\"authors\"{gap}.{gap}y = []; }}"),
            format!(
                "{{ options{gap}.{gap}\"burrow\"{gap}.{gap}probe{gap}.{gap}\"authors\"{gap}= mkOption {{}}; }}"
            ),
            format!("{{ x = cfg{gap}.{gap}\"authors\"; }}"),
        ] {
            refused("modules/probe.nix", &text);
        }
    }
    for data in [
        "github-author.nix",
        "../lib/github-author.nix",
        "${./x}/github-author.nix",
    ] {
        accepted(
            "modules/probe.nix",
            &format!("{{ description = \"{data}\"; }}"),
        );
        accepted(
            "modules/probe.nix",
            &format!("{{ description = ''{data}''; }}"),
        );
        refused(
            "modules/probe.nix",
            &format!("{{ x = import \"{data}\"; }}"),
        );
        refused(
            "modules/probe.nix",
            &format!("{{ x = import (\"{data}\"); }}"),
        );
        refused(
            "modules/probe.nix",
            &format!("{{ x = import ''{data}''; }}"),
        );
    }
}

#[test]
fn every_exemption_has_its_full_structural_scope() {
    let cases = [
        (
            "modules/autoland/shadow.nix",
            "flowConfig",
            "{ trusted_authors = cfg.authors ++ []; settings = { review_authors = [ stewardLogin ]; verdict_authors = [ stewardLogin ]; people = cfg.authors; }; }",
        ),
        (
            "modules/watchdog/degradations.nix",
            "options.burrow.watchdog.degradations",
            "{ authors = mkOption { type = types.listOf types.str; default = config.burrow.autoland.authors or []; }; }",
        ),
        (
            "modules/watchdog/degradations.nix",
            "config.assertions",
            "[ { assertion = cfg.authors != []; } ]",
        ),
        (
            "modules/autoland.nix",
            "options.burrow.autoland",
            "{ authors = mkOption { type = types.listOf types.str; }; }",
        ),
        (
            "modules/autoland.nix",
            "config",
            "{ burrow.authors = [ { login = \"${cfg.app.slug}[bot]\"; id = cfg.app.accountId; } ]; }",
        ),
        (
            "modules/common.nix",
            "imports",
            "[ { options.burrow = { authors = mkOption { type = types.listOf (import ../lib/github-author.nix { inherit lib; }); }; }; } ]",
        ),
    ];
    for (path, owner, payload) in cases {
        for wrapper in [
            "let unused = null; in PAYLOAD",
            "with builtins; PAYLOAD",
            "assert true; PAYLOAD",
            "f (g PAYLOAD)",
            "[ PAYLOAD ]",
            "x: PAYLOAD",
        ] {
            let value = wrapper.replace("PAYLOAD", payload);
            accepted(path, &format!("{{ {owner} = {value}; }}"));
            refused(path, &format!("{{ {owner} = {{ inbox = {value}; }}; }}"));
        }
    }
    // Dots within a quoted component cannot impersonate an owning path.
    refused(
        "modules/autoland.nix",
        "{ \"options.burrow.autoland\" = { authors = mkOption { type = types.listOf types.str; }; }; }",
    );
    // Local bindings introduced by `let` retain their own names too.
    refused(
        "modules/autoland.nix",
        "{ settings = let inbox = { inherit (cfg) authors; }; in inbox; }",
    );
    refused(
        "modules/autoland.nix",
        "{ settings = with { inbox = { inherit (cfg) authors; }; }; {}; }",
    );
}

#[test]
fn string_import_context_and_nested_interpolations() {
    for importer in [
        "import",
        "builtins.import",
        "(import)",
        "builtins.\"import\"",
        "scopedImport {}",
        "builtins.scopedImport {}",
    ] {
        for gap in [" ", " /* import operand */ ", " # import operand\n"] {
            refused(
                "modules/probe.nix",
                &format!("{{ x = {importer}{gap}(\"github-author.nix\"); }}"),
            );
        }
    }
    accepted(
        "modules/probe.nix",
        "{ description = f \"github-author.nix\"; }",
    );
    accepted(
        "modules/probe.nix",
        "{ x = scopedImport { description = \"github-author.nix\"; } ./other.nix; }",
    );
    for expr in [
        " # }\n builtins.toJSON { authors = []; }\n",
        " /* } */ builtins.toJSON { authors = []; }",
        "\"nested ${ # }\n cfg.authors}\"",
        "''nested ${ /* } */ cfg.authors}''",
    ] {
        for (open, close) in [("\"", "\""), ("''", "''")] {
            refused(
                "modules/probe.nix",
                &format!("{{ x = {open}${{{expr}}}{close}; }}"),
            );
        }
    }
}

#[test]
fn literal_interpolations_cannot_hide_attribute_names_or_imports() {
    for text in [
        r#"{ "authors${suffix}" = []; }"#,
        r#"{ "auth${""}ors" = []; }"#,
        r#"{ ${"authors"} = []; }"#,
        r#"{ x.${"authors"}.y = []; }"#,
        r#"{ x = import "github-${"author"}.nix"; }"#,
    ] {
        refused("modules/probe.nix", text);
    }
    // A dynamic enclosing name cannot acquire the literal `settings` exemption.
    refused(
        "modules/autoland.nix",
        r#"{ "settings${suffix}" = { inherit (cfg) authors; }; }"#,
    );
}

#[test]
fn malformed_nix_is_refused() {
    assert!(!one("modules/probe.nix", "{ authors = [").is_empty());
}
