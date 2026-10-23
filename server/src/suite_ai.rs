//! 编辑器套件共用 OfficePilot 的模型配置（桌面版，用户在设置里开启）。
//!
//! - 在套件的 `ai-settings.json` 里维护一个自定义服务「OfficePilot（同步）」：地址、模型取自 OfficePilot
//!   当前使用的方案，密钥不写进文件，只写环境变量名 `OFFICEPILOT_SUITE_LLM_KEY`（套件的 apiKeyEnv）。
//! - OfficePilot 启动套件时把密钥放进套件进程的环境变量；文件里的其它内容原样保留。
//! - 第一次开启时把套件切换到这个服务并记下原来用的服务；关闭时删除这个服务并恢复。
//! - 开关状态记在数据目录的 `suite-ai-sync.json`。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::db::DbPool;
use crate::models::{AppSettings, LlmProfileConfig};

pub const ENDPOINT_ID: &str = "officepilot";
const ENDPOINT_NAME: &str = "OfficePilot（同步）";
pub const KEY_ENV: &str = "OFFICEPILOT_SUITE_LLM_KEY";

/// 套件的模型设置文件（套件的数据目录与产品名一致）
pub fn suite_settings_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("OFFICEPILOT_SUITE_SETTINGS") {
        return Some(PathBuf::from(p));
    }
    let home = PathBuf::from(std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?);
    let dir = if cfg!(target_os = "macos") {
        home.join("Library/Application Support")
    } else if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| home.join("AppData/Roaming"))
    } else {
        std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".config"))
    };
    // 编辑器套件的数据目录与产品名一致：现在是 OfficePilot，早期独立打包时是 OfficePilot Suite
    let current = dir.join("OfficePilot").join("ai-settings.json");
    let earlier = dir.join("OfficePilot Suite").join("ai-settings.json");
    Some(if !current.exists() && earlier.exists() { earlier } else { current })
}

fn state_path() -> PathBuf {
    PathBuf::from(&crate::config::config().data_dir).join("suite-ai-sync.json")
}

/// 开关状态，以及开启前套件使用的服务（关闭时恢复）
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SyncState {
    pub enabled: bool,
    pub owner_id: String,
    #[serde(default)]
    pub previous_provider: Option<String>,
    #[serde(default)]
    pub previous_endpoint: Option<String>,
    /// 开启前的 `providers.custom` 槽位，关闭时原样放回
    #[serde(default)]
    pub previous_slot: Option<Value>,
    /// 最近一次同步的方案和模型（界面显示）
    #[serde(default)]
    pub profile_name: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
}

pub fn load_state(path: &Path) -> SyncState {
    std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("创建 {} 失败：{e}", dir.display()))?;
    }
    let tmp = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4().simple()));
    std::fs::write(&tmp, bytes).map_err(|e| format!("写入 {} 失败：{e}", path.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("写入 {} 失败：{e}", path.display()))
}

fn save_state(path: &Path, state: &SyncState) -> Result<(), String> {
    write_atomic(path, &serde_json::to_vec_pretty(state).unwrap_or_default())
}

/// 读套件的设置；文件不存在时是空对象，内容损坏时报错（不覆盖用户的文件）
pub fn read_suite_settings(path: &Path) -> Result<Map<String, Value>, String> {
    match std::fs::read_to_string(path) {
        Ok(text) if text.trim().is_empty() => Ok(Map::new()),
        Ok(text) => match serde_json::from_str::<Value>(&text) {
            Ok(Value::Object(map)) => Ok(map),
            _ => Err(format!("编辑器套件的设置文件 {} 格式不对，没有修改它。可以在套件的设置里重新保存一次模型设置后再试", path.display())),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Map::new()),
        Err(e) => Err(format!("读取 {} 失败：{e}", path.display())),
    }
}

/// 当前方案和模型
pub fn active_profile(settings: &AppSettings) -> Option<(&LlmProfileConfig, String)> {
    let profile = settings.llm_profiles.iter().find(|p| p.id == settings.active_profile_id).or_else(|| settings.llm_profiles.first())?;
    let model = [settings.active_model.as_str(), settings.default_model.as_str(), profile.default_model.as_str()]
        .into_iter()
        .find(|m| !m.trim().is_empty() && profile.models.iter().any(|x| x == m))
        .map(str::to_string)
        .or_else(|| profile.models.first().cloned())?;
    Some((profile, model))
}

