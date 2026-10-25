//! Persistent task DAG, scheduler, checkpoints and deterministic dry-run simulator.
use crate::{digest,state::State};
use serde_json::{json,Value};
use std::collections::{BTreeMap,BTreeSet};
use std::fs;

fn ident(s:&str)->bool {
    !s.is_empty()&&s.len()<=96&&s.bytes().all(|c|c.is_ascii_alphanumeric()||matches!(c,b'-'|b'_'|b'.'))
}
fn nodes(plan:&Value)->Result<&Vec<Value>,String>{
    plan.get("nodes").and_then(Value::as_array).ok_or("plan must provide nodes array".into())
}
fn node_id(v:&Value)->Result<&str,String> {
    let id=v["id"].as_str().ok_or("node missing id")?;
    if !ident(id){return Err("invalid node id".into());}
    Ok(id)
}
pub fn validate(plan:&Value)->Result<(),String>{
    let items=nodes(plan)?;
    if items.is_empty()||items.len()>128{return Err("plan must have 1..128 nodes".into());}
    let mut seen=BTreeSet::new();
    for node in items {
        let id=node_id(node)?;
        if !seen.insert(id.to_string()){return Err("duplicate task node".into());}
        let deps=node["deps"].as_array().ok_or("node missing deps array")?;
        if deps.len()>64||deps.iter().any(|d|!d.is_string()) {return Err("invalid task dependencies".into());}
        if node["acceptance"].as_str().unwrap_or("").trim().is_empty() ||
           node["acceptance"].as_str().unwrap_or("").len()>400 {
            return Err("node acceptance criteria required".into());
        }
        if !matches!(node["status"].as_str().unwrap_or("planned"),
            "planned"|"reported_completed"|"verified"|"blocked"){
            return Err("unsupported task status".into());
        }
    }
    let graph: BTreeMap<&str,Vec<&str>>=items.iter().map(|n|(
        n["id"].as_str().unwrap(),
        n["deps"].as_array().unwrap().iter().filter_map(Value::as_str).collect()
    )).collect();
    for (id,deps) in &graph{
        for d in deps{
            if !graph.contains_key(d){return Err(format!("missing dependency for {id}: {d}"));}
            if id==d{return Err("task self-dependency".into());}
        }
    }
    fn visit<'a>(id:&'a str,graph:&BTreeMap<&'a str,Vec<&'a str>>,
                 visiting:&mut BTreeSet<&'a str>,visited:&mut BTreeSet<&'a str>)->Result<(),String>{
        if visited.contains(id){return Ok(());}
        if !visiting.insert(id){return Err("task dependency cycle".into());}
        for dep in graph.get(id).ok_or("task missing")?{visit(dep,graph,visiting,visited)?;}
        visiting.remove(id);visited.insert(id);
        Ok(())
    }
    let mut visiting=BTreeSet::new();let mut visited=BTreeSet::new();
    for id in graph.keys(){visit(id,&graph,&mut visiting,&mut visited)?;}
    Ok(())
}
fn active(s:&State)->Result<Value,String>{
    s.get("plan","active")?.ok_or("no plan: use native planner create".into())
}
pub fn planner(s:&mut State,op:&str,args:&[String],human_approved:bool)->Result<Value,String>{
    match op{
        "create"=>{
            let file=args.first().ok_or("planner create requires a JSON file")?;
            let path=std::path::Path::new(file);
            jani_core::config::validate_relative(path)?;
            let p=s.root.join(path);
            let meta=fs::symlink_metadata(&p).map_err(|e|e.to_string())?;
            if !meta.is_file()||meta.file_type().is_symlink()||meta.len()>131_072 {
                return Err("unsafe plan file".into());
            }
            let plan:Value=serde_json::from_slice(&fs::read(p).map_err(|e|e.to_string())?)
                .map_err(|e|e.to_string())?;
            validate(&plan)?;
            if s.get("plan","active")?.is_some(){return Err("plan already exists; refuse silent overwrite".into());}
            let hash=digest(serde_json::to_string(&plan).map_err(|e|e.to_string())?.as_bytes());
            s.set("plan","active",&plan)?;
            Ok(json!({"created":true,"nodes":nodes(&plan)?.len(),"digest":hash,"task":s.task}))
        }
        "inspect"=>active(s),
        "report"=>{
            let id=args.first().ok_or("planner report requires node id")?;
            let receipt=args.get(1).ok_or("planner report requires evidence id")?;
            if s.get("evidence",receipt)?.is_none() {return Err("unknown evidence reference".into());}
            let mut plan=active(s)?;
            let n=plan.get_mut("nodes").and_then(Value::as_array_mut).ok_or("corrupt plan")?;
            let item=n.iter_mut().find(|v|v["id"].as_str()==Some(id)).ok_or("node not found")?;
            if item["status"]=="verified" {return Err("cannot change verified node".into());}
            item["status"]=json!("reported_completed");
            item["evidence"]=json!(receipt);
            s.set("plan","active",&plan)?;
            Ok(json!({"id":id,"status":"reported_completed","trusted":false}))
        }
        "verify"=>{
            if !human_approved{return Err("planner verification requires verified human --approve".into());}
            let id=args.first().ok_or("planner verify requires node id")?;
            let mut plan=active(s)?;
            let n=plan.get_mut("nodes").and_then(Value::as_array_mut).ok_or("corrupt plan")?;
            let item=n.iter_mut().find(|v|v["id"].as_str()==Some(id)).ok_or("node not found")?;
            if item["status"]!="reported_completed"{return Err("node not reported completed".into());}
            let receipt=item["evidence"].as_str().ok_or("missing evidence")?;
            if s.get("evidence",receipt)?.is_none(){return Err("evidence missing".into());}
            let inspection=crate::memory::evidence(s,"check",&[receipt.to_owned()])?;
            if inspection["digestMatches"]!=true{return Err("evidence source changed since recording".into());}
            item["status"]=json!("verified");
            s.set("plan","active",&plan)?;
            Ok(json!({"id":id,"status":"verified","humanApproved":true,
                "note":"operator attestation, not cryptographic proof of test correctness"}))
        }
        _=>Err("planner commands: create FILE, inspect, report NODE EVIDENCE, verify NODE --approve".into())
    }
}
pub fn ready_nodes(plan:&Value)->Result<Vec<Value>,String>{
    validate(plan)?;
    let status:BTreeMap<String,String>=nodes(plan)?.iter().map(|n|(
        n["id"].as_str().unwrap().to_owned(),
        n["status"].as_str().unwrap_or("planned").to_owned()
    )).collect();
    let mut eligible=nodes(plan)?.iter().filter(|n|{
        if n["status"].as_str().unwrap_or("planned")!="planned"{return false;}
        n["deps"].as_array().is_some_and(|deps|deps.iter().all(|d|
            status.get(d.as_str().unwrap_or("")).is_some_and(|v|v=="verified")))
    }).cloned().collect::<Vec<_>>();
    eligible.sort_by(|a,b|b["priority"].as_i64().unwrap_or(0)
        .cmp(&a["priority"].as_i64().unwrap_or(0))
        .then_with(||a["id"].as_str().cmp(&b["id"].as_str())));
    Ok(eligible)
}
pub fn scheduler(s:&State)->Result<Value,String>{
    let plan=active(s)?;
    let ready=ready_nodes(&plan)?;
    Ok(json!({"ready":ready,"count":ready.len(),"authorized":false,
        "execution":"scheduler proposes next node; broker must authorize effects"}))
}
pub fn checkpoint(s:&mut State,op:&str)->Result<Value,String>{
    let plan=active(s)?;
    let canonical=serde_json::to_vec(&plan).map_err(|e|e.to_string())?;
    let hash=digest(&canonical);
    match op {
        "create"=>{
            s.set("checkpoint","active",&json!({"planDigest":hash,"plan":plan,"task":s.task}))?;
            Ok(json!({"created":true,"planDigest":hash,"modelResumed":false}))
        }
        "inspect"=>{
            let stored=s.get("checkpoint","active")?.ok_or("checkpoint not found")?;
            let expected=stored["planDigest"].as_str().ok_or("corrupted checkpoint digest")?;
            Ok(json!({"valid":hash==expected,"digest":expected,"currentDigest":hash,
                "canAutoResume":false,"checkpoint":stored}))
        }
        "resume"=>{
            let stored=s.get("checkpoint","active")?.ok_or("checkpoint not found")?;
            if stored["planDigest"]!=hash {return Err("stale checkpoint; reconcile plan first".into());}
            Ok(json!({"reconciled":true,"ready":ready_nodes(&plan)?,
                "agentExecutionResumed":false,"externalSideEffectsReplayed":false}))
        }
        _=>Err("checkpoint commands: create, inspect, resume".into())
    }
}
pub fn simulator(s:&State,seed:u64)->Result<Value,String>{
    let plan=active(s)?;
    let ready=ready_nodes(&plan)?;
    let mut events=Vec::new();
    for (i,n) in ready.iter().enumerate().take(80) {
        let h=seed.wrapping_add(i as u64).wrapping_mul(0x9e3779b97f4a7c15);
        let failed=(h>>28)%7==0;
        events.push(json!({"node":n["id"],"faultInjected":failed,
            "outcome":if failed{"simulated_failure"}else{"simulated_success"},
            "externalSideEffects":false}));
    }
    Ok(json!({"tool":"simulator","seed":seed,"simulated":events,"actualExecutions":0,
        "disclaimer":"Deterministic dry-run of currently ready task nodes only"}))
}
#[cfg(test)]
mod tests{
    use super::*;
    #[test]
    fn cyclic_plan_is_rejected(){
        let c=json!({"nodes":[
            {"id":"a","deps":["b"],"acceptance":"one"},
            {"id":"b","deps":["a"],"acceptance":"two"}]});
        assert!(validate(&c).is_err());
    }
    #[test]
    fn readiness_requires_verified_predecessor(){
        let p=json!({"nodes":[
            {"id":"a","deps":[],"acceptance":"first","status":"reported_completed"},
            {"id":"b","deps":["a"],"acceptance":"next","status":"planned"}]});
        assert!(ready_nodes(&p).unwrap().is_empty());
    }
}
