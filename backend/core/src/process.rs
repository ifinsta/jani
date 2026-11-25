use crate::config::JResult;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

pub const EXCERPT_LIMIT: usize = 24_576;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessIsolation {
    Direct,
    JobObject,
    SandboxNamespace,
}

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

pub fn execute(cmd: &mut Command, cwd: &Path, max_secs: u64) -> JResult<ProcessOutput> {
    execute_with(cmd, cwd, max_secs, ProcessIsolation::Direct)
}

fn process_working_directory(cwd: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        let text = cwd.to_string_lossy();
        if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
            return PathBuf::from(format!(r"\\{rest}"));
        }
        if let Some(rest) = text.strip_prefix(r"\\?\") {
            if rest.as_bytes().get(1) == Some(&b':') {
                return PathBuf::from(rest);
            }
        }
    }
    cwd.to_path_buf()
}

// Redirect output to files rather than leaving child pipes full while polling a deadline.
// The file is truncated to a bounded excerpt for the tool response. JobObject/SandboxNamespace
// additionally bound the process tree; filesystem and network confinement come from the
// backend selected by `sandbox`.
pub fn execute_with(
    cmd: &mut Command,
    cwd: &Path,
    max_secs: u64,
    isolation: ProcessIsolation,
) -> JResult<ProcessOutput> {
    execute_with_scrubbed(cmd, cwd, max_secs, isolation, &[])
}

pub fn execute_with_scrubbed(
    cmd: &mut Command,
    cwd: &Path,
    max_secs: u64,
    isolation: ProcessIsolation,
    credential_env_names: &[String],
) -> JResult<ProcessOutput> {
    execute_with_scrubbed_cancel(cmd, cwd, max_secs, isolation, credential_env_names, None)
}

pub fn execute_with_scrubbed_cancel(
    cmd: &mut Command,
    cwd: &Path,
    max_secs: u64,
    isolation: ProcessIsolation,
    credential_env_names: &[String],
    cancelled: Option<&std::sync::atomic::AtomicBool>,
) -> JResult<ProcessOutput> {
    execute_with_stdin_cancel(
        cmd,
        cwd,
        max_secs,
        isolation,
        credential_env_names,
        None,
        cancelled,
    )
}

pub fn execute_with_stdin_cancel(
    cmd: &mut Command,
    cwd: &Path,
    max_secs: u64,
    isolation: ProcessIsolation,
    credential_env_names: &[String],
    stdin_bytes: Option<&[u8]>,
    cancelled: Option<&std::sync::atomic::AtomicBool>,
) -> JResult<ProcessOutput> {
    let key = unique_id();
    let in_path = stdin_bytes.map(|_| std::env::temp_dir().join(format!("jani-{key}-stdin")));
    let out_path = std::env::temp_dir().join(format!("jani-{key}-stdout"));
    let err_path = std::env::temp_dir().join(format!("jani-{key}-stderr"));
    let result = (|| -> JResult<ProcessOutput> {
        let out = secure_output(&out_path)?;
        let err = secure_output(&err_path)?;
        let stdin_cfg = match (stdin_bytes, in_path.as_deref()) {
            (Some(bytes), Some(p)) => {
                use std::io::Write;
                let mut f = secure_output(p)?;
                f.write_all(bytes).map_err(|e| e.to_string())?;
                drop(f);
                Stdio::from(File::open(p).map_err(|e| e.to_string())?)
            }
            _ => Stdio::null(),
        };
        // Model credentials must never reach child processes, build scripts, or test code.
        cmd.env_remove(crate::env::API_KEY)
            .env_remove(crate::env::OPENAI_API_KEY);
        for name in credential_env_names {
            cmd.env_remove(name);
        }
        cmd.current_dir(process_working_directory(cwd))
            .stdin(stdin_cfg)
            .stdout(Stdio::from(out))
            .stderr(Stdio::from(err));

        #[cfg(windows)]
        let job = if isolation == ProcessIsolation::JobObject {
            Some(JobGuard::create()?)
        } else {
            None
        };
        #[cfg(not(windows))]
        let job: Option<()> = {
            let _ = isolation;
            None
        };
        #[cfg(windows)]
        if job.is_some() {
            use std::os::windows::process::CommandExt;
            use windows_sys::Win32::System::Threading::CREATE_SUSPENDED;
            cmd.creation_flags(CREATE_SUSPENDED);
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| format!("failed to launch command: {e}"))?;
        #[cfg(windows)]
        if let Some(guard) = job.as_ref() {
            if let Err(error) = assign_to_job(guard.0, &child).and_then(|()| resume_child(&child)) {
                terminate(&mut child, job_handle(&job));
                return Err(error);
            }
        }
        let start = Instant::now();
        let (status, timed_out) = loop {
            if cancelled.is_some_and(|c| c.load(Ordering::SeqCst)) {
                terminate(&mut child, job_handle(&job));
                return Err("COMMAND_CANCELLED: terminated by operator cancellation".into());
            }
            let output_limit = [out_path.as_path(), err_path.as_path()]
                .iter()
                .filter_map(|p| fs::metadata(p).ok().map(|m| m.len()))
                .sum::<u64>();
            if output_limit > 4_000_000 {
                terminate(&mut child, job_handle(&job));
                break (None, true);
            }
            match child.try_wait().map_err(|e| e.to_string())? {
                Some(x) => break (Some(x), false),
                None if start.elapsed() >= Duration::from_secs(max_secs) => {
                    terminate(&mut child, job_handle(&job));
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
    if let Some(p) = in_path {
        let _ = fs::remove_file(p);
    }
    result
}

#[cfg(windows)]
type JobHandle = windows_sys::Win32::Foundation::HANDLE;

#[cfg(not(windows))]
type JobHandle = ();

#[cfg(windows)]
struct JobGuard(JobHandle);

#[cfg(windows)]
impl JobGuard {
    fn create() -> JResult<Self> {
        use windows_sys::Win32::System::JobObjects::{
            CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        };
        unsafe {
            let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if handle.is_null() {
                return Err("cannot create Windows Job Object".into());
            }
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let ok = SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const std::ffi::c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            if ok == 0 {
                windows_sys::Win32::Foundation::CloseHandle(handle);
                return Err("cannot configure Windows Job Object".into());
            }
            Ok(Self(handle))
        }
    }
}

#[cfg(windows)]
impl Drop for JobGuard {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

#[cfg(windows)]
fn job_handle(job: &Option<JobGuard>) -> Option<JobHandle> {
    job.as_ref().map(|guard| guard.0)
}

#[cfg(not(windows))]
fn job_handle(_job: &Option<()>) -> Option<()> {
    None
}

#[cfg(windows)]
fn assign_to_job(job: JobHandle, child: &Child) -> JResult<()> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;
    let ok = unsafe { AssignProcessToJobObject(job, child.as_raw_handle() as _) };
    if ok == 0 {
        return Err("cannot assign child process to the Job Object".into());
    }
    Ok(())
}

#[cfg(windows)]
fn resume_child(child: &Child) -> JResult<()> {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, INVALID_HANDLE_VALUE},
        System::{
            Diagnostics::ToolHelp::{
                CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD,
                THREADENTRY32,
            },
            Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME},
        },
    };
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return Err("cannot inspect suspended child threads".into());
        }
        let result = (|| {
            let mut entry: THREADENTRY32 = std::mem::zeroed();
            entry.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
            if Thread32First(snapshot, &mut entry) == 0 {
                return Err("cannot enumerate suspended child threads".into());
            }
            loop {
                if entry.th32OwnerProcessID == child.id() {
                    let thread = OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID);
                    if thread.is_null() {
                        return Err("cannot open suspended child thread".into());
                    }
                    let resumed = ResumeThread(thread);
                    CloseHandle(thread);
                    if resumed == u32::MAX {
                        return Err(
                            "cannot resume child process after Job Object assignment".into()
                        );
                    }
                    return Ok(());
                }
                if Thread32Next(snapshot, &mut entry) == 0 {
                    return Err("cannot find suspended child thread".into());
                }
            }
        })();
        CloseHandle(snapshot);
        result
    }
}

