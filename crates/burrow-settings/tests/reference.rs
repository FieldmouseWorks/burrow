use std::{path::Path, process::Command};

#[test]
fn the_reference_command_documents_every_option_and_its_read_door() {
    let output = Command::new(env!("CARGO_BIN_EXE_burrow-settings"))
        .arg("reference")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "reference command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("`proj settings show` prints the value in force"));
    assert!(!text.contains("`settings show`"));
    for setting in burrow_settings::schema::SETTINGS {
        assert_eq!(
            text.matches(&format!("| `{}` |", setting.name)).count(),
            1,
            "{} must have exactly one reference row",
            setting.name
        );
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    assert_eq!(
        text,
        std::fs::read_to_string(root.join("docs/settings-reference.md")).unwrap(),
        "regenerate the reference with burrow-settings reference"
    );
    let house = std::fs::read_to_string(root.join("house/rules.md")).unwrap();
    assert!(house.contains("read them with `proj settings show`"));
    assert!(!house.contains("read them with `settings show`"));
    // burrow's own AGENTS.md carrying it verbatim is the guidance check's
    // (crates/burrow-guidance's house), which the docs lane runs too.
}
