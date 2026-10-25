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
    let target = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR")).join("contract.rs");
    fs::write(target, out).expect("write generated contract");
}