fn endpoint_json(profile: &LlmProfileConfig, model: &str) -> Value {
    json!({
        "id": ENDPOINT_ID,
        "name": ENDPOINT_NAME,
        "baseUrl": profile.base_url.trim(),
        "apiKey": "",
        "model": model,
        "models": profile.models,
        "api": "openai-compatible",
        "apiKeyEnv": KEY_ENV,
    })
}

/// 开启前套件使用的服务
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Previous {
    pub provider: Option<String>,
    pub endpoint: Option<String>,
    pub slot: Option<Value>,
}

/// 把同步的服务写进套件设置；`select` 时设为当前服务，返回原来使用的服务
pub fn apply(settings: &mut Map<String, Value>, profile: &LlmProfileConfig, model: &str, select: bool) -> Previous {
    let entry = endpoint_json(profile, model);
    let list = settings.entry("customEndpoints").or_insert_with(|| json!([]));
    if !list.is_array() {
        *list = json!([]);
    }
    let list = list.as_array_mut().unwrap();
    match list.iter_mut().find(|e| e["id"] == ENDPOINT_ID) {
        // 保留套件自己补充的字段（探测到的模型能力等）
        Some(existing) => {
            if let (Some(obj), Some(new)) = (existing.as_object_mut(), entry.as_object()) {
                for (k, v) in new {
                    obj.insert(k.clone(), v.clone());
                }
            }
        }
        None => list.push(entry),
    }
    let previous = Previous {
        provider: settings.get("provider").and_then(Value::as_str).map(str::to_string),
        endpoint: settings.get("customEndpoint").and_then(Value::as_str).map(str::to_string),
        slot: settings.get("providers").and_then(|p| p.get("custom")).cloned(),
    };
    if select {
        settings.insert("provider".into(), json!("custom"));
        settings.insert("customEndpoint".into(), json!(ENDPOINT_ID));
    }
    // 同步的服务正在使用时，`custom` 槽位与它保持一致（套件读取时也会这样整理）
    if settings.get("customEndpoint").and_then(Value::as_str) == Some(ENDPOINT_ID) {
        let providers = settings.entry("providers").or_insert_with(|| json!({}));
        if !providers.is_object() {
            *providers = json!({});
        }
        providers["custom"] = json!({
            "apiKey": "",
            "model": model,
            "baseUrl": profile.base_url.trim(),
            "profile": { "api": "openai-compatible", "apiKeyEnv": KEY_ENV },
        });
    }
    previous
}

/// 删除同步的服务；它正在使用时恢复开启前的服务
pub fn remove(settings: &mut Map<String, Value>, previous: &Previous) {
    if let Some(list) = settings.get_mut("customEndpoints").and_then(Value::as_array_mut) {
        list.retain(|e| e["id"] != ENDPOINT_ID);
        if list.is_empty() {
            settings.remove("customEndpoints");
        }
    }
    if settings.get("customEndpoint").and_then(Value::as_str) != Some(ENDPOINT_ID) {
        return;
    }
    let remaining: Vec<Value> = settings.get("customEndpoints").and_then(Value::as_array).cloned().unwrap_or_default();
    let has = |id: &str| remaining.iter().any(|e| e["id"] == id);
    // 原来选中的服务还在就恢复它，否则换成剩下的第一个
    let restore = previous.endpoint.clone().filter(|id| id != ENDPOINT_ID && has(id)).or_else(|| remaining.first().and_then(|e| e["id"].as_str()).map(str::to_string));
    let providers = settings.entry("providers").or_insert_with(|| json!({}));
    let slot = match &restore {
        Some(id) if previous.endpoint.as_deref() == Some(id.as_str()) => previous.slot.clone(),
        Some(id) => remaining.iter().find(|e| e["id"] == id.as_str()).map(|e| json!({ "apiKey": e["apiKey"], "model": e["model"], "baseUrl": e["baseUrl"] })),
        None => None,
    };
    if let Some(obj) = providers.as_object_mut() {
        match slot {
            Some(v) => {
                obj.insert("custom".into(), v);
            }
            None => {
                obj.remove("custom");
            }
        }
    }
    match &restore {
        Some(id) => {
            settings.insert("customEndpoint".into(), json!(id));
        }
        None => {
            settings.remove("customEndpoint");
        }
    }
    if settings.get("provider").and_then(Value::as_str) == Some("custom") {
        let back = match previous.provider.as_deref() {
            Some("custom") if restore.is_some() => "custom",
            Some(p) if p != "custom" => p,
            _ => "deepseek",
        };
        settings.insert("provider".into(), json!(back));
    }
}

