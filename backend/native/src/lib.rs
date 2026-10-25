//! Standalone native toolkit, deliberately not a second ungated execution runtime.
pub mod state;
mod repo;
mod plans;
mod memory;

use serde_json::{json,Value};
use sha2::{Digest,Sha256};
use std::path::Path;

pub fn digest(bytes:&[u8])->String {hex::encode(Sha256::digest(bytes))}

pub const TOOLS: &[&str]=&[
    "index","context","planner","pruner","checkpoint","verifier",
    "impact","scheduler","workspace","evidence","optimizer","simulator"
];

pub fn mutation(tool:&str,operation:&str)->bool {
    matches!((tool,operation),
        ("index","build") | ("context","pin") | ("context","assemble") |
        ("planner","create") | ("planner","report") | ("planner","verify") |
        ("pruner","run") | ("checkpoint","create") |
        ("workspace","snapshot") | ("evidence","record"))
}
pub fn run(
    root:&Path,state_dir:&Path,tenant:&str,task:&str,
    tool:&str,args:&[String],human_approved:bool
)->Result<Value,String>{
    if !TOOLS.contains(&tool){return Err(format!("unknown native tool: {tool}"));}
    let op=args.first().map(String::as_str).unwrap_or("");
    let rest=&args[usize::from(!args.is_empty())..];
    let mut s=state::State::open(root,state_dir,tenant,task)?;
    match tool {
        "index"=>match op {
            "build"=>repo::build(&mut s),
            "search"=>repo::search(&s,rest.first().ok_or("index search requires query")?),
            _=>Err("index commands: build, search QUERY".into())
        },
        "context"=>memory::context(&mut s,op,rest),
        "planner"=>plans::planner(&mut s,op,rest,human_approved),
        "pruner"=>memory::pruner(&mut s,op),
        "checkpoint"=>plans::checkpoint(&mut s,op),
        "verifier"=>repo::verifier(&s,op),
        "impact"=>match op {
            "analyze"=>repo::impact(&s,rest.first().ok_or("impact analyze requires PATH")?),
            _=>Err("impact commands: analyze PATH".into())
        },
        "scheduler"=>match op {
            "next"=>plans::scheduler(&s),
            _=>Err("scheduler commands: next".into())
        },
        "workspace"=>repo::workspace(&mut s,op),
        "evidence"=>memory::evidence(&mut s,op,rest),
        "optimizer"=>match op {
            "review"=>memory::optimizer(&s),
            _=>Err("optimizer commands: review".into())
        },
        "simulator"=>match op {
            "run"=>{
                let seed=rest.first().map(|x|x.parse::<u64>())
                    .transpose().map_err(|_|"simulator seed must be an integer")?.unwrap_or(20261025);
                plans::simulator(&s,seed)
            }
            _=>Err("simulator commands: run [SEED]".into())
        },
        _=>Ok(json!({"unreachable":true}))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    #[test]
    fn all_tools_registered(){
        assert_eq!(TOOLS.len(),12);
        let mut names=TOOLS.to_vec();names.sort();names.dedup();
        assert_eq!(names.len(),12);
    }
    #[test]
    fn isolated_repo_index_and_plan(){
        let root=std::env::temp_dir().join(format!("jani-native-test-{}",
            std::process::id()));
        let _=fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/main.rs"),"pub fn greeting() {}").unwrap();
        fs::write(root.join("plan.json"),r#"{"nodes":[{"id":"one","deps":[],"acceptance":"compile"}]}"#).unwrap();
        let state=root.join(".jani");
        let call=|tool:&str,params:Vec<&str>|run(&root,&state,"tenant-a","task-1",
            tool,&params.into_iter().map(str::to_string).collect::<Vec<_>>(),false);
        assert_eq!(call("index",vec!["build"]).unwrap()["updated"],2);
        assert!(call("index",vec!["search","greeting"]).unwrap()["totalMatches"]>=1);
        assert!(call("planner",vec!["create","plan.json"]).unwrap()["created"]==true);
        assert_eq!(call("scheduler",vec!["next"]).unwrap()["count"],1);
        assert!(call("simulator",vec!["run","22"]).unwrap()["actualExecutions"]==0);
        assert!(run(&root,&state,"tenant-b","task-1","planner",
            &["inspect".into()],false).is_err());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn pruning_keeps_pinned_references_and_evidence() {
        let root=std::env::temp_dir().join(format!("jani-native-retention-{}",
            std::process::id()));
        let _=fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"),"pub fn keep() {}").unwrap();
        let state=root.join(".jani");
        let run_tool=|tool:&str,params:&[&str]|run(&root,&state,"tenant","job",
            tool,&params.iter().map(|s|s.to_string()).collect::<Vec<_>>(),false);
        run_tool("index",&["build"]).unwrap();
        run_tool("context",&["pin","src/lib.rs"]).unwrap();
        run_tool("context",&["assemble","keep","500"]).unwrap();
        run_tool("evidence",&["record","receipt-1","src/lib.rs"]).unwrap();
        let before=run_tool("context",&["show"]).unwrap();
        assert!(before["items"].as_array().unwrap().len()>0);
        let pruned=run_tool("pruner",&["run"]).unwrap();
        assert_eq!(pruned["authoritativeEvidencePruned"],false);
        assert_eq!(pruned["pinsKept"],1);
        let after=run_tool("context",&["show"]).unwrap();
        assert!(after["items"].as_array().unwrap().is_empty());
        assert_eq!(run_tool("evidence",&["check","receipt-1"]).unwrap()["digestMatches"],true);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn checkpoint_detects_plan_changes_and_cannot_resume_model() {
        let root=std::env::temp_dir().join(format!("jani-native-checkpoint-{}",
            std::process::id()));
        let _=fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("plan.json"),r#"{"nodes":[{"id":"work","deps":[],"acceptance":"done"}]}"#).unwrap();
        let dir=root.join(".jani");
        run(&root,&dir,"t","task","planner",&["create".into(),"plan.json".into()],false).unwrap();
        run(&root,&dir,"t","task","checkpoint",&["create".into()],false).unwrap();
        let r=run(&root,&dir,"t","task","checkpoint",&["resume".into()],false).unwrap();
        assert_eq!(r["agentExecutionResumed"],false);
        assert_eq!(r["externalSideEffectsReplayed"],false);
        fs::remove_dir_all(root).unwrap();
    }

}
