use crate::policy::EffectiveLimits;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BudgetSnapshot {
    pub task_id: String,
    pub tool_calls: u32,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub usd_spent: f64,
    pub started_at: u64,
    pub updated_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tool_calls: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_wall_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_usd: Option<f64>,
}

pub struct BudgetLedger {
    task_id: String,
    dir: PathBuf,
    started_at: u64,
    max_tool_calls: Option<u32>,
    max_wall_seconds: Option<u64>,
    max_usd: Option<f64>,
    usd_per_1k_input: f64,
    usd_per_1k_output: f64,
    tool_calls: u32,
    input_tokens: u64,
    output_tokens: u64,
    usd_spent: f64,
}

impl BudgetLedger {
    /// Open the ledger for a task. Usage accumulated by earlier runs is
    /// reloaded so a restart cannot reset an exhausted budget, and persisted
    /// limits are intersected with the current limits so loosening policy
    /// never restores spent capacity.
    pub fn new(state_dir: &Path, task_id: &str, limits: &EffectiveLimits) -> Result<Self, String> {
        let mut ledger = Self {
            task_id: task_id.into(),
            dir: state_dir.join("budgets"),
            started_at: crate::types::now_unix(),
            max_tool_calls: limits.max_tool_calls,
            max_wall_seconds: limits.max_wall_seconds,
            max_usd: limits.max_usd,
            usd_per_1k_input: limits.usd_per_1k_input_tokens,
            usd_per_1k_output: limits.usd_per_1k_output_tokens,
            tool_calls: 0,
            input_tokens: 0,
            output_tokens: 0,
            usd_spent: 0.0,
        };
        let path = ledger.path();
        match fs::read_to_string(&path) {
            Ok(text) => {
                let stored: BudgetSnapshot = serde_json::from_str(&text)
                    .map_err(|e| format!("invalid budget ledger {}: {e}", path.display()))?;
                ledger.tool_calls = stored.tool_calls;
                ledger.input_tokens = stored.input_tokens;
                ledger.output_tokens = stored.output_tokens;
                ledger.usd_spent = stored.usd_spent;
                if stored.started_at > 0 {
                    ledger.started_at = stored.started_at;
                }
                ledger.max_tool_calls = stricter(ledger.max_tool_calls, stored.max_tool_calls);
                ledger.max_wall_seconds =
                    stricter(ledger.max_wall_seconds, stored.max_wall_seconds);
                ledger.max_usd = stricter_f64(ledger.max_usd, stored.max_usd);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "cannot read budget ledger {}: {error}",
                    path.display()
                ))
            }
        }
        Ok(ledger)
    }

    pub fn reserve_tool_call(&mut self) -> Result<(), String> {
        self.check_deadline()?;
        if let Some(max) = self.max_tool_calls {
            if self.tool_calls >= max {
                return Err(format!("tool-call budget exhausted ({max})"));
            }
        }
        self.tool_calls += 1;
        self.save()
    }

    pub fn check_deadline(&self) -> Result<(), String> {
        if let Some(max) = self.max_wall_seconds {
            let elapsed = crate::types::now_unix().saturating_sub(self.started_at);
            if elapsed >= max {
                return Err(format!("task deadline exhausted ({max}s)"));
            }
        }
        Ok(())
    }

    pub fn check_model_budget(&self) -> Result<(), String> {
        self.check_deadline()?;
        if let Some(max) = self.max_usd {
            if self.usd_spent >= max {
                return Err(format!("model budget exhausted (${max:.2})"));
            }
        }
        Ok(())
    }

    pub fn record_usage(&mut self, input_tokens: u64, output_tokens: u64) -> Result<(), String> {
        self.input_tokens = self.input_tokens.saturating_add(input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(output_tokens);
        self.usd_spent += (input_tokens as f64 / 1000.0) * self.usd_per_1k_input
            + (output_tokens as f64 / 1000.0) * self.usd_per_1k_output;
        self.save()?;
        if let Some(max) = self.max_usd {
            if self.usd_spent > max {
                return Err(format!(
                    "model budget exceeded: ${:.4} spent against ${max:.2}",
                    self.usd_spent
                ));
            }
        }
        Ok(())
    }

    fn save(&self) -> Result<(), String> {
        fs::create_dir_all(&self.dir)
            .map_err(|e| format!("cannot create budget directory: {e}"))?;
        let snapshot = self.snapshot();
        let path = self.path();
        let temp = path.with_extension(format!("tmp-{}", crate::process::unique_id()));
        fs::write(
            &temp,
            serde_json::to_vec_pretty(&snapshot).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        fs::rename(&temp, &path).map_err(|e| e.to_string())?;
        Ok(())
    }

    fn snapshot(&self) -> BudgetSnapshot {
        BudgetSnapshot {
            task_id: self.task_id.clone(),
            tool_calls: self.tool_calls,
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            usd_spent: self.usd_spent,
            started_at: self.started_at,
            updated_at: crate::types::now_unix(),
            max_tool_calls: self.max_tool_calls,
            max_wall_seconds: self.max_wall_seconds,
            max_usd: self.max_usd,
        }
    }

    fn path(&self) -> PathBuf {
        let safe: String = self
            .task_id
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        self.dir.join(format!("{safe}.json"))
    }
}

fn stricter<T: Ord>(current: Option<T>, stored: Option<T>) -> Option<T> {
    match (current, stored) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (current, stored) => current.or(stored),
    }
}

