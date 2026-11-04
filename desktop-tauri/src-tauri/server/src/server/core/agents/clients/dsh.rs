//! DeepSeek Harness（`dsh`）的适配。
//!
//! 当前 DSH 把用户配置写在 `~/.dsh/profiles/<profile>/cordis.patch.yml` 的
//! `llm-pi-ai` 与 `agent-default-model` patch 项里；旧版本使用 `~/.dsh/settings.yaml`。
//! 两种布局都支持，托管文件以实际检测到的路径为准。密钥不在配置里 —— `apiKeyEnv`
//! 是凭据引用名，值放在 `DSH_HOME/.credentials.yaml` 的 `refs` 下，因此接入会改配置与
//! 凭据两个文件。两处都是 DSH 运行时按需读取的用户层，改完无需重启。
//!
//! 字段与取值依据是 DSH 0.1.2-rc.1 的 `@deepseek-ai/dsh-llm-pi-ai`（已装在本机，直接读的类型
//! 定义）与 `dsh-credentials-local`：
//!
//! - 自定义路由必须自己给出 `api` / `baseURL` / `models`，可用的 `api` 只有
//!   `openai-completions`、`openai-responses`、`anthropic-messages`；
//! - 模型档只认 `id` / `name` / `contextWindow` / `maxTokens` / `input[]` / `reasoningEfforts`，
//!   `input` 的取值只有 `text` 与 `image`；思考档位只有
//!   `off` / `minimal` / `low` / `medium` / `high` / `xhigh` / `max`；
//! - 无法服务的字段会让 DSH 在写入时拒绝**整段** `llm-pi-ai` 命名空间，所以这里只写它认的字段，
//!   网关目录里对不上的思考档位直接丢掉，不猜；
//! - `.credentials.yaml` 必须是 `version: 1` 且顶层只有 `version` / `refs` / `records`，
//!   否则 DSH 自己会拒绝启动，本页也就拒绝改写；
//! - 官方设置页派生的凭据名是 `<路由转大写、非字母数字转下划线>_API_KEY`，这里沿用同一口径。

use super::super::{
    files::{self, Change},
    validate, yaml_editing, Model, Profile,
};
use super::{native_entry, native_id, remove, set};
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

const NAMESPACE: &str = "llm-pi-ai";
const DEFAULT_MODEL: &str = "agent-default-model";
/// DSH 允许列出的思考档位，顺序即设置页的展示顺序。
const LEVELS: [&str; 7] = ["off", "minimal", "low", "medium", "high", "xhigh", "max"];

const LLM_NAME: &str = "@deepseek-ai/dsh-llm-pi-ai";
const DEFAULT_MODEL_NAME: &str = "@deepseek-ai/dsh-agent-default-model";

fn patch_config<'a>(value: &'a Value, id: &str) -> Option<&'a Value> {
    if value.is_object() {
        return value.get(id);
    }
    value
        .as_array()?
        .iter()
        .find(|entry| entry.get("id").and_then(Value::as_str) == Some(id))?
        .get("config")
}

fn patch_config_mut_existing<'a>(value: &'a mut Value, id: &str) -> Option<&'a mut Value> {
    if value.is_object() {
        return value.get_mut(id).filter(|entry| entry.is_object());
    }
    let index = value
        .as_array()?
        .iter()
        .position(|entry| entry.get("id").and_then(Value::as_str) == Some(id))?;
    value.as_array_mut()?.get_mut(index)?.get_mut("config")
}

