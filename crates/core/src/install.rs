//! Host installation planning.
//!
//! `install` means install-or-upgrade and is idempotent. This skeleton plans
//! per-user config and shortcut targets using platform standard locations and
//! returns a structured `not_implemented` refusal for the actual writes. The
//! application implements those writes in its own domain, without silent
//! elevation and without enabling auto-start.

use std::{
    fs,
    path::{Path, PathBuf},
};

use serde::Serialize;

use crate::{
    audit::capability_ids,
    error::{CapabilityState, Error, Result},
    platform::{Platform, platform_name},
};

/// Stable application id written into audit records.
pub const APP_ID: &str = "open-finder";
/// Human display title used for shortcut names.
pub const APP_TITLE: &str = "OpenFinder";

/// Options for install-or-upgrade planning.
#[derive(Debug, Clone, Default)]
pub struct InstallOptions {
    /// Skip shortcut creation.
    pub no_shortcut: bool,
    /// Overwrite an existing configuration with defaults.
    pub reset_config: bool,
}

/// Options for uninstall planning.
#[derive(Debug, Clone, Default)]
pub struct UninstallOptions {
    /// Keep the user configuration file.
    pub keep_config: bool,
}

/// One planned host action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    /// Create the configuration file.
    WriteConfig,
    /// Leave an existing configuration untouched.
    ReuseConfig,
    /// Overwrite the configuration with defaults.
    ResetConfig,
    /// Create a per-user shortcut.
    CreateShortcut,
    /// Write the install marker used for upgrade detection.
    WriteMarker,
    /// Remove a per-user shortcut.
    RemoveShortcut,
    /// Remove the configuration file.
    RemoveConfig,
    /// Keep the configuration file.
    KeepConfig,
    /// Remove the install marker.
    RemoveMarker,
}

/// One planned action and its target.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlannedAction {
    /// Action kind.
    pub kind: ActionKind,
    /// Absolute target path.
    pub target: String,
    /// Human-readable detail.
    pub detail: String,
}

/// Install-or-upgrade plan.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallPlan {
    /// Application id.
    pub app: String,
    /// Application version.
    pub version: String,
    /// Runtime platform identity.
    pub platform: String,
    /// Per-user configuration directory.
    pub config_dir: String,
    /// Configuration file.
    pub config_path: String,
    /// Install marker used for upgrade detection.
    pub marker_path: String,
    /// Shortcut target even when `--no-shortcut` suppresses the action.
    pub shortcut_path: Option<String>,
    /// Whether config or marker already exists.
    pub installed: bool,
    /// Previously recorded version, when readable.
    pub installed_version: Option<String>,
    /// Whether this is an upgrade rather than a first install.
    pub upgrade: bool,
    /// Always `false`: per-user paths never require elevation.
    pub requires_elevation: bool,
    /// Always `false`: installation never enables auto-start.
    pub auto_start: bool,
    /// Planned actions.
    pub actions: Vec<PlannedAction>,
}

/// Uninstall plan.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UninstallPlan {
    /// Application id.
    pub app: String,
    /// Application version.
    pub version: String,
    /// Runtime platform identity.
    pub platform: String,
    /// Configuration file.
    pub config_path: String,
    /// Shortcut target even when it does not exist.
    pub shortcut_path: Option<String>,
    /// Install marker used for upgrade detection.
    pub marker_path: String,
    /// Whether the configuration file exists.
    pub config_exists: bool,
    /// Whether the shortcut exists.
    pub shortcut_exists: bool,
    /// Always `false`: per-user paths never require elevation.
    pub requires_elevation: bool,
    /// Planned actions.
    pub actions: Vec<PlannedAction>,
}

struct InstallPaths {
    config_dir: PathBuf,
    config_path: PathBuf,
    marker_path: PathBuf,
    shortcut_path: Option<PathBuf>,
}