fn stricter_f64(current: Option<f64>, stored: Option<f64>) -> Option<f64> {
    match (current, stored) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (current, stored) => current.or(stored),
    }
}

pub fn show(state_dir: &Path, task_id: &str) -> Result<Value, String> {
    let safe: String = task_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let path = state_dir.join("budgets").join(format!("{safe}.json"));
    let text = fs::read_to_string(&path)
        .map_err(|e| format!("no budget ledger for task '{task_id}': {e}"))?;
    serde_json::from_str(&text).map_err(|e| format!("invalid budget ledger: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits() -> EffectiveLimits {
        EffectiveLimits {
            max_tool_calls: Some(2),
            max_wall_seconds: Some(60),
            max_usd: Some(0.01),
            usd_per_1k_input_tokens: 1.0,
            usd_per_1k_output_tokens: 1.0,
            ..Default::default()
        }
    }

    #[test]
    fn tool_call_and_model_budgets_are_hard_limits() {
        let dir = std::env::temp_dir().join(format!("jani-budget-{}", crate::process::unique_id()));
        let mut ledger = BudgetLedger::new(&dir, "task-1", &limits()).unwrap();
        ledger.reserve_tool_call().unwrap();
        ledger.reserve_tool_call().unwrap();
        assert!(ledger.reserve_tool_call().is_err());
        assert!(ledger.record_usage(20, 0).is_err());
        let shown = show(&dir, "task-1").unwrap();
        assert_eq!(shown["toolCalls"], 2);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn reopening_a_ledger_preserves_usage_and_tighter_limits() {
        let dir = std::env::temp_dir().join(format!("jani-budget-{}", crate::process::unique_id()));
        {
            let mut ledger = BudgetLedger::new(&dir, "task-2", &limits()).unwrap();
            ledger.reserve_tool_call().unwrap();
            ledger.record_usage(2, 0).unwrap();
        }
        let mut looser = limits();
        looser.max_tool_calls = Some(50);
        looser.max_usd = Some(10.0);
        let mut reopened = BudgetLedger::new(&dir, "task-2", &looser).unwrap();
        // The persisted cap of two calls still applies after a policy change.
        reopened.reserve_tool_call().unwrap();
        assert!(reopened.reserve_tool_call().is_err());
        let shown = show(&dir, "task-2").unwrap();
        assert_eq!(shown["toolCalls"], 2);
        assert_eq!(shown["inputTokens"], 2);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn corrupt_ledgers_fail_closed() {
        let dir = std::env::temp_dir().join(format!("jani-budget-{}", crate::process::unique_id()));
        let ledger_dir = dir.join("budgets");
        fs::create_dir_all(&ledger_dir).unwrap();
        fs::write(ledger_dir.join("task-3.json"), b"{not json").unwrap();
        assert!(BudgetLedger::new(&dir, "task-3", &limits()).is_err());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn persisted_deadline_survives_restart() {
        let dir = std::env::temp_dir().join(format!("jani-budget-{}", crate::process::unique_id()));
        fs::create_dir_all(dir.join("budgets")).unwrap();
        fs::write(
            dir.join("budgets/task-4.json"),
            serde_json::to_vec(&BudgetSnapshot {
                task_id: "task-4".into(),
                tool_calls: 0,
                input_tokens: 0,
                output_tokens: 0,
                usd_spent: 0.0,
                started_at: crate::types::now_unix().saturating_sub(120),
                updated_at: crate::types::now_unix(),
                max_tool_calls: Some(2),
                max_wall_seconds: Some(60),
                max_usd: Some(0.01),
            })
            .unwrap(),
        )
        .unwrap();
        let ledger = BudgetLedger::new(&dir, "task-4", &limits()).unwrap();
        assert!(ledger.check_deadline().is_err());
        let _ = fs::remove_dir_all(dir);
    }
}
