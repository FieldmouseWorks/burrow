//! Exercise the public settings page so the negative runs these tests on base code too.
use std::path::Path;

use burrow_dashboard::assets::{Assets, FONTS};
use burrow_dashboard::pages::{self, Frame};
use maud::{DOCTYPE, Markup, PreEscaped, html};
use serde_json::{Value, json};

// Keep assertions on the settings content; the shared shell has its own logout form.
fn settings_body(document: &Value, schema: &Value) -> Result<Markup, String> {
    let fonts = tempfile::tempdir().unwrap();
    for name in FONTS {
        std::fs::write(fonts.path().join(name), name.as_bytes()).unwrap();
    }
    let assets = Assets::load(
        Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/assets")),
        fonts.path(),
    )
    .unwrap();
    let frame = Frame {
        title: "Settings fixture",
        host: "dashboard.example.ts.net",
        assets: &assets,
    };
    let rendered = pages::settings(&frame, document, schema)?.into_string();
    assert!(rendered.contains("data-page=\"settings\""));
    assert!(rendered.contains("<title>Settings · Settings fixture</title>"));
    let (_, content) = rendered.split_once("<main>").unwrap();
    let (content, _) = content.split_once("</main>").unwrap();
    Ok(PreEscaped(content.to_owned()))
}

fn settings_fixture() -> Value {
    serde_json::from_str(include_str!("settings-show.json")).unwrap()
}

#[test]
fn settings_render_every_schema_option_and_new_fields() {
    let mut fixture = settings_fixture();
    let before = settings_body(&fixture["document"], &fixture["schema"])
        .unwrap()
        .into_string()
        .replace("<wbr>", "");
    assert!(!before.contains("future.extra"));
    fixture["schema"]["properties"]["options"]["properties"]["future.extra"] = json!({
        "x-owner": "another reader", "x-change": "live",
        "description": "A new setting <with help> & details.",
        "type": "object", "properties": {"value": {"type": "string"}}
    });
    fixture["document"]["options"]["future.extra"] = json!({
        "value": "<script>alert('value')</script>", "origin": "live", "file": "/new/<settings>"
    });
    let rendered = settings_body(&fixture["document"], &fixture["schema"])
        .unwrap()
        .into_string()
        .replace("<wbr>", "");
    let names = fixture["schema"]["properties"]["options"]["properties"]
        .as_object()
        .unwrap();
    assert_eq!(rendered.matches("<article ").count(), names.len());
    for name in names.keys() {
        assert!(
            rendered.contains(&format!("data-setting=\"{name}\"")),
            "{name}"
        );
    }
    assert!(rendered.contains(">another reader</h2>"));
    assert!(rendered.contains("A new setting &lt;with help&gt; &amp; details."));
    assert!(rendered.contains("&lt;script&gt;"));
    assert!(rendered.contains("/new/&lt;settings&gt;"));
    assert!(!rendered.contains("<script>"));
    assert!(rendered.contains("Live · next decision"));
    assert!(rendered.contains("Deploy required"));
    assert!(!rendered.contains("<input") && !rendered.contains("<form"));
}

#[test]
fn settings_show_values_sources_and_help_without_inventing_readings() {
    let mut fixture = settings_fixture();
    let rendered = settings_body(&fixture["document"], &fixture["schema"])
        .unwrap()
        .into_string()
        .replace("<wbr>", "");
    for text in [
        "default",
        "flake",
        "live",
        "/config/proj/roles",
        "null",
        "true",
        "6",
    ] {
        assert!(rendered.contains(text), "{text}");
    }
    // A live-capable setting at its default must not claim its file was read.
    assert!(!rendered.contains("/jobs/.usage.json"));
    fixture["document"]["options"]
        .as_object_mut()
        .unwrap()
        .remove("proj.roles");
    assert_eq!(
        settings_body(&fixture["document"], &fixture["schema"]).unwrap_err(),
        "Settings value missing: proj.roles"
    );
    let mut fixture = settings_fixture();
    fixture["schema"]["properties"]["options"]["properties"]
        .as_object_mut()
        .unwrap()
        .remove("proj.roles");
    assert_eq!(
        settings_body(&fixture["document"], &fixture["schema"]).unwrap_err(),
        "Settings schema missing: proj.roles"
    );
}

#[test]
fn settings_fixture_preview() {
    let fixture = settings_fixture();
    let body = settings_body(&fixture["document"], &fixture["schema"]).unwrap();
    // Optional output lets the browser acceptance use the very same renderer.
    if let Some(path) = std::env::var_os("BURROW_SETTINGS_PREVIEW") {
        let preview = html! {
            (DOCTYPE)
            html lang="en" {
                head {
                    meta charset="utf-8";
                    meta name="viewport" content="width=device-width, initial-scale=1";
                    title { "Settings fixture" }
                    link rel="stylesheet" href="/assets/app.css";
                }
                body { main { (body) } }
            }
        };
        std::fs::write(path, preview.into_string()).unwrap();
    }
}
