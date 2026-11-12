use super::*;

fn one(path: &str, text: &str) -> Vec<String> {
    scan(&[File {
        path: path.into(),
        text: text.into(),
    }])
}

fn refused(path: &str, text: &str) {
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
        refused("modules/probe.nix", text);
        refused("modules/ledger.nix", text);
    }
}

#[test]
fn the_steward_own_authors_are_exempt_and_only_those() {
    let own = "{ options.burrow.autoland = { authors = mkOption { type = types.listOf types.str; }; }; x = { inherit (cfg) authors; }; y = cfg.authors; }";
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
    let decl = "options.burrow = { authors = mkOption { type = types.listOf (import ../lib/github-author.nix { inherit lib; }); default = [ ]; }; };";
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