async fn user_settings(pool: &DbPool, owner_id: &str) -> Result<AppSettings, String> {
    crate::routes::settings::effective_settings(pool, owner_id).await.map_err(|e| e.to_string())
}

/// 开启：写入同步的服务并选中它
pub async fn enable(pool: &DbPool, owner_id: &str, suite_file: &Path, state_file: &Path) -> Result<SyncState, String> {
    let settings = user_settings(pool, owner_id).await?;
    let (profile, model) = active_profile(&settings).ok_or("还没有配置模型服务")?;
    if profile.api_keys.is_empty() || profile.base_url.trim().is_empty() {
        return Err(format!("模型服务「{}」还没有填写地址或密钥，请先在设置 → 模型服务里配置", profile.name));
    }
    let mut suite = read_suite_settings(suite_file)?;
    let old = load_state(state_file);
    // 已经开启时不再记录「原来的服务」，以免记成同步的服务本身
    let select = !old.enabled;
    let previous = apply(&mut suite, profile, &model, select);
    write_atomic(suite_file, &serde_json::to_vec_pretty(&Value::Object(suite)).unwrap_or_default())?;
    let state = SyncState {
        enabled: true,
        owner_id: owner_id.to_string(),
        previous_provider: if select { previous.provider } else { old.previous_provider },
        previous_endpoint: if select { previous.endpoint } else { old.previous_endpoint },
        previous_slot: if select { previous.slot } else { old.previous_slot },
        profile_name: Some(profile.name.clone()),
        model: Some(model),
    };
    save_state(state_file, &state)?;
    Ok(state)
}

/// 关闭：删除同步的服务，恢复原来的服务
pub async fn disable(suite_file: &Path, state_file: &Path) -> Result<(), String> {
    let state = load_state(state_file);
    let mut suite = read_suite_settings(suite_file)?;
    remove(&mut suite, &Previous { provider: state.previous_provider.clone(), endpoint: state.previous_endpoint.clone(), slot: state.previous_slot.clone() });
    if suite_file.exists() {
        write_atomic(suite_file, &serde_json::to_vec_pretty(&Value::Object(suite)).unwrap_or_default())?;
    }
    save_state(state_file, &SyncState::default())
}

/// 开启时：按 OfficePilot 当前的方案更新同步的服务（不改变套件当前选中的服务）
pub async fn resync(pool: &DbPool, suite_file: &Path, state_file: &Path) -> Result<(), String> {
    let state = load_state(state_file);
    if !state.enabled {
        return Ok(());
    }
    let settings = user_settings(pool, &state.owner_id).await?;
    let Some((profile, model)) = active_profile(&settings) else { return Ok(()) };
    let mut suite = read_suite_settings(suite_file)?;
    apply(&mut suite, profile, &model, false);
    write_atomic(suite_file, &serde_json::to_vec_pretty(&Value::Object(suite)).unwrap_or_default())?;
    save_state(state_file, &SyncState { profile_name: Some(profile.name.clone()), model: Some(model), ..state })
}

/// 启动套件时要带上的环境变量（开启共用且有密钥时）
pub async fn launch_env(pool: &DbPool, state_file: &Path) -> Option<(&'static str, String)> {
    let state = load_state(state_file);
    if !state.enabled {
        return None;
    }
    let settings = user_settings(pool, &state.owner_id).await.ok()?;
    let (profile, _) = active_profile(&settings)?;
    profile.api_keys.first().filter(|k| !k.trim().is_empty()).map(|k| (KEY_ENV, k.clone()))
}

// ───────────────────────── 用全局路径的便捷入口 ─────────────────────────

pub fn paths() -> Option<(PathBuf, PathBuf)> {
    Some((suite_settings_path()?, state_path()))
}

