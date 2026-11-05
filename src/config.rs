use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Restart {
    Never,
    OnCrash,
    Always,
}

impl Restart {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "never" => Some(Restart::Never),
            "on-crash" => Some(Restart::OnCrash),
            "always" => Some(Restart::Always),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct AppConfig {
    pub name: String,
    pub command: String,
    pub dir: Option<PathBuf>,
    pub restart: Restart,
    pub start_hidden: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub restart_gap_ms: u64,
    pub restart_backoff_max_ms: u64,
    pub restart_max: u32,
    pub restart_reset_ms: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub apps: Vec<AppConfig>,
    pub settings: Settings,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    #[serde(default)]
    defaults: RawDefaults,
    #[serde(default)]
    app: toml::Table,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawDefaults {
    restart: Option<String>,
    start_hidden: Option<bool>,
    restart_gap_ms: Option<u64>,
    restart_backoff_max_ms: Option<u64>,
    restart_max: Option<u32>,
    restart_reset_ms: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawApp {
    command: String,
    dir: Option<String>,
    restart: Option<String>,
    start_hidden: Option<bool>,
}

pub fn default_path() -> PathBuf {
    match std::env::var_os("APPDATA") {
        Some(appdata) => PathBuf::from(appdata).join("tray-run").join("config.toml"),
        None => PathBuf::from("tray-run.toml"),
    }
}

fn exe_dir_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    Some(exe.parent()?.join("tray-run.toml"))
}

pub fn resolve_path(explicit: Option<PathBuf>) -> PathBuf {
    if let Some(path) = explicit {
        return path;
    }
    if let Some(path) = std::env::var_os("TRAY_RUN_CONFIG").map(PathBuf::from) {
        return path;
    }
    let default = default_path();
    if default.exists() {
        return default;
    }
    if let Some(local) = exe_dir_path() {
        if local.exists() {
            return local;
        }
    }
    default
}

pub fn load(path: &Path) -> Result<Config, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    from_str(&text)
}

pub fn from_str(text: &str) -> Result<Config, String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let raw: RawConfig = toml::from_str(text).map_err(|e| format!("config syntax: {e}"))?;
    let settings = Settings {
        restart_gap_ms: raw.defaults.restart_gap_ms.unwrap_or(2_000).max(1),
        restart_backoff_max_ms: raw.defaults.restart_backoff_max_ms.unwrap_or(60_000).max(1),
        restart_max: raw.defaults.restart_max.unwrap_or(5),
        restart_reset_ms: raw.defaults.restart_reset_ms.unwrap_or(60_000),
    };
    let default_restart = match raw.defaults.restart.as_deref() {
        Some(value) => Restart::parse(value).ok_or_else(|| {
            format!("defaults.restart: expected never, on-crash or always, got {value:?}")
        })?,
        None => Restart::OnCrash,
    };
    let default_hidden = raw.defaults.start_hidden.unwrap_or(true);

    let mut apps = Vec::with_capacity(raw.app.len());
    for (name, value) in raw.app {
        let app: RawApp = value.try_into().map_err(|e| format!("app {name}: {e}"))?;
        if app.command.trim().is_empty() {
            return Err(format!("app {name}: command is empty"));
        }
        let restart = match app.restart.as_deref() {
            Some(value) => Restart::parse(value)
                .ok_or_else(|| format!("app {name}: restart expected never, on-crash or always"))?,
            None => default_restart,
        };
        apps.push(AppConfig {
            name,
            command: app.command,
            dir: app.dir.map(PathBuf::from),
            restart,
            start_hidden: app.start_hidden.unwrap_or(default_hidden),
        });
    }
    if apps.is_empty() {
        return Err("config has no [app.*] sections".to_string());
    }
    Ok(Config { apps, settings })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_defaults_and_overrides() {
        let cfg = from_str(
            r#"
[defaults]
restart = "always"
restart_max = 2

[app.ping]
command = "ping -t 127.0.0.1"

[app.dev]
command = "npm run dev"
dir = "C:/proj"
restart = "never"
start_hidden = false
"#,
        )
        .unwrap();
        assert_eq!(cfg.apps.len(), 2);
        assert_eq!(cfg.apps[0].name, "ping");
        assert_eq!(cfg.apps[0].restart, Restart::Always);
        assert_eq!(cfg.apps[1].restart, Restart::Never);
        assert!(!cfg.apps[1].start_hidden);
        assert_eq!(cfg.apps[1].dir.as_deref(), Some(Path::new("C:/proj")));
        assert_eq!(cfg.settings.restart_max, 2);
        assert_eq!(cfg.settings.restart_gap_ms, 2000);
    }

    #[test]
    fn rejects_unknown_keys() {
        let err = from_str("[app.x]\ncommand = \"ping\"\nnme = \"oops\"\n").unwrap_err();
        assert!(err.contains("app x"), "{err}");
    }

    #[test]
    fn rejects_empty_config() {
        assert!(from_str("[defaults]\nrestart = \"never\"\n").is_err());
    }
}
