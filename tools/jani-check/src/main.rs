//! Lightweight architecture and contract drift checks; no coordinator dependency.
use std::fs;
use std::path::{Path, PathBuf};

fn check(root: &Path) -> Result<(), String> {
    let manifest = fs::read_to_string(root.join("Cargo.toml")).map_err(|e| e.to_string())?;
    for forbidden in ["hivewire", "agents-collaboration"] {
        if manifest.to_lowercase().contains(forbidden) {
            return Err(format!("forbidden mandatory coordinator dependency: {forbidden}"));
        }
    }
    let contract = root.join("contract");
    let lifecycle: serde_json::Value = serde_json::from_slice(
        &fs::read(contract.join("lifecycle/states.json")).map_err(|e| e.to_string())?
    ).map_err(|e| e.to_string())?;
    let alerts: serde_json::Value = serde_json::from_slice(
        &fs::read(contract.join("alerts/states.json")).map_err(|e| e.to_string())?
    ).map_err(|e| e.to_string())?;
    for (name, data) in [("lifecycle", lifecycle), ("alerts", alerts)] {
        let states = data["states"].as_array().ok_or(format!("{name}: missing states"))?;
        let transitions = data["transitions"].as_object().ok_or(format!("{name}: missing transitions"))?;
        for (from, tos) in transitions {
            if !states.iter().any(|v| v.as_str() == Some(from)) {
                return Err(format!("{name}: unknown state {from}"));
            }
            for to in tos.as_array().ok_or("transition targets must be an array")? {
                if !states.contains(to) {
                    return Err(format!("{name}: unknown target {to}"));
                }
            }
        }
    }
    let src = fs::read_to_string(root.join("src/attention.rs")).map_err(|e| e.to_string())?;
    if !src.contains("use jani_contract::") || !src.contains("use jani_store::") {
        return Err("attention must use generated contracts and transactional store".into());
    }
    Ok(())
}
fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    match check(&root) {
        Ok(()) => println!("jani-check: architecture and contract checks passed"),
        Err(e) => {
            eprintln!("jani-check: {e}");
            std::process::exit(1);
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn current_repository_satisfies_gates() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        check(&root).unwrap();
    }
}
