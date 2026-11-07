//! Bounded repository metadata index, impact analysis and verification selection.
//! Never persists file bodies or executes tests directly.
use crate::{digest, state::State};
use regex::Regex;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// The index table can never hold more entries than the walk allows; the same
/// bound applies when listing it back.
pub(crate) const MAX_INDEX_ENTRIES: usize = 15_000;
const MAX_FILE_BYTES: u64 = 1_000_000;
const MAX_SYMBOLS: usize = 120;
const MAX_SEARCH_RESULTS: usize = 40;
const MAX_QUERY_LEN: usize = 100;
const MAX_PATH_LEN: usize = 180;
const MAX_IMPACT_RESULTS: usize = 100;
const MAX_TEST_CANDIDATES: usize = 120;
const MAX_DEPENDENCIES: usize = 160;

/// Repo-specific directories skipped on top of the shared `ignored_component`
/// and `protected_component` baselines.
const EXCLUDED_COMPONENTS: &[&str] = &["claims", "generated"];

fn allowed(path: &Path) -> bool {
    let parts: Vec<String> = path
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .collect();
    !parts.iter().any(|c| {
        jani_core::config::ignored_component(c)
            || EXCLUDED_COMPONENTS.contains(&c.as_str())
            || jani_core::config::protected_component(c)
    })
}
fn visit(
    root: &Path,
    dir: &Path,
    found: &mut Vec<PathBuf>,
    skip: Option<&Path>,
) -> Result<(), String> {
    if found.len() > MAX_INDEX_ENTRIES {
        return Err("index file limit exceeded".into());
    }
    let mut entries = fs::read_dir(root.join(dir))
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let relative = dir.join(entry.file_name());
        if skip == Some(relative.as_path()) || !allowed(&relative) {
            continue;
        }
        let kind = entry.file_type().map_err(|e| e.to_string())?;
        if kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            visit(root, &relative, found, skip)?;
        } else if kind.is_file()
            && matches!(
                relative.extension().and_then(|x| x.to_str()).unwrap_or(""),
                "rs" | "py"
                    | "go"
                    | "js"
                    | "ts"
                    | "tsx"
                    | "jsx"
                    | "java"
                    | "c"
                    | "h"
                    | "cpp"
                    | "md"
                    | "toml"
                    | "json"
                    | "yml"
                    | "yaml"
            )
        {
            found.push(relative);
        }
        if found.len() > MAX_INDEX_ENTRIES {
            return Err("index file limit exceeded".into());
        }
    }
    Ok(())
}
fn symbols(text: &str) -> Vec<String> {
    let re=Regex::new(r"(?m)^\s*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?(?:fn|struct|enum|trait|mod|class|def|interface)\s+([A-Za-z_][A-Za-z_0-9]*)").unwrap();
    let mut found = BTreeSet::new();
    for cap in re.captures_iter(text) {
        found.insert(cap[1].to_string());
        if found.len() >= MAX_SYMBOLS {
            break;
        }
    }
    found.into_iter().collect()
}
/// Conservative, bounded source-level dependency hints. These are not compiler-resolved
/// edges: dynamic imports, macro expansion, generated code and external packages
/// deliberately remain outside the guarantee.
fn dependencies(text: &str) -> Vec<String> {
    let patterns = [
        r"(?m)^\s*(?:pub\s+)?(?:use|mod)\s+(?:(?:crate|self|super)::)?([A-Za-z_][A-Za-z_0-9:]*)",
        r"(?m)^\s*(?:from|import)\s+([A-Za-z_][A-Za-z_0-9.]*)",
        r#"(?m)\b(?:from\s*|require\s*\(\s*|import\s*\(\s*)['"](?:\./|\.\./)?([A-Za-z_][A-Za-z_0-9./-]*)"#,
    ];
    let mut out = BTreeSet::new();
    for pattern in patterns {
        let re = Regex::new(pattern).expect("constant dependency regex");
        for capture in re.captures_iter(text) {
            for token in capture[1].split([':', '.', '/', '-']) {
                if token.len() > 1 && token.len() <= 80 {
                    out.insert(token.to_ascii_lowercase());
                }
                if out.len() >= MAX_DEPENDENCIES {
                    break;
                }
            }
            if out.len() >= MAX_DEPENDENCIES {
                break;
            }
        }
        if out.len() >= MAX_DEPENDENCIES {
            break;
        }
    }
    out.into_iter().collect()
}