fn patch_config_mut<'a>(
    value: &'a mut Value,
    id: &str,
    name: &str,
) -> Result<&'a mut Value, String> {
    if value.is_object() {
        let object = value.as_object_mut().expect("object checked");
        if !object.get(id).is_some_and(Value::is_object) {
            object.insert(id.to_string(), json!({}));
        }
        return Ok(object.get_mut(id).expect("inserted or existing object"));
    }
    let entries = value
        .as_array_mut()
        .ok_or("DSH profile patch 根节点必须是数组")?;
    let index = match entries
        .iter()
        .position(|entry| entry.get("id").and_then(Value::as_str) == Some(id))
    {
        Some(index) => index,
        None => {
            entries.push(json!({ "id": id, "name": name, "config": {} }));
            entries.len() - 1
        }
    };
    let entry = entries[index]
        .as_object_mut()
        .ok_or_else(|| format!("DSH profile patch 的 id={id} 项必须是映射"))?;
    if !entry.get("config").is_some_and(Value::is_object) {
        entry.insert("config".to_string(), json!({}));
    }
    Ok(entry.get_mut("config").expect("config inserted or existing object"))
}

fn prune_empty_patch(value: &mut Value, id: &str) {
    let Some(entries) = value.as_array_mut() else {
        return;
    };
    let Some(index) = entries
        .iter()
        .position(|entry| entry.get("id").and_then(Value::as_str) == Some(id))
    else {
        return;
    };
    let empty = entries[index]
        .get("config")
        .and_then(Value::as_object)
        .is_some_and(Map::is_empty);
    if empty {
        entries.remove(index);
    }
}

fn api_of(protocol: &str) -> Option<&'static str> {
    match protocol {
        "openai_chat" => Some("openai-completions"),
        "responses" => Some("openai-responses"),
        "anthropic" => Some("anthropic-messages"),
        _ => None,
    }
}

fn protocol_of(api: &str) -> &'static str {
    match api {
        "openai-responses" => "responses",
        "anthropic-messages" => "anthropic",
        "openai-completions" => "openai_chat",
        _ => "",
    }
}

/// 路由键是 DSH 设置里的字典键，统一成小写连字符标识符。
fn route_of(source: &str, profile: &Profile) -> String {
    if !source.trim().is_empty() {
        return source.trim().to_string();
    }
    let raw = native_id(profile).to_lowercase();
    let mut route = String::new();
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' {
            route.push(ch);
        } else if !route.ends_with('-') {
            route.push('-');
        }
    }
    let route = route.trim_matches('-').to_string();
    if route.is_empty() {
        "agent2api".into()
    } else {
        route
    }
}

/// 凭据引用名：与 DSH 设置页的派生口径一致（`<路由>_API_KEY`）。
fn credential_ref(route: &str) -> String {
    let mut name = String::new();
    for ch in route.chars() {
        if ch.is_ascii_alphanumeric() {
            name.push(ch.to_ascii_uppercase());
        } else if !name.ends_with('_') {
            name.push('_');
        }
    }
    format!("{}_API_KEY", name.trim_matches('_'))
}

/// 凭据文件跟 DSH 自己的解析口径走：`$DSH_HOME/.credentials.yaml`，没设变量时就在设置文件旁边。
pub fn credential_path(path: &Path) -> PathBuf {
    match std::env::var_os("DSH_HOME").filter(|value| !value.is_empty()) {
        Some(home) => PathBuf::from(home).join(".credentials.yaml"),
        None => path.with_file_name(".credentials.yaml"),
    }
}

fn references(path: &Path) -> Result<Map<String, Value>, String> {
    let file = credential_path(path);
    let Some(bytes) = files::read(&file)? else {
        return Ok(Map::new());
    };
    let text = files::text(Some(&bytes))?;
    if text.trim().is_empty() {
        return Ok(Map::new());
    }
    // 读侧宽容：凭据文件格式不对时按「没有可用密钥」处理，写入侧仍会拒绝改写。
    let Ok(value) = yaml_editing::parse(&text) else {
        return Ok(Map::new());
    };
    Ok(value
        .get("refs")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default())
}

