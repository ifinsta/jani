//! Bounded repository metadata index, impact analysis and verification selection.
//! Never persists file bodies or executes tests directly.
use crate::{digest, state::State};
use regex::Regex;
use serde_json::{json,Value};
use std::collections::{BTreeMap,BTreeSet};
use std::fs;
use std::path::{Path,PathBuf};

fn allowed(path:&Path)->bool {
    let parts:Vec<String>=path.components().map(|c|c.as_os_str().to_string_lossy().to_string()).collect();
    !parts.iter().any(|c| {
        matches!(c.as_str(),".git"|".jani"|"target"|"node_modules"|".env"|"claims"|"generated"|"__pycache__")
        || jani_core::config::protected_component(c)
    })
}
fn visit(root:&Path,dir:&Path,found:&mut Vec<PathBuf>)->Result<(),String> {
    if found.len()>15000 {return Err("index file limit exceeded".into());}
    let mut entries=fs::read_dir(root.join(dir)).map_err(|e|e.to_string())?
        .collect::<Result<Vec<_>,_>>().map_err(|e|e.to_string())?;
    entries.sort_by_key(|e|e.file_name());
    for entry in entries {
        let relative=dir.join(entry.file_name());
        if !allowed(&relative) {continue;}
        let kind=entry.file_type().map_err(|e|e.to_string())?;
        if kind.is_symlink() {continue;}
        if kind.is_dir(){visit(root,&relative,found)?;}
        else if kind.is_file(){
            if matches!(relative.extension().and_then(|x|x.to_str()).unwrap_or(""),
                "rs"|"py"|"go"|"js"|"ts"|"tsx"|"jsx"|"java"|"c"|"h"|"cpp"|"md"|"toml"|"json"|"yml"|"yaml") {
                found.push(relative);
            }
        }
        if found.len()>15000{return Err("index file limit exceeded".into());}
    }
    Ok(())
}
fn symbols(text:&str)->Vec<String>{
    let re=Regex::new(r"(?m)^\s*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?(?:fn|struct|enum|trait|mod|class|def|interface)\s+([A-Za-z_][A-Za-z_0-9]*)").unwrap();
    let mut found=BTreeSet::new();
    for cap in re.captures_iter(text) {
        found.insert(cap[1].to_string());
        if found.len()>=120 {break;}
    }
    found.into_iter().collect()
}
fn file_id(path:&str)->String {format!("f-{}", &digest(path.as_bytes())[..32])}
pub fn build(s:&mut State)->Result<Value,String>{
    let mut found=Vec::new();visit(&s.root,Path::new(""),&mut found)?;
    let prev=s.list("index",20000)?;
    let mut seen=BTreeSet::new();let mut updated=0;let mut unchanged=0;
    for relative in found {
        let name=relative.to_string_lossy().replace('\\',"/");
        let path=s.root.join(&relative);
        let meta=fs::symlink_metadata(&path).map_err(|e|e.to_string())?;
        if !meta.is_file()||meta.file_type().is_symlink()||meta.len()>1_000_000{continue;}
        let bytes=fs::read(&path).map_err(|e|e.to_string())?;
        if bytes.len()>1_000_000{continue;}
        let hash=digest(&bytes);let id=file_id(&name);
        seen.insert(id.clone());
        if let Some(old)=s.get("index",&id)? {
            if old["sha256"].as_str()==Some(hash.as_str()){unchanged+=1;continue;}
        }
        let extracted=std::str::from_utf8(&bytes).map(symbols).unwrap_or_default();
        s.set("index",&id,&json!({
            "path":name,"sha256":hash,"bytes":bytes.len(),"symbols":extracted
        }))?;
        updated+=1;
    }
    let mut removed=0;
    for (id,_) in prev {if !seen.contains(&id) {removed+=s.delete("index",&id)?;}}
    Ok(json!({"tool":"index","updated":updated,"unchanged":unchanged,
        "removed":removed,"total":s.count("index")?,"contentStored":false}))
}
pub fn search(s:&State,term:&str)->Result<Value,String>{
    if term.is_empty()||term.len()>100{return Err("index search term must be 1..100 chars".into());}
    let query=term.to_ascii_lowercase();let mut matches=Vec::new();
    for (_,record) in s.list("index",15000)? {
        let path=record["path"].as_str().unwrap_or("");
        let names=record["symbols"].as_array().cloned().unwrap_or_default();
        let path_match=path.to_ascii_lowercase().contains(&query);
        let symbol_match=names.iter().any(|x|x.as_str().unwrap_or("").to_ascii_lowercase().contains(&query));
        if path_match||symbol_match {
            let score=if symbol_match{3}else{1};
            matches.push((score,record));
        }
    }
    matches.sort_by(|a,b|b.0.cmp(&a.0).then_with(||a.1["path"].as_str().cmp(&b.1["path"].as_str())));
    let total=matches.len();
    Ok(json!({"query":term,"totalMatches":total,
        "results":matches.into_iter().take(40).map(|(_,r)|r).collect::<Vec<_>>(),
        "sourceBodiesDisclosed":false}))
}
pub fn impact(s:&State,path:&str)->Result<Value,String>{
    if path.contains("..")||path.starts_with('/')||path.len()>180{return Err("invalid relative path".into());}
    let stem=Path::new(path).file_stem().and_then(|x|x.to_str()).unwrap_or("").to_ascii_lowercase();
    if stem.len()<2{return Err("unable to derive module identifier".into());}
    let mut affected=Vec::new();let mut tests=Vec::new();
    for (_,entry) in s.list("index",15000)?{
        let name=entry["path"].as_str().unwrap_or("");
        if name==path{continue;}
        let lower=name.to_ascii_lowercase();
        let deps=entry["symbols"].as_array().into_iter().flatten()
            .any(|v|v.as_str().unwrap_or("").eq_ignore_ascii_case(&stem));
        if lower.contains(&stem)||deps{
            if lower.contains("test") {tests.push(name.to_owned())}
            else {affected.push(name.to_owned())}
        }
    }
    affected.truncate(100);tests.truncate(100);
    Ok(json!({"tool":"impact","source":path,"heuristic":"path-and-symbol only; not a complete dependency graph",
       "affected":affected,"tests":tests,"requiresVerification":true}))
}
pub fn workspace(s:&mut State,op:&str)->Result<Value,String>{
    match op {
        "status"|"snapshot"=>{
            let mut changed=Vec::new();let mut missing=Vec::new();
            for (_,entry) in s.list("index",15000)?{
                let name=entry["path"].as_str().unwrap_or("");
                let full=s.root.join(name);
                if !full.is_file(){missing.push(name.to_owned());continue;}
                let meta=fs::symlink_metadata(&full).map_err(|e|e.to_string())?;
                if meta.file_type().is_symlink(){missing.push(name.to_owned());continue;}
                if meta.len()>1_000_000 {changed.push(name.to_owned());continue;}
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
pub fn verifier(s:&State,op:&str)->Result<Value,String>{
    match op {
        "select"=>{
            let rows=s.list("index",15000)?;
            let mut tests=rows.iter().filter_map(|(_,r)|{
                let path=r["path"].as_str()?;
                if path.contains("test")||path.ends_with("_spec.rs") {Some(path.to_owned())}else{None}
            }).collect::<Vec<_>>();
            tests.sort();tests.truncate(120);
            Ok(json!({"tool":"verifier","suggestedArgv":["cargo","test","--workspace","--all-targets"],
                "candidateTests":tests,"executed":false,
                "executionRoute":"existing governed workspace_command only"}))
        }
        "run"=>Err("verifier cannot bypass the governed tool broker; submit suggested command to workspace_command".into()),
        _=>Err("verifier supports select and governed run via broker".into())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn symbols_are_deterministic() {
        assert_eq!(symbols("pub fn hello(){}\nstruct World {}\nfn hello(){}"),
            vec!["World".to_string(),"hello".to_string()]);
    }
}