fn terminate(child: &mut Child, job: Option<JobHandle>) {
    #[cfg(windows)]
    if let Some(job) = job {
        unsafe {
            windows_sys::Win32::System::JobObjects::TerminateJobObject(job, 1);
        }
    }
    #[cfg(not(windows))]
    let _ = job;
    let _ = child.kill();
    let _ = child.wait();
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

    #[test]
    fn children_do_not_inherit_custom_provider_credentials() {
        #[cfg(windows)]
        let mut cmd = {
            let mut c = Command::new("cmd");
            c.args(["/C", "set JANI_CUSTOM_PROVIDER_KEY"]);
            c
        };
        #[cfg(unix)]
        let mut cmd = {
            let mut c = Command::new("sh");
            c.args(["-c", "printf %s \"$JANI_CUSTOM_PROVIDER_KEY\""]);
            c
        };
        cmd.env("JANI_CUSTOM_PROVIDER_KEY", "custom-secret-value");
        let out = execute_with_scrubbed(
            &mut cmd,
            Path::new("."),
            10,
            ProcessIsolation::Direct,
            &["JANI_CUSTOM_PROVIDER_KEY".to_string()],
        )
        .unwrap();
        assert!(
            !out.stdout.contains("custom-secret-value"),
            "child inherited custom API key"
        );
    }

    #[cfg(windows)]
    #[test]
    fn batch_shims_run_from_a_canonical_worktree() {
        let root = std::env::temp_dir()
            .join(format!("jani-shim-{}", unique_id()))
            .canonicalize()
            .unwrap_or_else(|_| {
                let root = std::env::temp_dir().join(format!("jani-shim-{}", unique_id()));
                std::fs::create_dir(&root).unwrap();
                root.canonicalize().unwrap()
            });
        std::fs::write(root.join("probe.cmd"), "@echo off\r\necho shim-ok\r\n").unwrap();
        let mut cmd = Command::new("cmd");
        cmd.args(["/C", "probe.cmd"]);
        let out = execute(&mut cmd, &root, 10).unwrap();
        assert!(out.stdout.contains("shim-ok"), "{}", out.stderr);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn job_object_bounds_and_kills_process_tree() {
        let mut cmd = Command::new("cmd");
        cmd.args(["/C", "ping -n 10 127.0.0.1 >NUL"]);
        let out = execute_with(&mut cmd, Path::new("."), 1, ProcessIsolation::JobObject).unwrap();
        assert!(out.timed_out);
    }
}