pub fn read(path: &Path, value: &Value) -> Result<Vec<Profile>, String> {
    let Some(providers) = patch_config(value, NAMESPACE)
        .and_then(|config| config.get("providers"))
        .and_then(Value::as_object)
    else {
        return Ok(Vec::new());
    };
    let refs = references(path)?;
    let selection = patch_config(value, DEFAULT_MODEL);
    let active = selection
        .and_then(|value| value.get("provider"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let mut profiles = Vec::new();
    for (route, entry) in providers {
        let Some(fields) = entry.as_object() else {
            continue;
        };
        let api = fields.get("api").and_then(Value::as_str).unwrap_or("");
        let display = fields
            .get("displayName")
            .and_then(Value::as_str)
            .filter(|name| !name.trim().is_empty())
            .unwrap_or(route);
        let mut profile = Profile::new(
            route,
            display,
            protocol_of(api),
            fields.get("baseURL").and_then(Value::as_str).unwrap_or(""),
        );
        profile.env_key = fields
            .get("apiKeyEnv")
            .and_then(Value::as_str)
            .unwrap_or("")
            .into();
        profile.api_key = refs
            .get(&profile.env_key)
            .and_then(Value::as_str)
            .unwrap_or("")
            .into();
        if active == route {
            profile.model = selection
                .and_then(|value| value.get("model"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .into();
            profile.effort = selection
                .and_then(|value| value.get("reasoningEffort"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .into();
        }
        profile.enabled = active == route;
        profile.default = profile.enabled;
        profile.native.entry = entry.clone();
        profile.native.source_id = route.clone();
        profile.native.settings = selection.cloned().unwrap_or(Value::Null);
        profile.models = read_models(fields.get("models"));
        profiles.push(profile);
    }
    Ok(profiles)
}

fn read_models(value: Option<&Value>) -> Vec<Model> {
    let Some(items) = value.and_then(Value::as_array) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|entry| {
            let id = entry.get("id").and_then(Value::as_str)?;
            let efforts = entry.get("reasoningEfforts");
            Some(Model {
                id: id.into(),
                name: entry
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .into(),
                context_window: entry.get("contextWindow").and_then(Value::as_u64),
                max_output_tokens: entry.get("maxTokens").and_then(Value::as_u64),
                supports_tools: None,
                supports_images: entry
                    .get("input")
                    .and_then(Value::as_array)
                    .map(|list| list.iter().any(|value| value.as_str() == Some("image"))),
                supports_reasoning: match efforts {
                    Some(Value::Bool(false)) => Some(false),
                    Some(Value::Object(map)) if !map.is_empty() => Some(true),
                    _ => None,
                },
                reasoning_levels: efforts
                    .and_then(Value::as_object)
                    .map(|map| map.keys().cloned().collect())
                    .unwrap_or_default(),
                effort: String::new(),
            })
        })
        .collect()
}

/// 网关目录里的思考档位映射到 DSH 认的档位；对不上的直接丢掉，不猜取值。
fn efforts(levels: &[String]) -> Map<String, Value> {
    let mut offered: Vec<(&'static str, Option<&'static str>)> = Vec::new();
    for level in levels {
        let mapped = match level.trim().to_ascii_lowercase().as_str() {
            "off" | "none" | "disabled" => Some(("off", None)),
            "minimal" => Some(("minimal", Some("minimal"))),
            "low" => Some(("low", Some("low"))),
            "medium" | "mid" => Some(("medium", Some("medium"))),
            "high" => Some(("high", Some("high"))),
            "xhigh" => Some(("xhigh", Some("xhigh"))),
            "max" => Some(("max", Some("max"))),
            _ => None,
        };
        if let Some(entry) = mapped {
            if !offered.iter().any(|(key, _)| *key == entry.0) {
                offered.push(entry);
            }
        }
    }
    let mut ordered = Map::new();
    for key in LEVELS {
        if let Some((_, wire)) = offered.iter().find(|(name, _)| *name == key) {
            ordered.insert(key.into(), wire.map_or(Value::Null, |wire| json!(wire)));
        }
    }
    ordered
}

/// 模型条目：沿用文件里同名条目的未知字段（`compat` 之类），本页认识的字段以方案为准。
fn model_entry(model: &Model, existing: Option<&Value>) -> Result<Value, String> {
    if model.id.trim().is_empty() {
        return Err("模型 ID 不能为空".into());
    }
    let id = model.id.trim().to_string();
    let mut entry = existing
        .filter(|value| value.is_object())
        .cloned()
        .unwrap_or_else(|| json!({}));
    set(&mut entry, &["id"], json!(id))?;
    if !model.name.trim().is_empty() && model.name.trim() != id {
        set(&mut entry, &["name"], json!(model.name.trim()))?;
    } else {
        remove(&mut entry, &["name"]);
    }
    match model.context_window {
        Some(value) => set(&mut entry, &["contextWindow"], json!(value))?,
        None => remove(&mut entry, &["contextWindow"]),
    }
    match model.max_output_tokens {
        Some(value) => set(&mut entry, &["maxTokens"], json!(value))?,
        None => remove(&mut entry, &["maxTokens"]),
    }
    // 少报比多报安全：DSH 会因为多报的模态在会话中途被上游拒绝，而少报只是提前拒绝。
    set(
        &mut entry,
        &["input"],
        json!(if model.supports_images == Some(true) {
            vec!["text", "image"]
        } else {
            vec!["text"]
        }),
    )?;
    if model.supports_reasoning == Some(false) {
        set(&mut entry, &["reasoningEfforts"], json!(false))?;
    } else {
        let offered = efforts(&model.reasoning_levels);
        if offered.is_empty() {
            remove(&mut entry, &["reasoningEfforts"]);
        } else {
            set(&mut entry, &["reasoningEfforts"], Value::Object(offered))?;
        }
    }
    Ok(entry)
}

pub fn write(
    path: &Path,
    value: &mut Value,
    profile: &Profile,
    operation: &str,
    warnings: &mut Vec<String>,
) -> Result<Vec<Change>, String> {
    let api = api_of(&profile.protocol)
        .ok_or("DSH 只支持 OpenAI Chat Completions、OpenAI Responses 与 Anthropic Messages")?;
    let base = profile.base_url.trim().trim_end_matches('/');
    if base.is_empty() {
        return Err("请填写 DSH 的接口基址".into());
    }
    let route = route_of(&profile.native.source_id, profile);
    let existing = patch_config(value, NAMESPACE)
        .and_then(|config| config.get("providers"))
        .and_then(|providers| providers.get(route.as_str()));
    let mut entry = native_entry(existing, &profile.native.entry)?;
    set(
        &mut entry,
        &["displayName"],
        json!(if profile.name.trim().is_empty() {
            "Agent2API"
        } else {
            profile.name.trim()
        }),
    )?;
    set(&mut entry, &["api"], json!(api))?;
    set(&mut entry, &["baseURL"], json!(base))?;
    let previous = entry
        .get("models")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let models = profile
        .models
        .iter()
        .map(|model| {
            let existing = previous.iter().find(|item| {
                item.get("id").and_then(Value::as_str) == Some(model.id.trim())
            });
            model_entry(model, existing)
        })
        .collect::<Result<Vec<_>, _>>()?;
    set(&mut entry, &["models"], json!(models))?;
    let existing_ref = entry
        .get("apiKeyEnv")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    let mut pending = None;
    match validate::credential(profile)?.as_deref() {
        Some(key) if !key.is_empty() => {
            // 导入来的条目沿用它自己的引用名，本页新写的用派生名，便于断开时清理。
            let reference = if profile.auth_mode == "preserve" && !existing_ref.is_empty() {
                existing_ref.clone()
            } else {
                credential_ref(&route)
            };
            set(&mut entry, &["apiKeyEnv"], json!(reference))?;
            pending = Some((reference, key.to_string()));
        }
        Some(_) => remove(&mut entry, &["apiKeyEnv"]),
        None if profile.auth_mode == "env" => {
            set(&mut entry, &["apiKeyEnv"], json!(profile.env_key))?;
        }
        None => {}
    }
    let llm = patch_config_mut(value, NAMESPACE, LLM_NAME)?;
    set(llm, &["providers", &route], entry)?;
    let selected = patch_config(value, DEFAULT_MODEL)
        .and_then(|config| config.get("provider"))
        .and_then(Value::as_str)
        == Some(route.as_str());
    if (operation == "default" || selected) && !profile.model.trim().is_empty() {
        let selection = patch_config_mut(value, DEFAULT_MODEL, DEFAULT_MODEL_NAME)?;
        set(selection, &["provider"], json!(route))?;
        set(selection, &["model"], json!(profile.model.trim()))?;
        // 网关目录把「关闭思考」写作 off / none 两档，DSH 只有 off。
        let effort = match profile.effort.trim() {
            "" | "default" => None,
            "none" => Some("off"),
            other => Some(other),
        };
        match effort {
            Some(effort) => set(selection, &["reasoningEffort"], json!(effort))?,
            None => remove(selection, &["reasoningEffort"]),
        }
    }
    let mut extra = Vec::new();
    if let Some((reference, key)) = pending {
        extra.push(credential_change(path, &reference, Some(&key))?);
        warnings.push(
            "同时修改 DSH 配置与 .credentials.yaml（密钥只以引用名出现在配置里）。".into(),
        );
    }
    Ok(extra)
}

fn credential_change(path: &Path, reference: &str, key: Option<&str>) -> Result<Change, String> {
    let file = credential_path(path);
    let mut change = Change::load(file)?;
    let original = files::text(change.before.as_deref())?;
    let mut store = if original.trim().is_empty() {
        json!({ "version": 1 })
    } else {
        yaml_editing::parse(&original)?
    };
    if store.as_object().is_some_and(Map::is_empty) {
        store = json!({ "version": 1 });
    }
    supported(&store, &change.path)?;
    if store.get("refs").is_none_or(Value::is_null) {
        set(&mut store, &["refs"], json!({}))?;
    }
    match key {
        Some(key) => set(&mut store, &["refs", reference], json!(key))?,
        None => remove(&mut store, &["refs", reference]),
    }
    change.after = Some(yaml_editing::render(&original, &store)?.into_bytes());
    Ok(change)
}

/// DSH 对凭据文件的形状是硬要求：缺 `version`（旧扁平布局）或出现未知顶层字段时它自己会拒绝
/// 启动，这里跟着拒绝，避免把它写坏。
fn supported(store: &Value, path: &Path) -> Result<(), String> {
    match store.get("version").and_then(Value::as_u64) {
        Some(1) => {}
        Some(version) => {
            return Err(format!(
                "{} 的 version 是 {version}，本页只认 DSH 的 version 1 布局",
                path.display()
            ))
        }
        None => {
            return Err(format!(
                "{} 不是 version 1 布局：先启动一次 DSH 让它自行迁移，再回到本页接入",
                path.display()
            ))
        }
    }
    for key in store.as_object().map(Map::keys).into_iter().flatten() {
        if !matches!(key.as_str(), "version" | "refs" | "records") {
            return Err(format!(
                "{} 含有未知顶层字段 {key}，DSH 自身也会拒绝读取，请先处理",
                path.display()
            ));
        }
    }
    Ok(())
}

pub fn delete(path: &Path, value: &mut Value, profile: &Profile) -> Result<Vec<Change>, String> {
    let route = route_of(&profile.native.source_id, profile);
    let selected = patch_config(value, DEFAULT_MODEL)
        .and_then(|config| config.get("provider"))
        .and_then(Value::as_str)
        == Some(route.as_str());
    if let Some(llm) = patch_config_mut_existing(value, NAMESPACE) {
        remove(llm, &["providers", &route]);
    }
    prune_empty_patch(value, NAMESPACE);
    if value.is_array() {
        if selected {
            if let Some(selection) = patch_config_mut_existing(value, DEFAULT_MODEL) {
                remove(selection, &["provider"]);
                remove(selection, &["model"]);
                remove(selection, &["reasoningEffort"]);
            }
            prune_empty_patch(value, DEFAULT_MODEL);
        }
    } else if selected {
        // 旧 settings.yaml 的默认模型指向被删提供商时整段移除，回到 DSH 自带默认。
        remove(value, &[DEFAULT_MODEL]);
    }
    let mut extra = Vec::new();
    let file = credential_path(path);
    if Change::load(file.clone())?.before.is_some() {
        extra.push(credential_change(path, &credential_ref(&route), None)?);
    }
    Ok(extra)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn patch_document() -> Value {
        yaml_editing::parse(
            r#"
- id: llm-pi-ai
  name: "@deepseek-ai/dsh-llm-pi-ai"
  config:
    providers:
      cc:
        displayName: Agent2API
        apiKeyEnv: CC_API_KEY
        api: openai-responses
        baseURL: http://127.0.0.1:3065/v1
        models:
          - id: deepseek-v4.1-flash
- id: agent-default-model
  name: "@deepseek-ai/dsh-agent-default-model"
  config:
    provider: cc
    model: deepseek-v4.1-flash
    reasoningEffort: high
"#,
        )
        .expect("测试 patch 应当是合法 YAML")
    }

    fn temp_patch_path() -> PathBuf {
        std::env::temp_dir()
            .join(format!("agent2api-dsh-{}", std::process::id()))
            .join("profiles/desktop/cordis.patch.yml")
    }

    #[test]
    fn reads_current_profile_patch_layout() {
        let path = temp_patch_path();
        let profiles = read(&path, &patch_document()).expect("应当读取 DSH profile patch");
        assert_eq!(1, profiles.len());
        assert_eq!("cc", profiles[0].native.source_id);
        assert_eq!("responses", profiles[0].protocol);
        assert_eq!("http://127.0.0.1:3065/v1", profiles[0].base_url);
        assert_eq!("deepseek-v4.1-flash", profiles[0].model);
        assert!(profiles[0].enabled);
    }

    #[test]
    fn writes_and_deletes_current_profile_patch_layout() {
        let path = temp_patch_path();
        let mut value = patch_document();
        let mut profile = Profile::new(
            "agent2api",
            "Agent2API",
            "responses",
            "http://127.0.0.1:3065/v1",
        );
        profile.model = "glm-5.3-flash".into();
        profile.native.source_id = "agent2api".into();
        let mut warnings = Vec::new();
        let extra = write(
            &path,
            &mut value,
            &profile,
            "default",
            &mut warnings,
        )
        .expect("应当写入 DSH profile patch");
        assert!(extra.is_empty());
        assert!(warnings.is_empty());
        let providers = patch_config(&value, NAMESPACE)
            .and_then(|config| config.get("providers"))
            .and_then(Value::as_object)
            .expect("providers 映射应当存在");
        assert_eq!(
            Some("http://127.0.0.1:3065/v1"),
            providers["agent2api"]["baseURL"].as_str()
        );
        let selection = patch_config(&value, DEFAULT_MODEL).expect("默认模型 patch 应当存在");
        assert_eq!(Some("agent2api"), selection["provider"].as_str());
        assert_eq!(Some("glm-5.3-flash"), selection["model"].as_str());

        delete(&path, &mut value, &profile).expect("应当从 profile patch 删除");
        let providers = patch_config(&value, NAMESPACE)
            .and_then(|config| config.get("providers"))
            .and_then(Value::as_object)
            .expect("providers 映射应当存在");
        assert!(!providers.contains_key("agent2api"));
        assert!(patch_config(&value, DEFAULT_MODEL).is_none());
    }
}