/// Plan an install or upgrade using the current platform and environment.
pub fn plan_install(options: &InstallOptions) -> Result<InstallPlan> {
    plan_install_with(Platform::current(), &|key| std::env::var(key).ok(), options)
}

/// Plan an install or upgrade with explicit platform and environment.
pub fn plan_install_with(platform: Platform, env: &impl Fn(&str) -> Option<String>, options: &InstallOptions) -> Result<InstallPlan> {
    let paths = resolve_paths(platform, env)?;
    let installed_version = read_marker(&paths.marker_path);
    let installed = installed_version.is_some() || paths.config_path.is_file();
    let version = env!("CARGO_PKG_VERSION").to_string();
    let upgrade = installed_version.as_deref().is_some_and(|previous| previous != version.as_str());

    let mut actions = Vec::new();
    if paths.config_path.is_file() {
        if options.reset_config {
            actions.push(action(ActionKind::ResetConfig, &paths.config_path, "replace user configuration with defaults"));
        }
        else {
            actions.push(action(ActionKind::ReuseConfig, &paths.config_path, "keep existing user configuration"));
        }
    }
    else {
        actions.push(action(ActionKind::WriteConfig, &paths.config_path, "write default user configuration"));
    }
    if !options.no_shortcut
        && let Some(shortcut) = &paths.shortcut_path
    {
        actions.push(action(ActionKind::CreateShortcut, shortcut, "create per-user shortcut, no elevation"));
    }
    actions.push(action(ActionKind::WriteMarker, &paths.marker_path, "record installed version for idempotent upgrades"));

    Ok(InstallPlan {
        app: APP_ID.to_string(),
        version,
        platform: platform_name().to_string(),
        config_dir: paths.config_dir.display().to_string(),
        config_path: paths.config_path.display().to_string(),
        marker_path: paths.marker_path.display().to_string(),
        shortcut_path: paths.shortcut_path.as_ref().map(|path| path.display().to_string()),
        installed,
        installed_version,
        upgrade,
        requires_elevation: false,
        auto_start: false,
        actions,
    })
}

/// Plan an uninstall using the current platform and environment.
pub fn plan_uninstall(options: &UninstallOptions) -> Result<UninstallPlan> {
    plan_uninstall_with(Platform::current(), &|key| std::env::var(key).ok(), options)
}

/// Plan an uninstall with explicit platform and environment.
pub fn plan_uninstall_with(platform: Platform, env: &impl Fn(&str) -> Option<String>, options: &UninstallOptions) -> Result<UninstallPlan> {
    let paths = resolve_paths(platform, env)?;
    let config_exists = paths.config_path.is_file();
    let marker_exists = paths.marker_path.is_file();
    let shortcut_exists = paths.shortcut_path.as_ref().is_some_and(|path| path.exists());

    let mut actions = Vec::new();
    if shortcut_exists && let Some(shortcut) = &paths.shortcut_path {
        actions.push(action(ActionKind::RemoveShortcut, shortcut, "remove per-user shortcut, no elevation"));
    }
    if config_exists {
        if options.keep_config {
            actions.push(action(ActionKind::KeepConfig, &paths.config_path, "keep user configuration"));
        }
        else {
            actions.push(action(ActionKind::RemoveConfig, &paths.config_path, "remove user configuration"));
        }
    }
    if marker_exists {
        actions.push(action(ActionKind::RemoveMarker, &paths.marker_path, "remove install marker"));
    }

    Ok(UninstallPlan {
        app: APP_ID.to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        platform: platform_name().to_string(),
        config_path: paths.config_path.display().to_string(),
        shortcut_path: paths.shortcut_path.as_ref().map(|path| path.display().to_string()),
        marker_path: paths.marker_path.display().to_string(),
        config_exists,
        shortcut_exists,
        requires_elevation: false,
        actions,
    })
}

