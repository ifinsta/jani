use crate::config::JResult;
use std::fs::{self, File, OpenOptions};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

pub const EXCERPT_LIMIT: usize = 24_576;

#[derive(Debug)]
pub struct ProcessOutput {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

pub fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        return s.to_owned();
    }
    let end = s
        .char_indices()
        .take_while(|(i, _)| *i < n)
        .last()
        .map(|(i, _)| i)
        .unwrap_or(0);
    format!("{}...[truncated]", &s[..end])
}

pub fn unique_id() -> String {
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!(
        "{}-{}-{}",
        std::process::id(),
        t,
        NEXT_ID.fetch_add(1, Ordering::Relaxed)
    )
}

// Avoid exposing model/tool output to other local users through temporary files.
fn secure_output(path: &Path) -> JResult<File> {
    let mut opts = OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts.open(path).map_err(|e| e.to_string())
}

// Redirect output to files rather than leaving child pipes full while polling a deadline.
// The file is truncated to a bounded excerpt for the tool response. This is not an
// output-volume quota or a process sandbox; host-level isolation is still required.
pub fn execute(cmd: &mut Command, cwd: &Path, max_secs: u64) -> JResult<ProcessOutput> {
    let key = unique_id();
    let out_path = std::env::temp_dir().join(format!("jani-{key}-stdout"));
    let err_path = std::env::temp_dir().join(format!("jani-{key}-stderr"));
    let result = (|| -> JResult<ProcessOutput> {
        let out = secure_output(&out_path)?;
        let err = secure_output(&err_path)?;
        // Model credentials must never reach child processes, build scripts, or test code.
        cmd.env_remove("JANI_API_KEY")
            .env_remove("OPENAI_API_KEY")
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::from(out))
            .stderr(Stdio::from(err));
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("failed to launch command: {e}"))?;
        let start = Instant::now();
        let (status, timed_out) = loop {
            let output_limit = [out_path.as_path(), err_path.as_path()]
                .iter()
                .filter_map(|p| fs::metadata(p).ok().map(|m| m.len()))
                .sum::<u64>();
            if output_limit > 4_000_000 {
                let _ = child.kill();
                let _ = child.wait();
                break (None, true);
            }
            match child.try_wait().map_err(|e| e.to_string())? {
                Some(x) => break (Some(x), false),
                None if start.elapsed() >= Duration::from_secs(max_secs) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break (None, true);
                }
                None => thread::sleep(Duration::from_millis(50)),
            }
        };
        let excerpt = |p: &Path| -> JResult<String> {
            let mut f = File::open(p).map_err(|e| e.to_string())?;
            use std::io::Read;
            let mut b = Vec::new();
            let _ = (&mut f).take(EXCERPT_LIMIT as u64).read_to_end(&mut b);
            Ok(String::from_utf8_lossy(&b).into_owned())
        };
        Ok(ProcessOutput {
            code: status.and_then(|x| x.code()),
            stdout: excerpt(&out_path)?,
            stderr: excerpt(&err_path)?,
            timed_out,
        })
    })();
    let _ = fs::remove_file(out_path);
    let _ = fs::remove_file(err_path);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn children_do_not_inherit_provider_credentials() {
        #[cfg(windows)]
        let mut cmd = {
            let mut c = Command::new("cmd");
            c.args(["/C", "set JANI_API_KEY"]);
            c
        };
        #[cfg(unix)]
        let mut cmd = {
            let mut c = Command::new("sh");
            c.args(["-c", "printf %s \"$JANI_API_KEY\""]);
            c
        };
        cmd.env("JANI_API_KEY", "super-secret-value");
        let out = execute(&mut cmd, Path::new("."), 10).unwrap();
        assert!(
            !out.stdout.contains("super-secret-value"),
            "child inherited API key"
        );
    }
}
