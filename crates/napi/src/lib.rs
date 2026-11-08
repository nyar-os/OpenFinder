//! Platform-neutral N-API surface for OpenFinder.
//!
//! This crate forwards generic capabilities to the `wae` facade and contains no
//! platform `cfg` branches and no platform string literals. Missing host
//! capabilities arrive as structured upstream errors. Audit is wrapped at every
//! capability boundary through the shared `wae.audit/v0` facility.

#![deny(missing_docs)]

use std::{cell::RefCell, env, path::PathBuf};

use open_finder_core::{
    AuditFields, BuildOptions, CapabilityState, Error as CoreError, InstallOptions, QuerySpec, SortKey, UninstallOptions, WalkOptions,
    apply_install, apply_uninstall, build_index, capabilities as core_capabilities, capability_ids, config_dir, default_index_dir,
    inspect_volume, load_index, plan_install, plan_uninstall, platform_name, search, update_index, walk,
};
use serde_json::{Value, json};
use wae::{
    CallContext, Error, JsValue, Result, function, optional_string_prop, register_module,
    webview::{IpcHandler, IpcOutcome, OpenOptions, WebViewError, open},
};

mod audit;

use audit::Summary;
use wae::audit::Mode;

const APP_TITLE: &str = "OpenFinder";
const GUI_DIR_ENV: &str = "OPEN_FINDER_GUI";
/// GUI 宿主诊断日志：宿主会隐藏系统为它新建的控制台窗口，之后 stderr 不可见。
const GUI_LOG_ENV: &str = "WAE_WEBVIEW_LOG_FILE";

thread_local! {
    static GUI: RefCell<Option<wae::webview::WebViewHost>> = const { RefCell::new(None) };
}

struct HostIpc;

