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

    if root.join("src").exists() || root.join("crates").exists() {
        return Err("root-level src/ and crates/ are forbidden: packages belong under backend/".into());
    }
    for member in ["backend/core", "backend/store", "backend/server", "backend/mirror", "backend/mcp", "backend/native", "tools/jani-check", "tools/jani-claims"] {
        let path = root.join(member).join("Cargo.toml");
        let manifest = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        if manifest.contains("hivewire") || manifest.contains("agents-collaboration") {
            return Err(format!("{member} must not require a coordinator"));
        }
    }
    if !root.join("backend/mirror/src/lib.rs").is_file() {
        return Err("native Mirror package is missing".into());
    }
    let mcp = fs::read_to_string(root.join("backend/mcp/Cargo.toml"))
        .map_err(|e| e.to_string())?;
    if mcp.contains("jani-server") || mcp.contains("hivewire") || mcp.contains("agents-collaboration") {
        return Err("MCP governance kernel must not depend on server or coordinators".into());
    }
    if !root.join("contract/mcp/governance.schema.json").is_file() {
        return Err("missing Jani MCP governance contract schema".into());
    }
    if !root.join("backend/native/src/lib.rs").is_file() {
        return Err("jani-native toolkit missing".into());
    }
    let kernel = fs::read_to_string(root.join("backend/core/Cargo.toml")).map_err(|e| e.to_string())?;
    if kernel.contains("jani-server") || kernel.contains("jani-agent") || kernel.contains("jani-store") {
        return Err("jani-core may not depend on higher layers".into());
    }
    let manifest = fs::read_to_string(root.join("backend/mirror/Cargo.toml")).map_err(|e| e.to_string())?;
    for forbidden in ["reqwest", "jani-server", "jani-agent", "hivewire", "agents-collaboration"] {
        if manifest.contains(forbidden) {
            return Err(format!("Mirror core must remain independent of {forbidden}"));
        }
    }
    let src = fs::read_to_string(root.join("backend/server/src/attention.rs")).map_err(|e| e.to_string())?;
    if !src.contains("use jani_core::contract::") || !src.contains("use jani_store::") {
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
