//! Standalone CLI wrapper for `jani-verifier`.
use jani_verifier::{verify_contract, VerifyOptions};
use std::path::PathBuf;

fn main() {
    if let Err(err) = run() {
        eprintln!("jani-verifier: {err}");
        std::process::exit(2);
    }
}

fn take(args: &[String], i: &mut usize, flag: &str) -> Result<String, String> {
    *i += 1;
    args.get(*i)
        .cloned()
        .ok_or_else(|| format!("{flag} requires a value"))
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args.iter().any(|a| a == "--help" || a == "-h") {
        println!(
            "usage: jani-verifier --contract FILE [--repo DIR] [--state-dir DIR] [--signing-key FILE --key-id ID] [--verifier-id ID] [--out FILE]"
        );
        return Ok(());
    }
    let mut repo = std::env::current_dir().map_err(|e| e.to_string())?;
    let mut state_dir: Option<PathBuf> = None;
    let mut contract: Option<PathBuf> = None;
    let mut signing_key: Option<PathBuf> = None;
    let mut key_id: Option<String> = None;
    let mut verifier_id = "jani-verifier".to_string();
    let mut out_path: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--repo" => repo = PathBuf::from(take(&args, &mut i, "--repo")?),
            "--state-dir" => state_dir = Some(PathBuf::from(take(&args, &mut i, "--state-dir")?)),
            "--contract" => contract = Some(PathBuf::from(take(&args, &mut i, "--contract")?)),
            "--signing-key" => {
                signing_key = Some(PathBuf::from(take(&args, &mut i, "--signing-key")?))
            }
            "--key-id" => key_id = Some(take(&args, &mut i, "--key-id")?),
            "--verifier-id" => verifier_id = take(&args, &mut i, "--verifier-id")?,
            "--out" => out_path = Some(PathBuf::from(take(&args, &mut i, "--out")?)),
            other => return Err(format!("unknown argument '{other}'")),
        }
        i += 1;
    }
    let contract_path = contract.ok_or("--contract is required")?;
    let state = state_dir.unwrap_or_else(|| repo.join(".jani-state"));
    let receipt = verify_contract(&VerifyOptions {
        root: &repo,
        state_dir: &state,
        contract_path: &contract_path,
        signing_key_path: signing_key.as_deref(),
        key_id: key_id.as_deref(),
        verifier_id: &verifier_id,
        credential_env_names: &[],
        out_path: out_path.as_deref(),
    })?;
    println!(
        "{}",
        serde_json::to_string_pretty(&receipt).map_err(|e| e.to_string())?
    );
    if !receipt.passed {
        return Err("one or more acceptance or scope checks failed".into());
    }
    Ok(())
}