/// 启动套件前调用：同步一次配置，返回要带上的环境变量
pub async fn prepare_launch() -> Option<(&'static str, String)> {
    let (suite_file, state_file) = paths()?;
    if !load_state(&state_file).enabled {
        return None;
    }
    let pool = crate::state::db_pool();
    if let Err(e) = resync(&pool, &suite_file, &state_file).await {
        tracing::warn!("[SuiteAI] 同步模型配置失败：{e}");
    }
    launch_env(&pool, &state_file).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(url: &str, models: &[&str]) -> LlmProfileConfig {
        LlmProfileConfig {
            id: "p1".into(),
            name: "DeepSeek".into(),
            base_url: url.into(),
            api_keys: vec!["sk-test-not-real".into()],
            models: models.iter().map(|m| m.to_string()).collect(),
            default_model: models[0].into(),
            api_key: None,
            has_api_key: true,
        }
    }

    fn user_file() -> Map<String, Value> {
        serde_json::from_value(json!({
            "provider": "qwen",
            "providers": { "qwen": { "apiKey": "user-qwen-key", "model": "qwen3.8-max" } },
            "customEndpoints": [{ "id": "mine", "name": "本地", "baseUrl": "http://localhost:1234/v1", "apiKey": "", "model": "llama" }],
            "customEndpoint": "mine",
            "search": { "provider": "parallel" },
            "maxOutputTokens": 9000
        }))
        .unwrap()
    }

    #[test]
    fn apply_keeps_user_content_and_never_writes_the_key() {
        let mut s = user_file();
        let prev = apply(&mut s, &profile("https://api.deepseek.com/v1", &["deepseek-v4-pro"]), "deepseek-v4-pro", true);
        assert_eq!((prev.provider.as_deref(), prev.endpoint.as_deref(), prev.slot), (Some("qwen"), Some("mine"), None));
        assert_eq!(s["provider"], "custom");
        assert_eq!(s["customEndpoint"], ENDPOINT_ID);
        assert_eq!(s["customEndpoints"].as_array().unwrap().len(), 2);
        assert_eq!(s["providers"]["qwen"]["apiKey"], "user-qwen-key");
        assert_eq!(s["providers"]["custom"]["profile"]["apiKeyEnv"], KEY_ENV);
        assert_eq!((s["search"].clone(), s["maxOutputTokens"].clone()), (json!({ "provider": "parallel" }), json!(9000)));
        let text = serde_json::to_string(&s).unwrap();
        assert!(!text.contains("sk-test-not-real"), "密钥不能写进套件的文件");

        // 再同步一次：结果不变；方案变了只更新这个服务
        let snapshot = s.clone();
        apply(&mut s, &profile("https://api.deepseek.com/v1", &["deepseek-v4-pro"]), "deepseek-v4-pro", false);
        assert_eq!(s, snapshot);
        s["customEndpoints"][1]["models"] = json!(["probed"]);
        s["customEndpoints"][1]["contextWindow"] = json!(128000);
        apply(&mut s, &profile("https://dashscope.aliyuncs.com/v1", &["qwen3.8-flash"]), "qwen3.8-flash", false);
        let ours = &s["customEndpoints"][1];
        assert_eq!((ours["baseUrl"].as_str(), ours["model"].as_str()), (Some("https://dashscope.aliyuncs.com/v1"), Some("qwen3.8-flash")));
        assert_eq!(ours["contextWindow"], 128000, "套件补充的字段保留");
        assert_eq!(s["providers"]["custom"]["model"], "qwen3.8-flash");

        // 用户在套件里换成了别的服务：不再改回来
        s.insert("customEndpoint".into(), json!("mine"));
        s.insert("provider".into(), json!("qwen"));
        apply(&mut s, &profile("https://x/v1", &["m"]), "m", false);
        assert_eq!((s["provider"].as_str(), s["customEndpoint"].as_str()), (Some("qwen"), Some("mine")));
    }

    #[test]
    fn remove_restores_previous_choice() {
        let mut s = user_file();
        let prev = apply(&mut s, &profile("https://api.deepseek.com/v1", &["m"]), "m", true);
        remove(&mut s, &prev);
        assert_eq!(s, user_file(), "关闭后和开启前一样");

        // 原来没有任何自定义服务
        let mut empty = Map::new();
        let prev = apply(&mut empty, &profile("https://api.deepseek.com/v1", &["m"]), "m", true);
        remove(&mut empty, &prev);
        assert_eq!(Value::Object(empty), json!({ "provider": "deepseek", "providers": {} }));
    }

    #[test]
    fn broken_or_missing_file() {
        let dir = std::env::temp_dir().join(format!("officepilot-suite-ai-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(read_suite_settings(&dir.join("none.json")).unwrap().is_empty());
        std::fs::write(dir.join("bad.json"), "{ not json").unwrap();
        assert!(read_suite_settings(&dir.join("bad.json")).unwrap_err().contains("格式不对"));
        std::fs::write(dir.join("arr.json"), "[]").unwrap();
        assert!(read_suite_settings(&dir.join("arr.json")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    async fn memory_pool() -> DbPool {
        sqlx::any::install_default_drivers();
        let pool = sqlx::any::AnyPoolOptions::new().max_connections(1).connect("sqlite::memory:").await.unwrap();
        sqlx::raw_sql(include_str!("../../migrations/001_init.sql")).execute(&pool).await.unwrap();
        sqlx::raw_sql("INSERT INTO users (id, username, password_hash, created_at, updated_at) VALUES ('u1', 'u', 'x', 't', 't');").execute(&pool).await.unwrap();
        pool
    }

    /// settings_repo::save_for_user 会额外占着一个连接，单连接的内存库里直接写表
    async fn put_settings(pool: &DbPool, settings: &AppSettings) {
        sqlx::query("DELETE FROM user_settings WHERE user_id = 'u1'").execute(pool).await.unwrap();
        sqlx::query("INSERT INTO user_settings (id, user_id, payload, created_at, updated_at) VALUES ('s1', 'u1', ?, 't', 't')")
            .bind(serde_json::to_string(settings).unwrap())
            .execute(pool)
            .await
            .unwrap();
    }

    fn app_settings(p: LlmProfileConfig) -> AppSettings {
        serde_json::from_value(json!({
            "llm_profiles": [p],
            "active_profile_id": "p1",
            "default_model": "",
            "active_model": "",
            "basic": { "app_name": "OfficePilot", "workspace_title": "", "brand_tagline": "", "default_theme": "default" },
            "updated_at": "t"
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn enable_resync_launch_env_disable() {
        let pool = memory_pool().await;
        let dir = std::env::temp_dir().join(format!("officepilot-suite-ai-{}", uuid::Uuid::new_v4().simple()));
        let (suite_file, state_file) = (dir.join("suite/ai-settings.json"), dir.join("state.json"));
        put_settings(&pool, &app_settings(profile("https://api.deepseek.com/v1", &["deepseek-v4-pro", "deepseek-flash"]))).await;
        assert!(launch_env(&pool, &state_file).await.is_none(), "未开启时不传密钥");

        std::fs::create_dir_all(suite_file.parent().unwrap()).unwrap();
        std::fs::write(&suite_file, serde_json::to_string(&Value::Object(user_file())).unwrap()).unwrap();
        let state = enable(&pool, "u1", &suite_file, &state_file).await.unwrap();
        assert_eq!((state.model.as_deref(), state.previous_provider.as_deref()), (Some("deepseek-v4-pro"), Some("qwen")));
        // 重复开启不把「原来的服务」记成同步的服务
        assert_eq!(enable(&pool, "u1", &suite_file, &state_file).await.unwrap().previous_provider.as_deref(), Some("qwen"));
        assert_eq!(launch_env(&pool, &state_file).await, Some((KEY_ENV, "sk-test-not-real".to_string())));
        assert!(!std::fs::read_to_string(&suite_file).unwrap().contains("sk-test-not-real"));

        // OfficePilot 里换了模型
        let mut s = app_settings(profile("https://api.deepseek.com/v1", &["deepseek-v4-pro", "deepseek-flash"]));
        s.active_model = "deepseek-flash".into();
        put_settings(&pool, &s).await;
        resync(&pool, &suite_file, &state_file).await.unwrap();
        let written: Value = serde_json::from_str(&std::fs::read_to_string(&suite_file).unwrap()).unwrap();
        assert_eq!(written["providers"]["custom"]["model"], "deepseek-flash");
        assert_eq!(load_state(&state_file).model.as_deref(), Some("deepseek-flash"));

        disable(&suite_file, &state_file).await.unwrap();
        let restored: Value = serde_json::from_str(&std::fs::read_to_string(&suite_file).unwrap()).unwrap();
        assert_eq!(restored, Value::Object(user_file()));
        assert!(!load_state(&state_file).enabled);
        assert!(launch_env(&pool, &state_file).await.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