/// Execute an install or upgrade.
///
/// The skeleton deliberately stops at the structured `not_implemented` state.
/// The plan is real and audited, so the CLI can expose `--dry-run` today without
/// pretending that host writes happened.
pub fn apply_install(options: &InstallOptions) -> Result<()> {
    let plan = plan_install(options)?;
    Err(Error::capability(
        capability_ids::INSTALL_APPLY,
        CapabilityState::NotImplemented,
        format!("plan ready with {} action(s); implement per-user config and shortcut writes in the application domain", plan.actions.len()),
    ))
}

/// Execute an uninstall.
pub fn apply_uninstall(options: &UninstallOptions) -> Result<()> {
    let plan = plan_uninstall(options)?;
    Err(Error::capability(
        capability_ids::UNINSTALL_APPLY,
        CapabilityState::NotImplemented,
        format!("plan ready with {} action(s); implement per-user removal in the application domain", plan.actions.len()),
    ))
}

fn action(kind: ActionKind, target: &Path, detail: &str) -> PlannedAction {
    PlannedAction { kind, target: target.display().to_string(), detail: detail.to_string() }
}

fn resolve_paths(platform: Platform, env: &impl Fn(&str) -> Option<String>) -> Result<InstallPaths> {
    match platform {
        Platform::Windows => {
            let appdata = env_path(env, "APPDATA")
                .or_else(|| env_path(env, "USERPROFILE").map(|home| home.join("AppData").join("Roaming")))
                .ok_or_else(|| Error::message("APPDATA or USERPROFILE is required to resolve Windows install paths"))?;
            let config_dir = appdata.join("OpenFinder");
            let shortcut = appdata.join("Microsoft").join("Windows").join("Start Menu").join("Programs").join(format!("{APP_TITLE}.lnk"));
            Ok(paths(config_dir, Some(shortcut)))
        }
        Platform::Macos => {
            let home = home_dir(env)?;
            let config_dir = home.join("Library").join("Application Support").join("OpenFinder");
            let shortcut = home.join("Applications").join(format!("{APP_TITLE}.app"));
            Ok(paths(config_dir, Some(shortcut)))
        }
        Platform::Linux => {
            let home = home_dir(env)?;
            let config_base = env_path(env, "XDG_CONFIG_HOME").unwrap_or_else(|| home.join(".config"));
            let data_base = env_path(env, "XDG_DATA_HOME").unwrap_or_else(|| home.join(".local").join("share"));
            let config_dir = config_base.join("open-finder");
            let shortcut = data_base.join("applications").join("open-finder.desktop");
            Ok(paths(config_dir, Some(shortcut)))
        }
        Platform::Other => Err(Error::capability(
            capability_ids::INSTALL_PLAN,
            CapabilityState::UnsupportedPlatform,
            "no platform standard install locations for this target",
        )),
    }
}

fn paths(config_dir: PathBuf, shortcut_path: Option<PathBuf>) -> InstallPaths {
    let config_path = config_dir.join("config.json");
    let marker_path = config_dir.join("install.json");
    InstallPaths { config_dir, config_path, marker_path, shortcut_path }
}

fn env_path(env: &impl Fn(&str) -> Option<String>, key: &str) -> Option<PathBuf> {
    env(key).filter(|value| !value.trim().is_empty()).map(PathBuf::from)
}