impl IpcHandler for HostIpc {
    fn on_ipc(&mut self, message: &str) -> IpcOutcome {
        let Ok(request) = serde_json::from_str::<Value>(message)
        else {
            return IpcOutcome::ReplyToPage(r#"{"ok":false,"error":"invalid json"}"#.to_string());
        };
        let kind = request.get("type").and_then(Value::as_str).unwrap_or_default();
        match kind {
            "close" => IpcOutcome::CloseWindow,
            "capabilities" => match audit::run(capability_ids::CAPABILITIES, Mode::Execute, Value::Null, || {
                Ok((core_capabilities(), Summary::new(1, "capabilities")))
            }) {
                Ok(report) => IpcOutcome::ReplyToPage(reply(json!({ "ok": true, "data": report }))),
                Err(error) => IpcOutcome::ReplyToPage(reply(json!({ "ok": false, "error": error.payload() }))),
            },
            "search" => {
                let index_dir = PathBuf::from(request.get("indexDir").and_then(Value::as_str).unwrap_or_default());
                let Ok(spec) = serde_json::from_value::<QuerySpec>(request.clone())
                else {
                    return IpcOutcome::ReplyToPage(reply(json!({ "ok": false, "error": "invalid query spec" })));
                };
                let index_dir =
                    if index_dir.as_os_str().is_empty() { default_index_dir().unwrap_or_else(|_| PathBuf::from(".")) } else { index_dir };
                match audit::run(capability_ids::SEARCH, Mode::Execute, Value::Null, move || {
                    let index = load_index(&index_dir)?;
                    let page = search(&index, &spec)?;
                    let fields = AuditFields::for_search(&page);
                    Ok((page, Summary::new(fields.target_count, fields.target_summary)))
                }) {
                    Ok(page) => IpcOutcome::ReplyToPage(reply(json!({ "ok": true, "data": page }))),
                    Err(error) => IpcOutcome::ReplyToPage(reply(json!({ "ok": false, "error": error.payload() }))),
                }
            }
            "searchWalk" => {
                let root = request.get("root").and_then(Value::as_str).unwrap_or_default().to_string();
                if root.is_empty() {
                    return IpcOutcome::ReplyToPage(reply(json!({ "ok": false, "error": "root is required" })));
                }
                let Ok(spec) = serde_json::from_value::<QuerySpec>(request.clone())
                else {
                    return IpcOutcome::ReplyToPage(reply(json!({ "ok": false, "error": "invalid query spec" })));
                };
                let options = WalkOptions {
                    root: PathBuf::from(root),
                    max_depth: request.get("maxDepth").and_then(Value::as_u64).map_or(8, |value| value as u32),
                    max_entries: request.get("maxEntries").and_then(Value::as_u64).unwrap_or(100_000),
                    spec,
                };
                match audit::run(capability_ids::PORTABLE_WALK, Mode::Execute, Value::Null, move || {
                    let page = walk(&options)?;
                    let fields = AuditFields::for_walk(&page);
                    Ok((page, Summary::new(fields.target_count, fields.target_summary)))
                }) {
                    Ok(page) => IpcOutcome::ReplyToPage(reply(json!({ "ok": true, "data": page }))),
                    Err(error) => IpcOutcome::ReplyToPage(reply(json!({ "ok": false, "error": error.payload() }))),
                }
            }
            "installPlan" => {
                let options = InstallOptions {
                    no_shortcut: request.get("noShortcut").and_then(Value::as_bool).unwrap_or(false),
                    reset_config: request.get("resetConfig").and_then(Value::as_bool).unwrap_or(false),
                };
                let params = json!({ "noShortcut": options.no_shortcut, "resetConfig": options.reset_config });
                match audit::run(capability_ids::INSTALL_PLAN, Mode::DryRun, params, move || {
                    let plan = plan_install(&options)?;
                    let fields = AuditFields::for_install_plan(&plan);
                    Ok((plan, Summary::new(fields.target_count, fields.target_summary)))
                }) {
                    Ok(plan) => IpcOutcome::ReplyToPage(reply(json!({ "ok": true, "data": plan }))),
                    Err(error) => IpcOutcome::ReplyToPage(reply(json!({ "ok": false, "error": error.payload() }))),
                }
            }
            "uninstallPlan" => {
                let options = UninstallOptions { keep_config: request.get("keepConfig").and_then(Value::as_bool).unwrap_or(true) };
                let params = json!({ "keepConfig": options.keep_config });
                match audit::run(capability_ids::UNINSTALL_PLAN, Mode::DryRun, params, move || {
                    let plan = plan_uninstall(&options)?;
                    let fields = AuditFields::for_uninstall_plan(&plan);
                    Ok((plan, Summary::new(fields.target_count, fields.target_summary)))
                }) {
                    Ok(plan) => IpcOutcome::ReplyToPage(reply(json!({ "ok": true, "data": plan }))),
                    Err(error) => IpcOutcome::ReplyToPage(reply(json!({ "ok": false, "error": error.payload() }))),
                }
            }
            other => IpcOutcome::ReplyToPage(reply(json!({ "ok": false, "error": format!("unknown message: {other}") }))),
        }
    }
}

fn reply(value: Value) -> String {
    serde_json::to_string(&value).unwrap_or_else(|_| r#"{"ok":false,"error":"serialization failed"}"#.to_string())
}

fn core_error(error: CoreError) -> Error {
    let payload = error.payload();
    match serde_json::to_string(&payload) {
        Ok(json) => Error::new(format!("wae.error:{json}")),
        Err(_) => Error::new("wae.error:{\"code\":\"message\",\"message\":\"error payload serialization failed\"}"),
    }
}

fn webview_error(error: WebViewError) -> CoreError {
    match error {
        WebViewError::Unsupported { capability, .. } => CoreError::capability(
            capability_ids::GUI,
            CapabilityState::UnsupportedPlatform,
            format!("upstream webview capability {capability:?} is unavailable on this target"),
        ),
        other => CoreError::message(other.to_string()),
    }
}

fn json_to_js(env: wae::Env, value: Value) -> Result<JsValue> {
    match value {
        Value::Null => env.null(),
        Value::Bool(value) => env.boolean(value),
        Value::Number(number) => match number.as_i64() {
            Some(value) => env.int64(value),
            None => env.double(number.as_f64().unwrap_or(0.0)),
        },
        Value::String(text) => env.string(&text),
        Value::Array(items) => {
            let array = env.array_with_length(items.len())?;
            for (index, item) in items.into_iter().enumerate() {
                array.set_element(index as u32, json_to_js(env, item)?)?;
            }
            Ok(array)
        }
        Value::Object(map) => {
            let object = env.object()?;
            for (key, item) in map {
                object.set_named(&key, json_to_js(env, item)?)?;
            }
            Ok(object)
        }
    }
}

fn value_to_js<T: serde::Serialize>(ctx: &CallContext, value: &T) -> Result<JsValue> {
    let json = serde_json::to_value(value).map_err(|error| Error::new(format!("serialize failed: {error}")))?;
    json_to_js(ctx.env(), json)
}

fn object_arg(ctx: &CallContext) -> Result<Option<JsValue>> {
    let Some(arg) = ctx.args().first().copied()
    else {
        return Ok(None);
    };
    if arg.is_undefined()? {
        return Ok(None);
    }
    Ok(Some(arg))
}

fn optional_bool(obj: Option<JsValue>, name: &str) -> Result<bool> {
    let Some(obj) = obj
    else {
        return Ok(false);
    };
    let value = obj.get_named(name)?;
    if value.is_undefined()? {
        return Ok(false);
    }
    value.as_bool()
}

fn optional_i64(obj: JsValue, name: &str) -> Result<Option<i64>> {
    let value = obj.get_named(name)?;
    if value.is_undefined()? {
        return Ok(None);
    }
    // Keep the option name in the error instead of leaking a raw N-API status.
    match value.as_i64() {
        Ok(number) => Ok(Some(number)),
        Err(_) => Err(invalid_argument(format!("`{name}` must be an integer"))),
    }
}

fn invalid_argument(message: String) -> Error {
    let payload = json!({ "code": "invalid_argument", "platform": platform_name(), "message": message });
    Error::new(format!("wae.error:{payload}"))
}

fn optional_u64(obj: Option<JsValue>, name: &str) -> Result<Option<u64>> {
    let Some(obj) = obj
    else {
        return Ok(None);
    };
    let Some(value) = optional_i64(obj, name)?
    else {
        return Ok(None);
    };
    // A wrong type or a negative number must not collapse into the default:
    // that would silently widen a query or shrink a build budget.
    u64::try_from(value).map(Some).map_err(|_| invalid_argument(format!("`{name}` must be a non-negative integer, got {value}")))
}

fn optional_usize(obj: Option<JsValue>, name: &str) -> Result<Option<usize>> {
    let Some(obj) = obj
    else {
        return Ok(None);
    };
    let Some(value) = optional_i64(obj, name)?
    else {
        return Ok(None);
    };
    usize::try_from(value).map(Some).map_err(|_| invalid_argument(format!("`{name}` must be a non-negative integer, got {value}")))
}

fn optional_string(obj: Option<JsValue>, name: &str) -> Result<Option<String>> {
    Ok(obj.and_then(|obj| optional_string_prop(obj, name).ok().flatten()))
}

fn query_spec(arg: Option<JsValue>) -> Result<QuerySpec> {
    let mut spec = QuerySpec::default();
    let Some(obj) = arg
    else {
        return Ok(spec);
    };
    spec.pattern = optional_string(Some(obj), "pattern")?;
    spec.regex = optional_bool(Some(obj), "regex")?;
    spec.match_path = optional_bool(Some(obj), "matchPath")?;
    spec.path_filter = optional_string(Some(obj), "pathFilter")?;
    spec.min_size = optional_u64(Some(obj), "minSize")?;
    spec.max_size = optional_u64(Some(obj), "maxSize")?;
    spec.modified_after_ms = optional_u64(Some(obj), "modifiedAfterMs")?;
    spec.modified_before_ms = optional_u64(Some(obj), "modifiedBeforeMs")?;
    spec.kind = match optional_string(Some(obj), "kind")?.as_deref() {
        Some("file") => Some(open_finder_core::EntryKind::File),
        Some("dir") => Some(open_finder_core::EntryKind::Directory),
        _ => None,
    };
    spec.sort = match optional_string(Some(obj), "sort")?.as_deref() {
        Some("path") => SortKey::Path,
        Some("size") => SortKey::Size,
        Some("modified") => SortKey::Modified,
        _ => SortKey::Name,
    };
    spec.descending = optional_bool(Some(obj), "descending")?;
    if let Some(limit) = optional_usize(Some(obj), "limit")? {
        spec.limit = limit;
    }
    if let Some(offset) = optional_usize(Some(obj), "offset")? {
        spec.offset = offset;
    }
    Ok(spec)
}

fn resolve_index_dir(arg: Option<JsValue>) -> Result<PathBuf> {
    if let Some(dir) = optional_string(arg, "indexDir")?.filter(|dir| !dir.trim().is_empty()) {
        return Ok(PathBuf::from(dir));
    }
    default_index_dir().map_err(core_error)
}

fn volume_arg(arg: Option<JsValue>) -> CoreError {
    let detail = match arg.and_then(|obj| optional_string_prop(obj, "volume").ok().flatten()) {
        Some(volume) if !volume.trim().is_empty() => format!("volume {volume} is required"),
        _ => "volume is required, for example `C:`".to_string(),
    };
    CoreError::message(detail)
}

fn require_volume(arg: Option<JsValue>) -> Result<String> {
    optional_string(arg, "volume")?.filter(|volume| !volume.trim().is_empty()).ok_or_else(|| core_error(volume_arg(arg)))
}

fn install_options(ctx: &CallContext) -> Result<InstallOptions> {
    let arg = object_arg(ctx)?;
    Ok(InstallOptions { no_shortcut: optional_bool(arg, "noShortcut")?, reset_config: optional_bool(arg, "resetConfig")? })
}

fn uninstall_options(ctx: &CallContext) -> Result<UninstallOptions> {
    let arg = object_arg(ctx)?;
    Ok(UninstallOptions { keep_config: optional_bool(arg, "keepConfig")? })
}

fn resolve_gui_dir(requested: Option<String>) -> Result<String> {
    if let Some(dir) = requested
        && !dir.trim().is_empty()
    {
        return Ok(dir);
    }
    if let Ok(dir) = env::var(GUI_DIR_ENV)
        && !dir.trim().is_empty()
    {
        return Ok(dir);
    }
    let candidates = [PathBuf::from("packages/open-finder-ui/dist/gui"), PathBuf::from("dist/gui"), PathBuf::from("gui")];
    for candidate in candidates {
        if candidate.join("index.html").is_file() {
            return Ok(candidate.display().to_string());
        }
    }
    Err(Error::new("gui content dir not found: pass `contentDir` or build `packages/open-finder-ui/dist/gui`"))
}

/// Resolve the GUI diagnostics log and expose it to the host.
///
/// `node.exe` 是 CUI 子系统程序：没有控制台的启动器会为它新建一个可见的黑色
/// 控制台窗口，宿主在创建产品窗口时把它隐藏。隐藏之后 stderr 不再有人看得见，
/// 所以宿主诊断必须落盘。显式设置的环境变量优先，便于排障时改路径。
fn ensure_gui_log() -> Option<PathBuf> {
    if let Some(existing) = env::var_os(GUI_LOG_ENV) {
        return Some(PathBuf::from(existing));
    }
    let dir = config_dir().join("logs");
    if std::fs::create_dir_all(&dir).is_err() {
        return None;
    }
    let path = dir.join("gui.log");
    // SAFETY: N-API 调用是进程内的单线程执行，宿主只在此进程中读取该变量。
    unsafe { env::set_var(GUI_LOG_ENV, &path) };
    eprintln!("[open-finder] gui.log path={}", path.display());
    Some(path)
}

fn actor_kind(value: Option<&str>) -> wae::audit::ActorKind {
    match value {
        Some(value) if value.eq_ignore_ascii_case("agent") => wae::audit::ActorKind::Agent,
        _ => wae::audit::ActorKind::User,
    }
}

fn query_from_arg(arg: Option<JsValue>) -> Result<wae::audit::Query> {
    let mut query = wae::audit::Query::default();
    let Some(arg) = arg
    else {
        return Ok(query);
    };
    query.actor_kind = optional_string_prop(arg, "kind")?.map(|value| actor_kind(Some(value.as_str())));
    query.actor_id = optional_string_prop(arg, "id")?;
    query.capability = optional_string_prop(arg, "capability")?;
    query.from_ms = optional_i64(arg, "fromMs")?.map(|value| value.max(0) as u64);
    query.to_ms = optional_i64(arg, "toMs")?.map(|value| value.max(0) as u64);
    query.limit = optional_i64(arg, "limit")?.map(|value| value.max(0) as usize);
    Ok(query)
}

#[function]
fn version(ctx: CallContext) -> Result<JsValue> {
    let result = audit::run(capability_ids::VERSION, Mode::DryRun, Value::Null, || {
        Ok((env!("CARGO_PKG_VERSION").to_string(), Summary::new(0, "version")))
    });
    match result {
        Ok(version) => ctx.env().string(&version),
        Err(error) => Err(core_error(error)),
    }
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct BuildInfo {
    version: &'static str,
    commit: &'static str,
    built_at_unix: u64,
    target: String,
}

#[function(js_name = "buildInfo")]
fn build_info(ctx: CallContext) -> Result<JsValue> {
    let result = audit::run(capability_ids::VERSION, Mode::DryRun, Value::Null, || {
        let info = BuildInfo {
            version: env!("CARGO_PKG_VERSION"),
            commit: option_env!("WAE_APP_BUILD_COMMIT").unwrap_or("unknown"),
            built_at_unix: option_env!("WAE_APP_BUILD_EPOCH").and_then(|epoch| epoch.parse().ok()).unwrap_or(0),
            target: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
        };
        Ok((info, Summary::new(1, "build info")))
    });
    match result {
        Ok(info) => value_to_js(&ctx, &info),
        Err(error) => Err(core_error(error)),
    }
}

#[function]
fn capabilities(ctx: CallContext) -> Result<JsValue> {
    let result = audit::run(capability_ids::CAPABILITIES, Mode::DryRun, Value::Null, || {
        let report = core_capabilities();
        let summary = Summary::new(report.items.len() as u64, "domain capabilities");
        Ok((report, summary))
    });
    match result {
        Ok(report) => value_to_js(&ctx, &report),
        Err(error) => Err(core_error(error)),
    }
}

#[function(js_name = "defaultIndexDir")]
fn default_index_dir_fn(ctx: CallContext) -> Result<JsValue> {
    let result = audit::run(capability_ids::CAPABILITIES, Mode::DryRun, Value::Null, || {
        let dir = default_index_dir().map_err(|error| CoreError::message(error.to_string()))?;
        let summary = Summary::new(1, "default index directory");
        Ok((dir.display().to_string(), summary))
    });
    match result {
        Ok(dir) => ctx.env().string(&dir),
        Err(error) => Err(core_error(error)),
    }
}

#[function(js_name = "inspectVolume")]
fn inspect_volume_fn(ctx: CallContext) -> Result<JsValue> {
    let arg = object_arg(&ctx)?;
    let volume = require_volume(arg)?;
    let params = json!({ "volume": &volume });
    let result = audit::run(capability_ids::INDEX_BUILD, Mode::DryRun, params, move || {
        let report = inspect_volume(&volume)?;
        let fields = AuditFields::for_volume(&report);
        Ok((report, Summary::new(fields.target_count, fields.target_summary)))
    });
    match result {
        Ok(report) => value_to_js(&ctx, &report),
        Err(error) => Err(core_error(error)),
    }
}

#[function(js_name = "indexBuild")]
fn index_build(ctx: CallContext) -> Result<JsValue> {
    let arg = object_arg(&ctx)?;
    let volume = require_volume(arg)?;
    let index_dir = resolve_index_dir(arg)?;
    let options = BuildOptions { volume, index_dir, record_limit: optional_u64(arg, "recordLimit")? };
    let params = json!({ "volume": &options.volume, "indexDir": options.index_dir.display().to_string(), "recordLimit": options.record_limit });
    let result = audit::run(capability_ids::INDEX_BUILD, Mode::Execute, params, move || {
        let report = build_index(&options)?;
        let fields = AuditFields::for_build(&report);
        Ok((report, Summary::new(fields.target_count, fields.target_summary)))
    });
    match result {
        Ok(report) => value_to_js(&ctx, &report),
        Err(error) => Err(core_error(error)),
    }
}

#[function(js_name = "indexUpdate")]
fn index_update(ctx: CallContext) -> Result<JsValue> {
    let arg = object_arg(&ctx)?;
    let index_dir = resolve_index_dir(arg)?;
    let max_records = optional_u64(arg, "maxRecords")?.unwrap_or(200_000);
    let params = json!({ "indexDir": index_dir.display().to_string(), "maxRecords": max_records });
    let result = audit::run(capability_ids::INDEX_UPDATE, Mode::Execute, params, move || {
        let report = update_index(&index_dir, max_records)?;
        let fields = AuditFields::for_update(&report);
        Ok((report, Summary::new(fields.target_count, fields.target_summary)))
    });
    match result {
        Ok(report) => value_to_js(&ctx, &report),
        Err(error) => Err(core_error(error)),
    }
}

#[function(js_name = "search")]
fn search_fn(ctx: CallContext) -> Result<JsValue> {
    let arg = object_arg(&ctx)?;
    let index_dir = resolve_index_dir(arg)?;
    let spec = query_spec(arg)?;
    let params = json!({ "indexDir": index_dir.display().to_string(), "pattern": &spec.pattern, "limit": spec.limit, "offset": spec.offset });
    let result = audit::run(capability_ids::SEARCH, Mode::Execute, params, move || {
        let index = load_index(&index_dir)?;
        let page = search(&index, &spec)?;
        let fields = AuditFields::for_search(&page);
        Ok((page, Summary::new(fields.target_count, fields.target_summary)))
    });
    match result {
        Ok(page) => value_to_js(&ctx, &page),
        Err(error) => Err(core_error(error)),
    }
}

#[function(js_name = "searchWalk")]
fn search_walk(ctx: CallContext) -> Result<JsValue> {
    let arg = object_arg(&ctx)?;
    let Some(root) = optional_string(arg, "root")?.filter(|root| !root.trim().is_empty())
    else {
        return Err(Error::new("root is required for a portable walk"));
    };
    let options = WalkOptions {
        root: PathBuf::from(root),
        max_depth: optional_usize(arg, "maxDepth")?.map_or(8, |value| value as u32),
        max_entries: optional_u64(arg, "maxEntries")?.unwrap_or(100_000),
        spec: query_spec(arg)?,
    };
    let params = json!({ "root": options.root.display().to_string(), "maxDepth": options.max_depth, "maxEntries": options.max_entries });
    let result = audit::run(capability_ids::PORTABLE_WALK, Mode::Execute, params, move || {
        let page = walk(&options)?;
        let fields = AuditFields::for_walk(&page);
        Ok((page, Summary::new(fields.target_count, fields.target_summary)))
    });
    match result {
        Ok(page) => value_to_js(&ctx, &page),
        Err(error) => Err(core_error(error)),
    }
}

#[function(js_name = "platform")]
fn platform(ctx: CallContext) -> Result<JsValue> {
    ctx.env().string(platform_name())
}

#[function(js_name = "installPlan")]
fn install_plan(ctx: CallContext) -> Result<JsValue> {
    let options = install_options(&ctx)?;
    let params = json!({ "noShortcut": options.no_shortcut, "resetConfig": options.reset_config });
    let result = audit::run(capability_ids::INSTALL_PLAN, Mode::DryRun, params, move || {
        let plan = plan_install(&options)?;
        let fields = AuditFields::for_install_plan(&plan);
        let summary = Summary::new(fields.target_count, fields.target_summary);
        Ok((plan, summary))
    });
    match result {
        Ok(plan) => value_to_js(&ctx, &plan),
        Err(error) => Err(core_error(error)),
    }
}

#[function(js_name = "installApply")]
fn install_apply(ctx: CallContext) -> Result<JsValue> {
    let options = install_options(&ctx)?;
    let params = json!({ "noShortcut": options.no_shortcut, "resetConfig": options.reset_config });
    let result = audit::run(capability_ids::INSTALL_APPLY, Mode::Execute, params, move || {
        apply_install(&options)?;
        Ok(((), Summary::new(0, "install applied")))
    });
    match result {
        Ok(()) => {
            let object = ctx.env().object()?;
            object.set_named("ok", ctx.env().boolean(true)?)?;
            Ok(object)
        }
        Err(error) => Err(core_error(error)),
    }
}

#[function(js_name = "uninstallPlan")]
fn uninstall_plan(ctx: CallContext) -> Result<JsValue> {
    let options = uninstall_options(&ctx)?;
    let params = json!({ "keepConfig": options.keep_config });
    let result = audit::run(capability_ids::UNINSTALL_PLAN, Mode::DryRun, params, move || {
        let plan = plan_uninstall(&options)?;
        let fields = AuditFields::for_uninstall_plan(&plan);
        let summary = Summary::new(fields.target_count, fields.target_summary);
        Ok((plan, summary))
    });
    match result {
        Ok(plan) => value_to_js(&ctx, &plan),
        Err(error) => Err(core_error(error)),
    }
}

#[function(js_name = "uninstallApply")]
fn uninstall_apply(ctx: CallContext) -> Result<JsValue> {
    let options = uninstall_options(&ctx)?;
    let params = json!({ "keepConfig": options.keep_config });
    let result = audit::run(capability_ids::UNINSTALL_APPLY, Mode::Execute, params, move || {
        apply_uninstall(&options)?;
        Ok(((), Summary::new(0, "uninstall applied")))
    });
    match result {
        Ok(()) => {
            let object = ctx.env().object()?;
            object.set_named("ok", ctx.env().boolean(true)?)?;
            Ok(object)
        }
        Err(error) => Err(core_error(error)),
    }
}

#[function(js_name = "openGui")]
fn open_gui(ctx: CallContext) -> Result<JsValue> {
    let arg = object_arg(&ctx)?;
    let content_dir = resolve_gui_dir(optional_string(arg, "contentDir")?)?;
    let title = optional_string(arg, "title")?.unwrap_or_else(|| APP_TITLE.to_string());
    let second_launch = match optional_string(arg, "secondLaunch")? {
        Some(value) if value == "focus" || value == "reject" => value,
        Some(value) => {
            return Err(core_error(CoreError::policy(
                "window_config_invalid",
                format!("`secondLaunch` must be `focus` or `reject`, got `{value}`"),
            )));
        }
        None => "focus".to_string(),
    };
    let params = json!({ "contentDir": &content_dir, "title": &title });
    let log_path = ensure_gui_log();
    if log_path.is_none() {
        eprintln!("[open-finder] gui.log unavailable: host diagnostics stay on stderr only");
    }

    let mut backend: Option<String> = None;
    let result = audit::run(capability_ids::GUI_OPEN, Mode::Execute, params, || {
        GUI.with(|slot| {
            let mut slot = slot.borrow_mut();
            if let Some(host) = slot.as_mut()
                && host.is_alive()
            {
                if second_launch == "reject" {
                    return Err(CoreError::policy(
                        "window_second_launch_rejected",
                        "a window is already open and `window.secondLaunch` is `reject`",
                    ));
                }
                host.focus().map_err(webview_error)?;
                backend = Some(host.backend_name().to_string());
                return Ok(());
            }
            // A host left in the slot without a live window is stale.
            *slot = None;
            let options = OpenOptions::from_content_dir(&content_dir, &title);
            let host = open(options, Box::new(HostIpc)).map_err(webview_error)?;
            backend = Some(host.backend_name().to_string());
            *slot = Some(host);
            Ok(())
        })?;
        Ok(((), Summary::new(1, "gui window opened or focused")))
    });
    match result {
        Ok(()) => {
            let backend = backend.unwrap_or_default();
            let object = ctx.env().object()?;
            object.set_named("backend", ctx.env().string(&backend)?)?;
            Ok(object)
        }
        Err(error) => Err(core_error(error)),
    }
}

#[function(js_name = "pumpEvents")]
fn pump_events(ctx: CallContext) -> Result<JsValue> {
    let result = audit::run(capability_ids::GUI_PUMP, Mode::Execute, Value::Null, || {
        let alive = GUI.with(|slot| slot.borrow_mut().as_mut().map(|host| host.pump()).unwrap_or(false));
        Ok((alive, Summary::new(1, "gui event pump")))
    });
    match result {
        Ok(alive) => ctx.env().boolean(alive),
        Err(error) => Err(core_error(error)),
    }
}

#[function(js_name = "closeGui")]
fn close_gui(ctx: CallContext) -> Result<JsValue> {
    let result = audit::run(capability_ids::GUI_CLOSE, Mode::Execute, Value::Null, || {
        let closed = GUI.with(|slot| {
            let mut slot = slot.borrow_mut();
            match slot.as_mut() {
                Some(host) => host.close().is_ok(),
                None => false,
            }
        });
        if closed {
            GUI.with(|slot| {
                slot.borrow_mut().take();
            });
        }
        Ok((closed, Summary::new(1, "gui window close")))
    });
    match result {
        Ok(closed) => ctx.env().boolean(closed),
        Err(error) => Err(core_error(error)),
    }
}

#[function(js_name = "isGuiOpen")]
fn is_gui_open(ctx: CallContext) -> Result<JsValue> {
    let result = audit::run(capability_ids::GUI_STATUS, Mode::DryRun, Value::Null, || {
        let alive = GUI.with(|slot| slot.borrow().as_ref().map(|host| host.is_alive()).unwrap_or(false));
        Ok((alive, Summary::new(1, "gui status")))
    });
    match result {
        Ok(alive) => ctx.env().boolean(alive),
        Err(error) => Err(core_error(error)),
    }
}

#[function(js_name = "configureAudit")]
fn configure_audit(ctx: CallContext) -> Result<JsValue> {
    let arg = object_arg(&ctx)?;
    let actor = wae::audit::Actor {
        kind: actor_kind(optional_string(arg, "actorKind")?.as_deref()),
        id: optional_string(arg, "actorId")?.unwrap_or_else(|| "unknown".to_string()),
        source: optional_string(arg, "actorSource")?.unwrap_or_else(|| "local".to_string()),
    };
    let session = optional_string(arg, "session")?;
    let host = optional_string(arg, "host")?;
    audit::configure(actor, session, host);
    let object = ctx.env().object()?;
    object.set_named("ok", ctx.env().boolean(true)?)?;
    Ok(object)
}

#[function(js_name = "auditQuery")]
fn audit_query(ctx: CallContext) -> Result<JsValue> {
    let filter = query_from_arg(ctx.args().first().copied())?;
    let params = json!({ "capability": filter.capability, "limit": filter.limit });
    let result = audit::run(capability_ids::AUDIT_QUERY, Mode::DryRun, params, || {
        let entries = audit::query(&filter).map_err(|error| CoreError::message(error.to_string()))?;
        let summary = Summary::new(entries.len() as u64, "audit query");
        Ok((entries, summary))
    });
    match result {
        Ok(entries) => {
            let json = serde_json::to_string(&entries).map_err(|error| Error::new(error.to_string()))?;
            ctx.env().string(&json)
        }
        Err(error) => Err(core_error(error)),
    }
}

register_module![
    version,
    build_info,
    capabilities,
    default_index_dir_fn,
    inspect_volume_fn,
    index_build,
    index_update,
    search_fn,
    search_walk,
    platform,
    install_plan,
    install_apply,
    uninstall_plan,
    uninstall_apply,
    open_gui,
    pump_events,
    close_gui,
    is_gui_open,
    configure_audit,
    audit_query
];
