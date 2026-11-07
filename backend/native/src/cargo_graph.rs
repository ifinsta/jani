//! Read-only import of Cargo's resolver graph. The input must come from a
//! separately governed cargo metadata --locked --offline invocation.
//! This proves package edges, NOT Rust symbol-level references.
use crate::{digest, state::State};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component, Path},
};
const MAX_BYTES: u64 = 8_000_000;
const MAX_PACKAGES: usize = 8_000;
const MAX_EDGES: usize = 30_000;

pub fn import(s: &State, rel: &str) -> Result<Value, String> {
    let path = Path::new(rel);
    if rel.len() > 240
        || path.is_absolute()
        || path.components().next().is_none()
        || !path.components().all(|p| matches!(p, Component::Normal(_)))
    {
        return Err("invalid relative Cargo metadata path".into());
    }
    let source = s.root.join(path);
    let meta = fs::symlink_metadata(&source).map_err(|_| "Cargo metadata missing")?;
    if !meta.is_file()
        || meta.file_type().is_symlink()
        || meta.len() > MAX_BYTES
        || !source
            .canonicalize()
            .map_err(|e| e.to_string())?
            .starts_with(&s.root)
    {
        return Err("unsafe or oversized Cargo metadata path".into());
    }
    let bytes = fs::read(source).map_err(|e| e.to_string())?;
    parse(&s.root, &bytes)
}

fn parse(root: &Path, bytes: &[u8]) -> Result<Value, String> {
    if bytes.len() as u64 > MAX_BYTES {
        return Err("Cargo metadata exceeds limit".into());
    }
    let d: Value = serde_json::from_slice(bytes).map_err(|_| "malformed Cargo metadata JSON")?;
    if d["version"].as_u64() != Some(1) {
        return Err("unsupported Cargo metadata format".into());
    }
    let workspace = d["workspace_root"]
        .as_str()
        .ok_or("missing workspace_root")?;
    if Path::new(workspace)
        .canonicalize()
        .map_err(|_| "invalid workspace_root")?
        != root
    {
        return Err("Cargo metadata belongs to another workspace".into());
    }
    let pkgs = d["packages"].as_array().ok_or("Cargo packages missing")?;
    if pkgs.is_empty() || pkgs.len() > MAX_PACKAGES {
        return Err("Cargo package count outside limits".into());
    }
    let mut identities = BTreeMap::new();
    for p in pkgs {
        let id = p["id"].as_str().ok_or("missing package id")?;
        let name = p["name"].as_str().ok_or("missing package name")?;
        let version = p["version"].as_str().ok_or("missing package version")?;
        if id.len() > 1024
            || name.is_empty()
            || name.len() > 120
            || version.len() > 60
            || identities
                .insert(id.to_owned(), (name.to_owned(), version.to_owned()))
                .is_some()
        {
            return Err("invalid or duplicate Cargo package".into());
        }
    }
    let members = d["workspace_members"]
        .as_array()
        .ok_or("workspace_members missing")?;
    let mut owned = BTreeSet::new();
    for m in members {
        let id = m.as_str().ok_or("invalid workspace member")?;
        if !identities.contains_key(id) || !owned.insert(id.to_owned()) {
            return Err("unresolved or duplicate workspace member".into());
        }
    }
    if owned.is_empty() {
        return Err("no workspace members".into());
    }
    let nodes = d
        .pointer("/resolve/nodes")
        .and_then(Value::as_array)
        .ok_or("Cargo resolver graph missing; --no-deps not supported")?;
    if nodes.len() > MAX_PACKAGES {
        return Err("Cargo resolver graph exceeds limit".into());
    }
    let mut node_ids = BTreeSet::new();
    let mut edges = BTreeSet::new();
    for n in nodes {
        let from = n["id"].as_str().ok_or("missing Cargo resolve id")?;
        if !identities.contains_key(from) || !node_ids.insert(from.to_owned()) {
            return Err("unknown or duplicate Cargo resolve node".into());
        }
        let deps = n["deps"]
            .as_array()
            .ok_or("resolved dependencies missing")?;
        for dep in deps {
            let to = dep["pkg"].as_str().ok_or("missing dependency package")?;
            let alias = dep["name"].as_str().ok_or("missing dependency alias")?;
            if !identities.contains_key(to) || alias.is_empty() || alias.len() > 128 {
                return Err("unresolved Cargo dependency".into());
            }
            let kinds = dep["dep_kinds"]
                .as_array()
                .ok_or("dependency kind missing")?;
            if kinds.is_empty() || kinds.len() > 12 {
                return Err("dependency kinds exceed limit".into());
            }
            for k in kinds {
                let kind = k["kind"].as_str().unwrap_or("normal");
                if !matches!(kind, "normal" | "dev" | "build") {
                    return Err("invalid dependency kind".into());
                }
                let target = match &k["target"] {
                    Value::Null => "",
                    Value::String(v) if v.len() < 256 => v,
                    _ => return Err("invalid dependency target".into()),
                };
                edges.insert((
                    from.to_owned(),
                    to.to_owned(),
                    alias.to_owned(),
                    kind.to_owned(),
                    target.to_owned(),
                ));
                if edges.len() > MAX_EDGES {
                    return Err("too many Cargo dependency edges".into());
                }
            }
        }
    }
    if !owned.iter().all(|x| node_ids.contains(x)) {
        return Err("workspace members absent from resolve graph".into());
    }
    Ok(json!({"tool":"index","operation":"cargo-graph",
        "format":"jani.cargo-resolve/v1",
        "packages":identities.into_iter().map(|(id,(name,version))|
            json!({"workspace":owned.contains(&id),"id":id,"name":name,"version":version}))
            .collect::<Vec<_>>(),
        "edges":edges.into_iter().map(|(from,to,alias,kind,target)|
            json!({"from":from,"to":to,"alias":alias,"kind":kind,"target":target}))
            .collect::<Vec<_>>(),
        "sourceWorkspaceMatches":true,"packageResolution":true,"symbolResolution":false,
        "compilerExecutedByThisTool":false,"externalAttestation":false,
        "evidenceSha256":digest(bytes)}))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_resolved_packages_and_denies_foreign_workspace() {
        let root = std::env::temp_dir().canonicalize().unwrap();
        let report = json!({
            "version":1,"workspace_root":root,
            "workspace_members":["main"],"packages":[
                {"id":"main","name":"main","version":"1.0"},
                {"id":"dep","name":"dep","version":"2.0"}],
            "resolve":{"nodes":[
                {"id":"main","deps":[{"name":"dep","pkg":"dep",
                    "dep_kinds":[{"kind":null,"target":null}]}]},
                {"id":"dep","deps":[]}]}
        });
        let parsed = parse(&root, &serde_json::to_vec(&report).unwrap()).unwrap();
        assert_eq!(parsed["edges"].as_array().unwrap().len(), 1);
        assert_eq!(parsed["symbolResolution"], false);
        let mut forged = report;
        forged["workspace_root"] = json!("/does-not-exist");
        assert!(parse(&root, &serde_json::to_vec(&forged).unwrap()).is_err());
        forged["workspace_root"] = json!(root);
        forged["resolve"]["nodes"][0]["deps"][0]["pkg"] = json!("absent");
        assert!(parse(&root, &serde_json::to_vec(&forged).unwrap()).is_err());
    }
}