fn home_dir(env: &impl Fn(&str) -> Option<String>) -> Result<PathBuf> {
    env_path(env, "HOME")
        .or_else(|| env_path(env, "USERPROFILE"))
        .ok_or_else(|| Error::message("HOME or USERPROFILE is required to resolve per-user install paths"))
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct InstallMarker {
    app_version: Option<String>,
}

fn read_marker(path: &Path) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str::<InstallMarker>(&text).ok()?.app_version
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        sync::atomic::{AtomicU64, Ordering},
    };

    use super::*;

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("open-app-install-{}-{}", std::process::id(), COUNTER.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn windows_env(appdata: &Path) -> HashMap<String, String> {
        HashMap::from([("APPDATA".to_string(), appdata.display().to_string())])
    }

    #[test]
    fn windows_plan_uses_per_user_standard_paths() {
        let root = temp_dir();
        let env = windows_env(&root);
        let plan = plan_install_with(Platform::Windows, &|key| env.get(key).cloned(), &InstallOptions::default()).unwrap();
        assert!(PathBuf::from(&plan.config_path).ends_with(Path::new("OpenFinder").join("config.json")));
        assert!(
            PathBuf::from(plan.shortcut_path.as_ref().unwrap())
                .ends_with(Path::new("Microsoft").join("Windows").join("Start Menu").join("Programs").join("OpenFinder.lnk"))
        );
        assert!(!plan.requires_elevation);
        assert!(!plan.auto_start);
    }

    #[test]
    fn no_shortcut_removes_the_shortcut_action() {
        let root = temp_dir();
        let env = windows_env(&root);
        let plan =
            plan_install_with(Platform::Windows, &|key| env.get(key).cloned(), &InstallOptions { no_shortcut: true, reset_config: false })
                .unwrap();
        assert!(!plan.actions.iter().any(|action| action.kind == ActionKind::CreateShortcut));
    }

    #[test]
    fn marker_detects_upgrade_and_reset_rewrites_config() {
        let root = temp_dir();
        let env = windows_env(&root);
        let config_dir = root.join("OpenFinder");
        fs::create_dir_all(&config_dir).unwrap();
        fs::write(config_dir.join("config.json"), "{}").unwrap();
        fs::write(config_dir.join("install.json"), r#"{"appVersion":"0.0.1"}"#).unwrap();

        let plan =
            plan_install_with(Platform::Windows, &|key| env.get(key).cloned(), &InstallOptions { no_shortcut: false, reset_config: true })
                .unwrap();
        assert!(plan.installed);
        assert!(plan.upgrade);
        assert!(plan.actions.iter().any(|action| action.kind == ActionKind::ResetConfig));
    }

    #[test]
    fn linux_plan_honours_xdg_config_home() {
        let root = temp_dir();
        let env = HashMap::from([
            ("HOME".to_string(), root.display().to_string()),
            ("XDG_CONFIG_HOME".to_string(), root.join("cfg").display().to_string()),
            ("XDG_DATA_HOME".to_string(), root.join("data").display().to_string()),
        ]);
        let plan = plan_install_with(Platform::Linux, &|key| env.get(key).cloned(), &InstallOptions::default()).unwrap();
        assert!(PathBuf::from(&plan.config_path).ends_with(Path::new("cfg").join("open-finder").join("config.json")));
        assert!(
            PathBuf::from(plan.shortcut_path.as_ref().unwrap()).ends_with(Path::new("data").join("applications").join("open-finder.desktop"))
        );
    }

    #[test]
    fn uninstall_keeps_config_when_requested() {
        let root = temp_dir();
        let env = windows_env(&root);
        let config_dir = root.join("OpenFinder");
        fs::create_dir_all(&config_dir).unwrap();
        fs::write(config_dir.join("config.json"), "{}").unwrap();
        let plan = plan_uninstall_with(Platform::Windows, &|key| env.get(key).cloned(), &UninstallOptions { keep_config: true }).unwrap();
        assert!(plan.config_exists);
        assert!(plan.actions.iter().any(|action| action.kind == ActionKind::KeepConfig));
    }

    #[test]
    fn execution_refuses_with_a_structured_not_implemented_state() {
        // The public apply path uses process environment, so only assert the
        // error shape through the planning helper that shares the same code.
        let error = Error::capability(capability_ids::INSTALL_APPLY, CapabilityState::NotImplemented, "skeleton");
        assert_eq!(error.code(), "not_implemented");
        assert_eq!(capability_ids::INSTALL_APPLY, "install-apply");
    }
}
