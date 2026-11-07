//! Deterministically generate closed Rust vocabularies and transitions from contract/.
use serde_json::Value;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

fn variant(state: &str) -> String {
    let mut result = String::new();
    for segment in state.split('_') {
        let mut chars = segment.chars();
        if let Some(first) = chars.next() {
            result.push(first.to_ascii_uppercase());
            result.extend(chars);
        }
    }
    result
}

fn generate(out: &mut String, path: &Path) {
    let text = fs::read_to_string(path).expect("read contract state machine");
    let data: Value = serde_json::from_str(&text).expect("parse contract state machine");
    let name = data["name"].as_str().expect("state machine name");
    assert!(matches!(name, "AgentPhase" | "AlertPhase"));
    let states: Vec<&str> = data["states"]
        .as_array()
        .expect("states")
        .iter()
        .map(|v| v.as_str().expect("state name"))
        .collect();
    assert!(!states.is_empty());
    let transitions = data["transitions"].as_object().expect("transitions");
    for (from, destinations) in transitions {
        assert!(states.contains(&from.as_str()), "unknown transition source");
        for to in destinations.as_array().expect("destinations") {
            assert!(
                states.contains(&to.as_str().expect("destination")),
                "unknown transition destination"
            );
        }
    }
    writeln!(
        out,
        "#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]"
    )
    .unwrap();
    writeln!(out, "#[serde(rename_all = \"snake_case\")]").unwrap();
    writeln!(out, "pub enum {name} {{").unwrap();
    for state in &states {
        writeln!(out, "    {},", variant(state)).unwrap();
    }
    writeln!(out, "}}").unwrap();
    writeln!(out, "impl {name} {{").unwrap();
    writeln!(
        out,
        "    pub fn can_transition(self, next: Self) -> bool {{"
    )
    .unwrap();
    let mut allowed = Vec::new();
    for state in &states {
        if let Some(destinations) = transitions.get(*state).and_then(Value::as_array) {
            for to in destinations {
                let to = to.as_str().expect("destination");
                allowed.push(format!("(Self::{}, Self::{})", variant(state), variant(to)));
            }
        }
    }
    if allowed.is_empty() {
        writeln!(out, "        false").unwrap();
    } else {
        writeln!(
            out,
            "        matches!((self, next), {})",
            allowed.join(" | ")
        )
        .unwrap();
    }
    writeln!(out, "    }}").unwrap();
    writeln!(out, "}}").unwrap();
}

/// Emit the release-channel vocabulary shared by the Rust updater and the
/// install/release tooling so schemas, type names and target mappings have a
/// single source in contract/releases.json.
fn generate_releases(out: &mut String, path: &Path) {
    let text = fs::read_to_string(path).expect("read release contract");
    let data: Value = serde_json::from_str(&text).expect("parse release contract");
    let schema = |key: &str| {
        data["schemas"][key]
            .as_str()
            .unwrap_or_else(|| panic!("release schema {key}"))
    };
    let component = data["component"].as_str().expect("release component");
    let types = data["types"].as_object().expect("release types");
    let package = types[component]
        .as_str()
        .unwrap_or_else(|| panic!("release types must contain component {component}"));
    let platforms = data["platforms"].as_object().expect("release platforms");
    let targets = data["targets"].as_object().expect("release targets");
    let mut rows = Vec::new();
    for (key, triple) in targets {
        let triple = triple.as_str().expect("target triple");
        let (os, arch) = key.split_once('-').expect("target key os-arch");
        assert!(
            platforms.contains_key(os),
            "release target {key} uses unknown platform {os}"
        );
        rows.push(format!("        ({os:?}, {arch:?}, {os:?}, {triple:?}),"));
    }
    rows.sort();
    let mut binaries = Vec::new();
    for (kind, name) in types {
        binaries.push(format!(
            "        ({kind:?}, {:?}),",
            name.as_str().expect("binary name")
        ));
    }
    binaries.sort();
    for (key, name) in [
        ("RELEASE_CHANNEL_SCHEMA", schema("channel")),
        ("RELEASE_MANIFEST_SCHEMA", schema("manifest")),
        ("RELEASE_UPDATE_SCHEMA", schema("update")),
        ("RELEASE_LEGACY_CHANNEL_SCHEMA", schema("legacyChannel")),
        ("RELEASE_LEGACY_MANIFEST_SCHEMA", schema("legacyManifest")),
        ("RELEASE_COMPONENT", component),
        ("RELEASE_PACKAGE", package),
    ] {
        writeln!(out, "pub const {key}: &str = {name:?};").unwrap();
    }
    writeln!(
        out,
        "/// Release type -> binary/package name.\npub const RELEASE_BINARIES: &[(&str, &str)] = &["
    )
    .unwrap();
    for row in binaries {
        writeln!(out, "{row}").unwrap();
    }
    writeln!(out, "];").unwrap();
    writeln!(
        out,
        "/// (rust OS, rust arch, release platform, rust target triple).\npub const RELEASE_TARGETS: &[(&str, &str, &str, &str)] = &["
    )
    .unwrap();
    for row in rows {
        writeln!(out, "{row}").unwrap();
    }
    writeln!(out, "];").unwrap();
}

fn main() {
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../contract");
    println!("cargo:rerun-if-changed={}", base.display());
    let version = fs::read_to_string(base.join("VERSION")).expect("contract VERSION");
    let version = version.trim();
    assert!(!version.is_empty());
    let mut out = String::from("// Generated from contract/. Do not edit.\n");
    writeln!(out, "pub const CONTRACT_VERSION: &str = {version:?};").unwrap();
    generate(&mut out, &base.join("lifecycle/states.json"));
    generate(&mut out, &base.join("alerts/states.json"));
    generate_releases(&mut out, &base.join("releases.json"));
    let target = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR")).join("contract.rs");
    fs::write(target, out).expect("write generated contract");
}