fn file_id(path: &str) -> String {
    format!("f-{}", &digest(path.as_bytes())[..32])
}
pub fn build(s: &mut State) -> Result<Value, String> {
    let mut found = Vec::new();
    // Skip the state directory wherever it lives, not only when it is .jani.
    let skip = s.state_dir.strip_prefix(&s.root).ok();
    visit(&s.root, Path::new(""), &mut found, skip)?;
    let prev = s.list("index", MAX_INDEX_ENTRIES)?;
    let mut seen = BTreeSet::new();
    let mut updated = 0;
    let mut unchanged = 0;
    for relative in found {
        let name = relative.to_string_lossy().replace('\\', "/");
        let path = s.root.join(&relative);
        let meta = fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
        if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > MAX_FILE_BYTES {
            continue;
        }
        let bytes = fs::read(&path).map_err(|e| e.to_string())?;
        if bytes.len() as u64 > MAX_FILE_BYTES {
            continue;
        }
        let hash = digest(&bytes);
        let id = file_id(&name);
        seen.insert(id.clone());
        if let Some(old) = s.get("index", &id)? {
            if old["sha256"].as_str() == Some(hash.as_str()) && old["dependencies"].is_array() {
                unchanged += 1;
                continue;
            }
        }
        let extracted = std::str::from_utf8(&bytes).map(symbols).unwrap_or_default();
        let refs = std::str::from_utf8(&bytes)
            .map(dependencies)
            .unwrap_or_default();
        s.set(
            "index",
            &id,
            &json!({
                "path":name,"sha256":hash,"bytes":bytes.len(),"symbols":extracted,"dependencies":refs
            }),
        )?;
        updated += 1;
    }
    let mut removed = 0;
    for (id, _) in prev {
        if !seen.contains(&id) {
            removed += s.delete("index", &id)?;
        }
    }
    Ok(
        json!({"tool":"index","updated":updated,"unchanged":unchanged,
        "removed":removed,"total":s.count("index")?,"contentStored":false}),
    )
}
pub fn search(s: &State, term: &str) -> Result<Value, String> {
    if term.is_empty() || term.len() > MAX_QUERY_LEN {
        return Err("index search term must be 1..100 chars".into());
    }
    let query = term.to_ascii_lowercase();
    let mut matches = Vec::new();
    for (_, record) in s.list("index", MAX_INDEX_ENTRIES)? {
        let path = record["path"].as_str().unwrap_or("");
        let names = record["symbols"].as_array().cloned().unwrap_or_default();
        let path_match = path.to_ascii_lowercase().contains(&query);
        let symbol_match = names.iter().any(|x| {
            x.as_str()
                .unwrap_or("")
                .to_ascii_lowercase()
                .contains(&query)
        });
        if path_match || symbol_match {
            let score = if symbol_match { 3 } else { 1 };
            matches.push((score, record));
        }
    }
    matches.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| a.1["path"].as_str().cmp(&b.1["path"].as_str()))
    });
    let total = matches.len();
    Ok(json!({"query":term,"totalMatches":total,
        "results":matches.into_iter().take(MAX_SEARCH_RESULTS).map(|(_,r)|r).collect::<Vec<_>>(),
        "sourceBodiesDisclosed":false}))
}
pub fn impact(s: &State, path: &str) -> Result<Value, String> {
    if path.contains("..")
        || path.starts_with('/')
        || path.contains('\\')
        || path.len() > MAX_PATH_LEN
    {
        return Err("invalid relative path".into());
    }
    let stem = Path::new(path)
        .file_stem()
        .and_then(|x| x.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if stem.len() < 2 {
        return Err("unable to derive module identifier".into());
    }
    let records = s.list("index", MAX_INDEX_ENTRIES)?;
    let source = records
        .iter()
        .find(|(_, entry)| entry["path"].as_str() == Some(path))
        .ok_or("target is not indexed; rebuild index before impact analysis")?;
    if source.1["dependencies"].as_array().is_none() {
        return Err("legacy index entry; rebuild index".into());
    }
    // Do not make safety claims from a snapshot whose bytes have since changed.
    // This also blocks reviews against deleted or symlink-replaced files.
    for (_, entry) in &records {
        let name = entry["path"].as_str().ok_or("index path missing")?;
        let meta = fs::symlink_metadata(s.root.join(name))
            .map_err(|_| "index is stale; rebuild before impact analysis".to_string())?;
        if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > MAX_FILE_BYTES {
            return Err("index is stale or unsafe; rebuild before impact analysis".into());
        }
        let current = digest(
            &fs::read(s.root.join(name)).map_err(|_| "indexed file unavailable".to_string())?,
        );
        if entry["sha256"].as_str() != Some(current.as_str()) {
            return Err("index is stale; rebuild before impact analysis".into());
        }
    }
    let mut affected = Vec::new();
    let mut tests = Vec::new();
    let mut evidence = Vec::new();
    for (_, entry) in records {
        let name = entry["path"].as_str().unwrap_or("");
        if name == path {
            continue;
        }
        let lower = name.to_ascii_lowercase();
        let refs = entry["dependencies"].as_array();
        let dependency_match = refs
            .into_iter()
            .flatten()
            .any(|v| v.as_str() == Some(stem.as_str()));
        let symbol_match = entry["symbols"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|v| v.as_str().unwrap_or("").eq_ignore_ascii_case(&stem));
        let path_match = lower.contains(&stem);
        if dependency_match || symbol_match || path_match {
            if lower.contains("test") {
                tests.push(name.to_owned());
            } else {
                affected.push(name.to_owned());
            }
            if evidence.len() < MAX_IMPACT_RESULTS {
                evidence.push(json!({"path":name,
                    "reason":if dependency_match {"import-or-module-token"}
                             else if symbol_match {"symbol-name"} else {"path-name"},
                    "sha256":entry["sha256"]}));
            }
        }
    }
    affected.truncate(MAX_IMPACT_RESULTS);
    tests.truncate(MAX_IMPACT_RESULTS);
    Ok(json!({"tool":"impact","source":path,
        "heuristic":"bounded source-level import, module and symbol hints; not a compiler graph",
        "affected":affected,"tests":tests,"evidence":evidence,
        "indexFresh":true,"semanticCompleteness":false,"requiresVerification":true}))
}

pub fn workspace(s: &mut State, op: &str) -> Result<Value, String> {
    match op {
        "status"|"snapshot"=>{
            let mut changed=Vec::new();let mut missing=Vec::new();
            for (_,entry) in s.list("index",MAX_INDEX_ENTRIES)?{
                let name=entry["path"].as_str().unwrap_or("");
                let full=s.root.join(name);
                if !full.is_file(){missing.push(name.to_owned());continue;}
                let meta=fs::symlink_metadata(&full).map_err(|e|e.to_string())?;
                if meta.file_type().is_symlink(){missing.push(name.to_owned());continue;}
                if meta.len()>MAX_FILE_BYTES {changed.push(name.to_owned());continue;}
                if digest(&fs::read(full).map_err(|e|e.to_string())?)!=entry["sha256"].as_str().unwrap_or(""){
                    changed.push(name.to_owned());
                }
            }
            if op=="snapshot"{
                let hash=digest(serde_json::to_string(&(changed.clone(),missing.clone()))
                    .map_err(|e|e.to_string())?.as_bytes());
                s.set("workspace","last_snapshot",&json!({"digest":hash,
                    "changed":changed.clone(),"missing":missing.clone(),"isolation":"metadata_only"}))?;
            }
            Ok(json!({"tool":"workspace","operation":op,
                "changed":changed,"missing":missing,
                "note":"No files modified; unindexed new files require index build"}))
        }
        "rollback"=>Err("workspace rollback requires an authorized external VCS operation; native metadata cannot change source files".into()),
        _=>Err("workspace supports status, snapshot, rollback".into())
    }
}
pub fn verifier(s: &State, op: &str) -> Result<Value, String> {
    match op {
        "select"=>{
            let rows=s.list("index",MAX_INDEX_ENTRIES)?;
            let mut tests=rows.iter().filter_map(|(_,r)|{
                let path=r["path"].as_str()?;
                if path.contains("test")||path.ends_with("_spec.rs") {Some(path.to_owned())}else{None}
            }).collect::<Vec<_>>();
            tests.sort();tests.truncate(MAX_TEST_CANDIDATES);
            Ok(json!({"tool":"verifier","suggestedArgv":["cargo","test","--workspace","--all-targets"],
                "candidateTests":tests,"executed":false,
                "executionRoute":"existing governed command only"}))
        }
        "run"=>Err("verifier cannot bypass the governed tool broker; submit suggested command to the command tool".into()),
        _=>Err("verifier supports select and governed run via broker".into())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dependencies_are_deterministic_and_bounded() {
        let text = "use crate::payments::charge;\nmod receipt;\nfrom core.ledger import Entry\nimport os.path\nimport widget from './component.js';";
        let deps = dependencies(text);
        assert!(deps.contains(&"payments".to_string()));
        assert!(deps.contains(&"receipt".to_string()));
        assert!(deps.contains(&"ledger".to_string()));
        assert!(deps.contains(&"component".to_string()));
        assert!(deps.len() <= MAX_DEPENDENCIES);
    }
    #[test]
    fn indexed_dependency_impact_and_stale_source_refusal() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("jani-impact-{}-{stamp}", std::process::id()));
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/payments.rs"), "pub fn charge() {}\n").unwrap();
        fs::write(root.join("src/client.rs"), "use crate::payments::charge;\n").unwrap();
        let state_dir = root.join(".jani");
        let mut state = State::open(&root, &state_dir, "tenant-a", "task-a").unwrap();
        build(&mut state).unwrap();
        let report = impact(&state, "src/payments.rs").unwrap();
        assert!(report["affected"]
            .as_array()
            .unwrap()
            .contains(&json!("src/client.rs")));
        assert_eq!(report["indexFresh"], true);
        assert_eq!(report["semanticCompleteness"], false);
        fs::write(root.join("src/client.rs"), "changed but not indexed\n").unwrap();
        assert!(impact(&state, "src/payments.rs").is_err());
        drop(state);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn symbols_are_deterministic() {
        assert_eq!(
            symbols("pub fn hello(){}\nstruct World {}\nfn hello(){}"),
            vec!["World".to_string(), "hello".to_string()]
        );
    }
}
