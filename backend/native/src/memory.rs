//! Bounded context assembly, cache pruning, evidence references and Mirror optimization.
use crate::{digest,state::State,repo};
use serde_json::{json,Value};
use std::fs;
use std::path::Path;

pub fn context(s:&mut State,op:&str,args:&[String])->Result<Value,String>{
    match op{
        "pin"=>{
            let path=args.first().ok_or("context pin requires indexed source path")?;
            let indexed=s.list("index",15000)?.into_iter().any(|(_,v)|v["path"].as_str()==Some(path));
            if !indexed{return Err("context path is not indexed for this tenant and task".into());}
            let id=format!("pin-{}", &digest(path.as_bytes())[..24]);
            s.set("pin",&id,&json!({"path":path,"pinned":true}))?;
            Ok(json!({"pinned":path,"id":id}))
        }
        "assemble"=>{
            let query=args.first().ok_or("context assemble requires a search term")?;
            let tokens=args.get(1).and_then(|s|s.parse::<usize>().ok()).unwrap_or(4000);
            if !(256..=32_000).contains(&tokens){return Err("context budget 256..32000 tokens".into());}
            let mut assembled=Vec::new();let mut used=0usize;
            let mut candidates=Vec::new();
            for (_,r) in s.list("pin",500)? {candidates.push((true,r["path"].as_str().unwrap_or("").to_owned()));}
            let results=repo::search(s,query)?;
            if let Some(found)=results["results"].as_array(){
                for r in found{candidates.push((false,r["path"].as_str().unwrap_or("").to_owned()));}
            }
            candidates.sort_by(|a,b|b.0.cmp(&a.0).then_with(||a.1.cmp(&b.1)));
            candidates.dedup_by(|a,b|a.1==b.1);
            let indexed=s.list("index",15000)?.into_iter()
                .filter_map(|(_,v)|v["path"].as_str().map(|p|(p.to_owned(),v.clone())))
                .collect::<std::collections::BTreeMap<_,_>>();
            for (pinned,path) in candidates {
                let record=indexed.get(&path);
                if let Some(metadata)=record{
                    // Token estimate is for metadata only; source bodies are never copied.
                    let cost=48+metadata["symbols"].as_array().map_or(0,|s|s.len()*4);
                    if used+cost>tokens {
                        if pinned{return Err("context budget cannot accommodate pinned evidence".into());}
                        continue;
                    }
                    used+=cost;
                    assembled.push(json!({"path":path,"pinned":pinned,
                        "sha256":metadata["sha256"],"symbols":metadata["symbols"],"estimatedTokens":cost}));
                    if assembled.len()>=100{break;}
                }
            }
            let artifact=json!({"task":s.task,"query":query,"tokenBudget":tokens,
                "estimatedTokens":used,"items":assembled,"containsSourceBodies":false});
            s.set("context","last_pack",&artifact)?;
            Ok(artifact)
        }
        "show"=>Ok(s.get("context","last_pack")?.unwrap_or(json!({"items":[]}))),
        _=>Err("context commands: pin PATH, assemble QUERY [TOKENS], show".into())
    }
}
pub fn pruner(s:&mut State,op:&str)->Result<Value,String>{
    let contexts=s.list("context",10000)?;
    let candidates=contexts.iter().filter(|(id,_)|id!="last_pack")
        .map(|(id,_)|id.to_owned()).collect::<Vec<_>>();
    if op=="preview" {
        return Ok(json!({"candidates":candidates,"deleted":false,
            "neverPrune":["plan","checkpoint","evidence","audit","approvals","pins"]}));
    }
    if op!="run"{return Err("pruner commands: preview, run".into());}
    let mut removed=0;
    for id in candidates{removed+=s.delete("context",&id)?;}
    // A last_pack is deterministic working context and may be discarded;
    // pinned references live separately in the pin table.
    removed+=s.delete("context","last_pack")?;
    Ok(json!({"removedDisposableContext":removed,"pinsKept":s.count("pin")?,
        "authoritativeEvidencePruned":false}))
}
pub fn evidence(s:&mut State,op:&str,args:&[String])->Result<Value,String>{
    match op{
        "record"=>{
            let id=args.first().ok_or("evidence record requires ID")?;
            let path=args.get(1).ok_or("evidence record requires relative path")?;
            let rel=Path::new(path);
            jani_core::config::validate_relative(rel)?;
            if rel.components().any(|c|jani_core::config::protected_component(&c.as_os_str().to_string_lossy())){
                return Err("protected evidence source".into());
            }
            let full=s.root.join(rel);
            let meta=fs::symlink_metadata(&full).map_err(|e|e.to_string())?;
            if !meta.is_file()||meta.file_type().is_symlink()||meta.len()>2_000_000 {
                return Err("untrusted evidence path".into());
            }
            if s.get("evidence",id)?.is_some() {
                return Err("evidence reference already exists; no silent replacement".into());
            }
            let sha=digest(&fs::read(full).map_err(|e|e.to_string())?);
            let entry=json!({"id":id,"source":path,"sha256":sha,
                "status":"unverified","isApproval":false});
            s.set("evidence",id,&entry)?;
            Ok(entry)
        }
        "list"=>Ok(json!({"evidence":s.list("evidence",500)?
            .into_iter().map(|(_,v)|v).collect::<Vec<_>>()})),
        "check"=>{
            let id=args.first().ok_or("evidence check requires ID")?;
            let v=s.get("evidence",id)?.ok_or("evidence id not found")?;
            let path=v["source"].as_str().ok_or("corrupt evidence entry")?;
            let full=s.root.join(path);
            let meta=fs::symlink_metadata(&full).map_err(|e|e.to_string())?;
            if !meta.is_file()||meta.file_type().is_symlink(){return Err("evidence source not trusted".into());}
            let hash=digest(&fs::read(full).map_err(|e|e.to_string())?);
            Ok(json!({"id":id,"digestMatches":hash==v["sha256"].as_str().unwrap_or(""),
                "status":"unverified","doesNotProveOutcome":true}))
        }
        _=>Err("evidence commands: record ID FILE, check ID, list".into())
    }
}
pub fn optimizer(s:&State)->Result<Value,String>{
    let journal=s.state_dir.join(jani_core::journal::JOURNAL_FILE);
    if !journal.exists(){return Ok(json!({"recommendations":[],"historicalEvents":0,
        "noAuditEvidence":true,"autoApplied":false}));}
    let state=journal.parent().ok_or("invalid journal path")?;
    if fs::metadata(&journal).map_err(|e|e.to_string())?.len()>16_000_000{
        return Err("optimizer journal exceeds 16MiB analysis window".into());
    }
    let verified=jani_core::journal::verify(state)?;
    if verified["ok"].as_bool()!=Some(true){return Err("audit journal failed integrity check".into());}
    let events=jani_core::journal::read_events(&journal)?;
    let report=jani_mirror::workflow_report(&events,&s.tenant);
    Ok(json!({"historicalRuns":report.considered_runs,
        "recurringFindings":report.recurring_findings,
        "recommendations":report.runs.into_iter()
           .flat_map(|r|r.findings.into_iter()
               .map(move |f|json!({"run":r.run_id,"code":f.code,"suggestion":f.recommendation,"evidence":f.evidence})))
           .take(100).collect::<Vec<_>>(),
        "autoApplied":false,"evidenceOnly":true}))
}
#[cfg(test)]
mod tests{
    use super::*;
    #[test]
    fn digest_is_stable(){assert_eq!(digest(b"input"),digest(b"input"));}
}
