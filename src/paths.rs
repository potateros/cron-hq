use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Paths {
    pub home: PathBuf,
    pub config: PathBuf,
    pub agents: PathBuf,
    pub migrated: PathBuf,
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from)
}

impl Paths {
    pub fn from_env() -> Paths {
        let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        Paths {
            home: env_path("HQ_HOME").unwrap_or_else(|| home_dir().join("Library/Application Support/hq")),
            config: env_path("HQ_CONFIG").unwrap_or_else(|| repo.join("jobs.toml")),
            agents: env_path("HQ_LAUNCH_AGENTS_DIR").unwrap_or_else(|| home_dir().join("Library/LaunchAgents")),
            migrated: env_path("HQ_MIGRATED_DIR").unwrap_or_else(|| repo.join("migrated")),
        }
    }

    /// Everything under `root`, for tests.
    pub fn temp(root: &Path) -> Paths {
        Paths {
            home: root.join("home"),
            config: root.join("jobs.toml"),
            agents: root.join("agents"),
            migrated: root.join("migrated"),
        }
    }

    pub fn db(&self) -> PathBuf {
        self.home.join("history.db")
    }
    pub fn state(&self) -> PathBuf {
        self.home.join("state.json")
    }
    pub fn log(&self, id: &str) -> PathBuf {
        self.home.join("logs").join(format!("{id}.log"))
    }
    pub fn lock(&self, id: &str) -> PathBuf {
        self.home.join("locks").join(format!("{id}.lock"))
    }
    pub fn launchd_log(&self) -> PathBuf {
        self.home.join("logs").join("_launchd.log")
    }
    pub fn launchctl_calls(&self) -> PathBuf {
        self.home.join("launchctl.calls")
    }
}

/// The message for a baked-in default config that no longer exists (hq was built from a repo that moved or
/// was deleted). None when HQ_CONFIG chose the path or the file is there.
pub fn missing_default_config(config: &Path, from_env: bool) -> Option<String> {
    if from_env || config.exists() {
        return None;
    }
    Some(format!(
        "config not found at {} (reinstall hq from its repo: cargo install --path . --root ~/.local --locked --force)",
        config.display()
    ))
}

pub fn expand_home(p: &str) -> PathBuf {
    if p == "~" {
        return home_dir();
    }
    match p.strip_prefix("~/") {
        Some(rest) => home_dir().join(rest),
        None => PathBuf::from(p),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    #[test]
    fn missing_default_config_message() {
        let gone = Path::new("/nonexistent/jobs.toml");
        assert_eq!(
            missing_default_config(gone, false).unwrap(),
            "config not found at /nonexistent/jobs.toml (reinstall hq from its repo: cargo install --path . --root ~/.local --locked --force)"
        );
        assert_eq!(missing_default_config(gone, true), None);
        assert_eq!(missing_default_config(Path::new("/"), false), None);
    }

    #[test]
    fn temp_layout() {
        let p = Paths::temp(Path::new("/r"));
        assert_eq!(p.home, PathBuf::from("/r/home"));
        assert_eq!(p.agents, PathBuf::from("/r/agents"));
        assert_eq!(p.migrated, PathBuf::from("/r/migrated"));
        assert_eq!(p.config, PathBuf::from("/r/jobs.toml"));
    }

    #[test]
    fn derived_paths() {
        let p = Paths::temp(Path::new("/r"));
        assert_eq!(p.db(), PathBuf::from("/r/home/history.db"));
        assert_eq!(p.state(), PathBuf::from("/r/home/state.json"));
        assert_eq!(p.log("j"), PathBuf::from("/r/home/logs/j.log"));
        assert_eq!(p.lock("j"), PathBuf::from("/r/home/locks/j.lock"));
        assert_eq!(p.launchd_log(), PathBuf::from("/r/home/logs/_launchd.log"));
        assert_eq!(p.launchctl_calls(), PathBuf::from("/r/home/launchctl.calls"));
    }

    #[test]
    fn expand_home_forms() {
        let home = std::env::var("HOME").unwrap();
        assert_eq!(expand_home("~"), PathBuf::from(&home));
        assert_eq!(expand_home("~/a/b"), PathBuf::from(format!("{home}/a/b")));
        assert_eq!(expand_home("/abs"), PathBuf::from("/abs"));
        assert_eq!(expand_home("~x"), PathBuf::from("~x"));
    }
}
