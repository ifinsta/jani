use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    env, fs,
    path::{Component, Path, PathBuf},
};

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct Finding {
    id: String,
    status: String,
    kind: String,
    document: String,
    line: usize,
    statement: String,
    target: String,
    reason: String,
}
#[derive(Deserialize)]
struct Claim {
    id: String,
    kind: String,
    path: String,
    #[serde(default)]
    needle: String,
    #[serde(default)]
    statement: String,
}
fn id(s: &str) -> String {
    let mut h = 0xcbf29ce484222325_u64;
    for b in s.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("CLM-{h:016x}")
}
fn rel(p: &str) -> bool {
    !p.is_empty()
        && !p.contains('\\')
        && Path::new(p)
            .components()
            .all(|x| matches!(x, Component::Normal(_)))
}
fn files(root: &Path, at: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    for e in fs::read_dir(root.join(at)).map_err(|e| e.to_string())? {
        let e = e.map_err(|e| e.to_string())?;
        let t = e.file_type().map_err(|e| e.to_string())?;
        if t.is_symlink() {
            continue;
        }
        let name = e.file_name().to_string_lossy().into_owned();
        let p = at.join(&name);
        if t.is_dir() {
            if !matches!(
                name.as_str(),
                ".git" | "target" | "claims" | ".jani" | "node_modules"
            ) {
                files(root, &p, out)?;
            }
        } else if p.extension().is_some_and(|s| s == "md") {
            out.push(p);
        }
        if out.len() > 2000 {
            return Err("too many Markdown documents".into());
        }
    }
    Ok(())
}
fn finding(
    id: String,
    doc: &str,
    n: usize,
    statement: &str,
    kind: &str,
    target: &str,
    reason: &str,
) -> Finding {
    Finding {
        id,
        status: "open".into(),
        kind: kind.into(),
        document: doc.into(),
        line: n,
        statement: statement.chars().take(220).collect(),
        target: target.into(),
        reason: reason.into(),
    }
}
fn claim_test(root: &Path, c: &Claim) -> Result<(), String> {
    if !rel(&c.path) {
        return Err("unsafe evidence path".into());
    }
    let path = root.join(&c.path);
    if !path
        .canonicalize()
        .is_ok_and(|p| p.starts_with(root) && p.is_file())
    {
        return Err("evidence file does not exist".into());
    }
    if c.kind == "file_exists" {
        return Ok(());
    }
    if !matches!(c.kind.as_str(), "source_contains" | "source_absent")
        || c.needle.is_empty()
        || c.needle.len() > 250
    {
        return Err("unsupported or empty evidence assertion".into());
    }
    if fs::metadata(&path).map_err(|e| e.to_string())?.len() > 2_000_000 {
        return Err("oversized evidence".into());
    }
    let exists = fs::read_to_string(path)
        .map_err(|e| e.to_string())?
        .contains(&c.needle);
    if exists == (c.kind == "source_contains") {
        Ok(())
    } else {
        Err("claim contradicted by source".into())
    }
}
fn scan_doc(
    root: &Path,
    doc: &Path,
    out: &mut BTreeMap<String, Finding>,
    passed: &mut usize,
) -> Result<(), String> {
    let path = root.join(doc);
    if fs::metadata(&path).map_err(|e| e.to_string())?.len() > 1_000_000 {
        return Err("oversized Markdown".into());
    }
    let txt = fs::read_to_string(path).map_err(|e| e.to_string())?;
    let link = Regex::new(r"\[[^\]]*\]\(([^)\s]+)\)").unwrap();
    let source_ref =
        Regex::new(r"\x60((?:backend|contract|tools|docs)/[a-zA-Z0-9_./-]+)\x60").unwrap();
    let d = doc.to_string_lossy().replace('\\', "/");
    let mut prev = String::new();
    let mut fenced = false;
    for (ix, line) in txt.lines().enumerate() {
        let n = ix + 1;
        let s = line.trim();
        if s.starts_with("~~~") || s.as_bytes().starts_with(&[96, 96, 96]) {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        if let Some(c) = s.strip_prefix("<!-- jani-claim:") {
            let parsed = c
                .strip_suffix("-->")
                .and_then(|x| serde_json::from_str::<Claim>(x.trim()).ok());
            match parsed {
                Some(c)
                    if !c.id.is_empty()
                        && c.id.len() < 96
                        && c.id
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') =>
                {
                    if let Err(reason) = claim_test(root, &c) {
                        let statement = if c.statement.is_empty() {
                            &prev
                        } else {
                            &c.statement
                        };
                        let f = finding(c.id, &d, n, statement, "claim_mismatch", &c.path, &reason);
                        out.insert(f.id.clone(), f);
                    } else {
                        *passed += 1;
                    }
                }
                _ => {
                    let f = finding(
                        id(&format!("{d}:{n}:syntax")),
                        &d,
                        n,
                        &prev,
                        "invalid_claim",
                        "",
                        "invalid annotation",
                    );
                    out.insert(f.id.clone(), f);
                }
            }
            continue;
        }
        for m in link.captures_iter(line) {
            let href = m.get(1).unwrap().as_str();
            if href.starts_with('#')
                || href.starts_with('/')
                || href.contains("://")
                || href.starts_with("mailto:")
                || href.contains('%')
            {
                continue;
            }
            let href = href.split('#').next().unwrap_or("");
            if href.is_empty() {
                continue;
            }
            let target = root.join(doc.parent().unwrap_or(Path::new(""))).join(href);
            if !target
                .canonicalize()
                .is_ok_and(|p| p.starts_with(root) && p.exists())
            {
                let f = finding(
                    id(&format!("{d}:link:{href}")),
                    &d,
                    n,
                    s,
                    "broken_link",
                    href,
                    "relative link target missing",
                );
                out.insert(f.id.clone(), f);
            }
        }
        if !d.to_lowercase().contains("proposal") {
            for m in source_ref.captures_iter(line) {
                let target = m.get(1).unwrap().as_str();
                if target.contains("..")
                    || !rel(target)
                    || (target.starts_with("tools/") && !target.starts_with("tools/jani-"))
                {
                    continue;
                }
                if !root.join(target).exists() {
                    let f = finding(
                        id(&format!("{d}:ref:{target}")),
                        &d,
                        n,
                        s,
                        "missing_source_reference",
                        target,
                        "source path missing",
                    );
                    out.insert(f.id.clone(), f);
                }
            }
        }
        if !s.is_empty() {
            prev = s.to_owned();
        }
    }
    Ok(())
}
fn agent_reports(root: &Path, out: &mut BTreeMap<String, Finding>) -> Result<(), String> {
    let dir = root.join("claims/agent-reviews");
    if !dir.exists() {
        return Ok(());
    }
    for e in fs::read_dir(dir).map_err(|e| e.to_string())? {
        let e = e.map_err(|e| e.to_string())?;
        let path = e.path();
        if path.extension().is_none_or(|x| x != "json")
            || e.file_type().map_err(|e| e.to_string())?.is_symlink()
        {
            continue;
        }
        if fs::metadata(&path).map_err(|e| e.to_string())?.len() > 131_072 {
            return Err("AI report exceeds limit".into());
        }
        let v: Value = serde_json::from_slice(&fs::read(path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        let candidates = v["candidates"]
            .as_array()
            .ok_or("AI report missing candidates array")?;
        if candidates.len() > 100 {
            return Err("too many AI candidates".into());
        }
        for c in candidates {
            let d = c["document"].as_str().unwrap_or("");
            let n = c["line"].as_u64().unwrap_or(0) as usize;
            let stmt = c["statement"].as_str().unwrap_or("");
            let src = c["sourcePath"].as_str().unwrap_or("");
            let quote = c["sourceQuote"].as_str().unwrap_or("");
            let explanation = c["explanation"].as_str().unwrap_or("");
            if !rel(d)
                || !d.ends_with(".md")
                || !rel(src)
                || n == 0
                || stmt.is_empty()
                || quote.len() < 6
                || explanation.len() < 8
                || quote.len() > 500
            {
                return Err("invalid AI candidate".into());
            }
            let docfile = root.join(d);
            let sourcefile = root.join(src);
            if !docfile.canonicalize().is_ok_and(|p| p.starts_with(root))
                || !sourcefile.canonicalize().is_ok_and(|p| p.starts_with(root))
                || fs::symlink_metadata(&docfile).is_ok_and(|m| m.file_type().is_symlink())
                || fs::symlink_metadata(&sourcefile).is_ok_and(|m| m.file_type().is_symlink())
            {
                return Err("unsafe AI evidence path".into());
            }
            let dc = fs::read_to_string(docfile).map_err(|e| e.to_string())?;
            let sc = fs::read_to_string(sourcefile).map_err(|e| e.to_string())?;
            if dc
                .lines()
                .nth(n - 1)
                .is_none_or(|line| !line.contains(stmt))
                || !sc.contains(quote)
            {
                return Err("stale or fabricated AI evidence".into());
            }
            let key = id(&format!("agent:{d}:{n}:{src}:{quote}"));
            out.insert(
                key.clone(),
                Finding {
                    id: key,
                    status: "needs_human_review".into(),
                    kind: "ai_candidate".into(),
                    document: d.into(),
                    line: n,
                    statement: stmt.into(),
                    target: src.into(),
                    reason: explanation.into(),
                },
            );
        }
    }
    Ok(())
}
fn audit(root: &Path) -> Result<Value, String> {
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    let mut docs = Vec::new();
    files(&root, Path::new(""), &mut docs)?;
    docs.sort();
    let mut out = BTreeMap::new();
    let mut passed = 0;
    for doc in &docs {
        scan_doc(&root, doc, &mut out, &mut passed)?;
    }
    agent_reports(&root, &mut out)?;
    let open = out.values().filter(|x| x.status == "open").count();
    let review = out
        .values()
        .filter(|x| x.status == "needs_human_review")
        .count();
    Ok(
        json!({"schemaVersion":"jani.claims/v1","documentsScanned":docs.len(),"verifiedClaims":passed,"open":open,"review":review,"findings":out.into_values().collect::<Vec<_>>()}),
    )
}
fn summary(v: &Value) -> String {
    let mut s=format!("# Open documentation claims\n\nScanned {} Markdown files. Verified claims: {}. Open: {}. AI review candidates: {}.\n\n",
  v["documentsScanned"],v["verifiedClaims"],v["open"],v["review"]);
    for c in v["findings"].as_array().into_iter().flatten() {
        s.push_str(&format!(
            "## {} ({})\n\n- Document: {}:{}\n- Kind: {}\n- Source: {}\n- Reason: {}\n\n",
            c["id"].as_str().unwrap_or(""),
            c["status"].as_str().unwrap_or(""),
            c["document"].as_str().unwrap_or(""),
            c["line"],
            c["kind"].as_str().unwrap_or(""),
            c["target"].as_str().unwrap_or(""),
            c["reason"].as_str().unwrap_or("")
        ));
    }
    s.push_str("These checks do not prove that every prose statement is correct. AI candidates require human review.\n");
    s
}
fn review_queue(root: &Path) -> Result<Value, String> {
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    let mut docs = Vec::new();
    files(&root, Path::new(""), &mut docs)?;
    docs.sort();
    let mut review = Vec::new();
    for doc in docs {
        let d = doc.to_string_lossy().replace('\\', "/");
        if d.to_lowercase().contains("proposal") {
            continue;
        }
        let body = fs::read_to_string(root.join(&doc)).map_err(|e| e.to_string())?;
        let mut fenced = false;
        for (i, line) in body.lines().enumerate() {
            let text = line.trim();
            if text.starts_with("~~~") || text.as_bytes().starts_with(&[96, 96, 96]) {
                fenced = !fenced;
                continue;
            }
            if fenced
                || text.starts_with("<!--")
                || text.starts_with("|")
                || text.len() < 45
                || text.len() > 450
                || text.starts_with('#')
            {
                continue;
            }
            let low = text.to_ascii_lowercase();
            if [
                " implements ",
                " supports ",
                " requires ",
                " currently ",
                " is implemented ",
                " cannot ",
                " must ",
                " does not ",
                " never ",
                " is available ",
            ]
            .iter()
            .any(|term| low.contains(term))
            {
                review.push(json!({"document":d,"line":i+1,"statement":text}));
                if review.len() >= 120 {
                    break;
                }
            }
        }
        if review.len() >= 120 {
            break;
        }
    }
    Ok(json!({"schemaVersion":"jani.claims.review-queue/v1",
   "candidates":review,
   "instructions":"Read each documented statement and current source before concluding anything. Submit quoted source evidence to claims/agent-reviews/*.json using the Jani agent reviewer protocol. Do not claim an AI verdict is verified."}))
}
fn run() -> Result<(), String> {
    let mut a = env::args().skip(1);
    let cmd = a.next().unwrap_or_else(|| "check".into());
    let mut root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    while let Some(flag) = a.next() {
        if flag == "--root" {
            root = PathBuf::from(a.next().ok_or("--root needs path")?);
        } else {
            return Err(format!("unknown argument {flag}"));
        }
    }
    if cmd == "queue" {
        println!(
            "{}",
            serde_json::to_string_pretty(&review_queue(&root)?).map_err(|e| e.to_string())?
        );
        return Ok(());
    }
    let report = audit(&root)?;
    let mut index = serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?;
    index.push(b'\n');
    let md = summary(&report);
    match cmd.as_str() {
        "sync" => {
            fs::create_dir_all(root.join("claims")).map_err(|e| e.to_string())?;
            fs::write(root.join("claims/index.json"), index).map_err(|e| e.to_string())?;
            fs::write(root.join("claims/OPEN.md"), &md).map_err(|e| e.to_string())?;
            println!("{md}");
        }
        "scan" => println!("{md}"),
        "check" | "strict" => {
            if fs::read(root.join("claims/index.json")).map_err(|e| e.to_string())? != index
                || fs::read(root.join("claims/OPEN.md")).map_err(|e| e.to_string())?
                    != md.as_bytes()
            {
                return Err("claims register stale; run cargo run -p jani-claims -- sync".into());
            }
            if cmd == "strict" && report["open"].as_u64().unwrap_or(0) > 0 {
                return Err("unresolved claims".into());
            }
            println!(
                "jani-claims: register current; {} unresolved",
                report["open"]
            );
        }
        _ => return Err("usage: jani-claims scan|sync|check|strict|queue [--root PATH]".into()),
    }
    Ok(())
}
fn main() {
    if let Err(e) = run() {
        eprintln!("jani-claims: {e}");
        std::process::exit(1);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn anchored_claim_checks_source() {
        let root = std::env::temp_dir().join(format!("claims-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("docs")).unwrap();
        fs::create_dir_all(root.join("backend")).unwrap();
        fs::write(root.join("backend/main.rs"), "mod mcp;").unwrap();
        fs::write(root.join("docs/notes.md"),"No MCP.\n<!-- jani-claim: {\"id\":\"MCP-ABSENT\",\"kind\":\"source_absent\",\"path\":\"backend/main.rs\",\"needle\":\"mod mcp;\"} -->\n[missing](absent.md)\n").unwrap();
        let r = audit(&root).unwrap();
        assert_eq!(r["open"], 2);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn agent_evidence_is_not_treated_as_proven() {
        let root = std::env::temp_dir().join(format!("claims-ai-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("docs")).unwrap();
        fs::create_dir_all(root.join("backend")).unwrap();
        fs::create_dir_all(root.join("claims/agent-reviews")).unwrap();
        fs::write(root.join("backend/main.rs"), "fn evidence() {}").unwrap();
        fs::write(root.join("docs/notes.md"), "The workflow is complete.").unwrap();
        let candidate = json!({"candidates":[{
         "document":"docs/notes.md","line":1,"statement":"The workflow is complete.",
         "sourcePath":"backend/main.rs","sourceQuote":"fn evidence()",
         "explanation":"The agent suspects the statement is not supported by the current source."
        }]});
        fs::write(
            root.join("claims/agent-reviews/review.json"),
            candidate.to_string(),
        )
        .unwrap();
        let result = audit(&root).unwrap();
        assert_eq!(result["review"], 1);
        assert_eq!(result["open"], 0);
        assert_eq!(result["findings"][0]["status"], "needs_human_review");
        fs::write(root.join("backend/main.rs"), "fn changed() {}").unwrap();
        assert!(audit(&root).is_err());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn mcp_tool_name_is_not_treated_as_source_directory() {
        let root = std::env::temp_dir().join(format!("claims-names-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("docs")).unwrap();
        fs::write(
            root.join("docs/notes.md"),
            "Use tools/call in an MCP request.",
        )
        .unwrap();
        let r = audit(&root).unwrap();
        assert_eq!(r["open"], 0);
        fs::remove_dir_all(root).unwrap();
    }
}
