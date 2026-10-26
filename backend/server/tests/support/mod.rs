use std::path::PathBuf;
use std::process::Command;

// Integration tests must never read the developer's real user-wide settings.
pub fn hermetic_user_settings() -> PathBuf {
    std::env::temp_dir().join(format!(
        "jani-test-no-user-settings-{}.json",
        std::process::id()
    ))
}

pub fn jani() -> Command {
    jani_with_user_settings(hermetic_user_settings())
}

pub fn jani_with_user_settings(path: PathBuf) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_jani"));
    command.env("JANI_USER_SETTINGS", path);
    command
}
