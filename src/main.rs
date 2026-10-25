mod agent;
mod config;
mod exec;
mod model;
mod process;
mod skills;
mod workspace;

use config::{Config, JResult};
use skills::Catalog;

fn main() {
    if let Err(err) = run() {
        eprintln!("jani: {err}");
        std::process::exit(1);
    }
}

fn run() -> JResult<()> {
    let cfg = Config::parse()?;
    if cfg.command == "version" {
        println!("jani {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if cfg.command == "help" {
        println!("{}", config::help());
        return Ok(());
    }
    let catalog = Catalog::load(&cfg)?;
    match cfg.command.as_str() {
        "skills" => {
            println!("{}", catalog.listing());
            Ok(())
        }
        "doctor" => {
            println!("Jani v{}", env!("CARGO_PKG_VERSION"));
            println!("Worktree: {}", cfg.root.display());
            println!("Discovered {} skills", catalog.skills.len());
            println!("Model: {}", cfg.model);
            println!(
                "Model key: {}",
                if cfg.api_key.is_some() {
                    "SET"
                } else {
                    "MISSING"
                }
            );
            println!("Writes: {}; commands: {}", cfg.allow_write, cfg.allow_exec);
            println!(
                "Write scope: {}",
                if cfg.write_paths.is_empty() {
                    "none".to_string()
                } else {
                    cfg.write_paths
                        .iter()
                        .map(|p| p.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            );
            Ok(())
        }
        "run" => {
            let prompt = cfg.prompt.as_ref().ok_or("no prompt supplied")?;
            let mut agent = agent::Agent::new(&cfg, &catalog);
            println!("{}", agent.run(prompt)?);
            Ok(())
        }
        _ => Err("unrecognized command".into()),
    }
}
