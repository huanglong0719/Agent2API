use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{files, ClientDef};

#[derive(Default, Serialize, Deserialize)]
struct Preferences {
    paths: std::collections::BTreeMap<String, String>,
}

fn preferences_file() -> PathBuf {
    crate::paths::config_dir().join("agent-paths.json")
}

fn preferences() -> Result<Preferences, String> {
    let Some(bytes) = files::read(&preferences_file())? else {
        return Ok(Preferences::default());
    };
    serde_json::from_slice(&bytes)
        .map_err(|error| format!("配置路径偏好无法读取，拒绝覆盖: {error}"))
}

pub fn path_override(id: &str) -> Option<String> {
    preferences().ok()?.paths.remove(id)
}

pub fn save_override(def: &ClientDef, raw: &str) -> Result<(), String> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = LOCK.lock().map_err(|_| "配置路径锁不可用")?;
    let mut preferences = preferences()?;
    let raw = raw.trim();
    if raw.is_empty() {
        preferences.paths.remove(def.id);
    } else {
        let path = expand(raw)?;
        if !path.is_absolute() || path.is_dir() {
            return Err("配置路径必须是绝对文件路径，不能是目录".into());
        }
        let valid_name = match def.id {
            "codex" | "kimi-code" => path.file_name().is_some_and(|name| name == "config.toml"),
            "workbuddy" => path.file_name().is_some_and(|name| name == "models.json"),
            "cline" => path
                .file_name()
                .is_some_and(|name| name == "providers.json"),
            "zcode" => path
                .file_name()
                .is_some_and(|name| name == "provider_config.json"),
            "crush" => path.file_name().is_some_and(|name| name == "crush.json"),
            "dsh" => path.file_name().is_some_and(|name| {
                name == "settings.yaml" || name == "cordis.patch.yml"
            }),
            "cursor-cli" => path
                .file_name()
                .is_some_and(|name| name == "cli-config.json"),
            "opencode" => path.file_name().is_some_and(|name| {
                name == "opencode.json" || name == "opencode.jsonc" || name == "config.json"
            }),
            _ => path.file_name().is_some_and(|name| name == "settings.json"),
        };
        if !valid_name {
            return Err("文件名与这个客户端的配置文件不匹配".into());
        }
        preferences
            .paths
            .insert(def.id.into(), path.display().to_string());
    }
    let text =
        serde_json::to_vec_pretty(&preferences).map_err(|e| format!("保存配置路径失败: {e}"))?;
    files::write_atomic(&preferences_file(), &text)
}

pub fn home() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn expand(raw: &str) -> Result<PathBuf, String> {
    if raw == "~" {
        return Ok(home());
    }
    if let Some(rest) = raw.strip_prefix("~/").or_else(|| raw.strip_prefix("~\\")) {
        return Ok(home().join(rest));
    }
    Ok(PathBuf::from(raw))
}

fn env_dir(key: &str) -> Option<PathBuf> {
    std::env::var_os(key)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

fn dsh_candidates(home: &Path) -> Vec<PathBuf> {
    let profiles = home.join("profiles");
    let mut names: Vec<String> = std::fs::read_dir(&profiles)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect();
    names.sort_by_key(|name| match name.as_str() {
        "desktop" => (0, name.clone()),
        "web" => (1, name.clone()),
        "headless" => (2, name.clone()),
        _ => (3, name.clone()),
    });
    let mut paths: Vec<PathBuf> = names
        .into_iter()
        .map(|name| profiles.join(name).join("cordis.patch.yml"))
        .collect();
    paths.push(home.join("cordis.patch.yml"));
    // 旧版 DSH 的全局 settings.yaml 只在没有 profile patch 时作为兼容入口。
    paths.push(home.join("settings.yaml"));
    paths
}

pub fn candidates(def: &ClientDef) -> Vec<PathBuf> {
    let home = home();
    let xdg = env_dir("XDG_CONFIG_HOME").unwrap_or_else(|| home.join(".config"));
    let dir = |env: &str, fallback: &str| env_dir(env).unwrap_or_else(|| home.join(fallback));
    match def.id {
        "claude-code" => vec![dir("CLAUDE_CONFIG_DIR", ".claude").join("settings.json")],
        "codex" => vec![dir("CODEX_HOME", ".codex").join("config.toml")],
        "gemini-cli" => vec![env_dir("GEMINI_CLI_HOME")
            .unwrap_or_else(|| home.clone())
            .join(".gemini/settings.json")],
        "zcode" => vec![home.join(".zcode/v2/provider_config.json")],
        "dsh" => dsh_candidates(&dir("DSH_HOME", ".dsh")),
        "workbuddy" => vec![
            dir("WORKBUDDY_CONFIG_DIR", ".workbuddy").join("models.json"),
            home.join(".codebuddy/models.json"),
        ],
        "qoder" => vec![
            dir("QODER_CONFIG_DIR", ".qoder").join("settings.json"),
            dir("QODERCN_CONFIG_DIR", ".qoder-cn").join("settings.json"),
        ],
        "cline" => vec![env_dir("CLINE_DATA_DIR")
            .unwrap_or_else(|| dir("CLINE_DIR", ".cline").join("data"))
            .join("settings/providers.json")],
        "opencode" => {
            if let Some(path) = env_dir("OPENCODE_CONFIG") {
                return vec![path];
            }
            let dir = env_dir("OPENCODE_CONFIG_DIR").unwrap_or_else(|| xdg.join("opencode"));
            vec![
                dir.join("opencode.jsonc"),
                dir.join("opencode.json"),
                dir.join("config.json"),
            ]
        }
        "kimi-code" => {
            if let Some(dir) = env_dir("KIMI_CODE_HOME") {
                return vec![dir.join("config.toml")];
            }
            if let Some(dir) = env_dir("KIMI_SHARE_DIR") {
                return vec![dir.join("config.toml")];
            }
            vec![
                home.join(".kimi-code/config.toml"),
                home.join(".kimi/config.toml"),
            ]
        }
        "cursor-cli" => vec![env_dir("CURSOR_CONFIG_DIR")
            .unwrap_or_else(|| {
                if cfg!(target_os = "linux") {
                    xdg.join("cursor")
                } else {
                    home.join(".cursor")
                }
            })
            .join("cli-config.json")],
        "zed" => vec![if cfg!(windows) {
            env_dir("APPDATA")
                .unwrap_or_else(|| home.join("AppData/Roaming"))
                .join("Zed/settings.json")
        } else {
            xdg.join("zed/settings.json")
        }],
        "crush" => vec![env_dir("CRUSH_GLOBAL_CONFIG")
            .unwrap_or_else(|| {
                if cfg!(windows) {
                    env_dir("LOCALAPPDATA")
                        .unwrap_or_else(|| home.join("AppData/Local"))
                        .join("crush")
                } else {
                    xdg.join("crush")
                }
            })
            .join("crush.json")],
        _ => Vec::new(),
    }
}

pub struct Detection {
    pub path: PathBuf,
    pub detected: bool,
    pub evidence: Vec<String>,
    pub version: String,
    pub unsupported: bool,
    pub note: Option<String>,
}

fn choose_path(
    override_path: Option<PathBuf>,
    paths: &[PathBuf],
    fallback_index: usize,
) -> (PathBuf, bool) {
    let auto_path = paths.iter().find(|path| path.is_file()).cloned();
    let override_is_file = override_path.as_ref().is_some_and(|path| path.is_file());
    let used_override = override_is_file && auto_path.as_ref() != override_path.as_ref();
    let path = override_path
        .as_ref()
        .filter(|path| path.is_file())
        .cloned()
        .or(auto_path)
        .or(override_path)
        .or_else(|| paths.get(fallback_index).cloned())
        .or_else(|| paths.first().cloned())
        .unwrap_or_default();
    (path, used_override)
}

pub fn detect(def: &ClientDef) -> Detection {
    let preferences = preferences();
    let preference_error = preferences.as_ref().err().cloned();
    let override_path = preferences
        .ok()
        .and_then(|mut preferences| preferences.paths.remove(def.id));
    let paths = candidates(def);
    let override_path = override_path.map(PathBuf::from);
    // 失效的手动路径不能盖住仍然存在的默认配置。只有覆盖路径真的存在时才优先，
    // 否则先采用自动发现结果；两者都不存在时仍保留手写路径供用户修正。
    let (path, used_override) = choose_path(
        override_path,
        &paths,
        if def.id == "opencode" { 1 } else { 0 },
    );
    let mut evidence = Vec::new();
    if path.is_file() {
        evidence.push(format!("发现配置文件: {}", path.display()));
    }
    if def.id == "gemini-cli" && path.with_file_name(".env").is_file() {
        evidence.push(format!(
            "发现环境配置文件: {}",
            path.with_file_name(".env").display()
        ));
    }
    if used_override {
        evidence.push("使用手动指定的配置路径".into());
    }
    let binary = find_binary(def);
    if let Some(ref binary) = binary {
        evidence.push(format!("发现客户端程序: {}", binary.display()));
    }
    let app = app_path(def);
    if let Some(app) = app {
        evidence.push(format!("发现桌面客户端: {}", app.display()));
    }
    let mut unsupported = preference_error.is_some();
    let mut note = preference_error;
    if def.id == "crush" {
        let dir = path.parent().unwrap_or(Path::new("."));
        if dir.join("crushrc").is_file() || dir.join(".crushrc").is_file() {
            unsupported = true;
            note = Some(
                "发现优先级更高的 crushrc；本页只管理 JSON，请在 Crush 中编辑实际配置。".into(),
            );
        }
    }
    let version = package_version(binary.as_deref(), def).unwrap_or_default();
    if def.id == "opencode"
        && version
            .split('.')
            .next()
            .and_then(|major| major.parse::<u32>().ok())
            .is_some_and(|major| major >= 2)
    {
        unsupported = true;
        note = Some("检测到 OpenCode v2；当前仅适配 v1 provider 格式，暂不开放写入。".into());
    }
    Detection {
        detected: !evidence.is_empty(),
        path,
        evidence,
        version,
        unsupported,
        note,
    }
}

fn find_binary(def: &ClientDef) -> Option<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut dirs: Vec<PathBuf> = std::env::split_paths(&path).collect();
    let home = home();
    dirs.extend([
        home.join(".local/bin"),
        home.join(".npm-global/bin"),
        home.join(".bun/bin"),
    ]);
    if let Some(appdata) = env_dir("APPDATA") {
        dirs.push(appdata.join("npm"));
    }
    if let Some(local) = env_dir("LOCALAPPDATA") {
        dirs.push(local.join("Microsoft/WinGet/Links"));
    }
    let extensions: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into())
            .split(';')
            .map(str::to_ascii_lowercase)
            .collect()
    } else {
        Vec::new()
    };
    for name in def.binaries {
        for dir in &dirs {
            let plain = dir.join(name);
            if plain.is_file()
                && (*name != "agent"
                    || plain
                        .to_string_lossy()
                        .to_ascii_lowercase()
                        .contains("cursor"))
            {
                return Some(plain);
            }
            for ext in &extensions {
                let path = dir.join(format!("{name}{ext}"));
                if path.is_file()
                    && (*name != "agent"
                        || path
                            .to_string_lossy()
                            .to_ascii_lowercase()
                            .contains("cursor"))
                {
                    return Some(path);
                }
            }
        }
    }
    None
}

fn app_path(def: &ClientDef) -> Option<PathBuf> {
    let mut paths = Vec::new();
    if cfg!(target_os = "macos") {
        for name in match def.id {
            "zcode" => vec!["ZCode"],
            "workbuddy" => vec!["WorkBuddy", "CodeBuddy"],
            "codex" => vec!["Codex"],
            "zed" => vec!["Zed"],
            _ => vec![],
        } {
            paths.push(PathBuf::from(format!("/Applications/{name}.app")));
            paths.push(home().join(format!("Applications/{name}.app")));
        }
    }
    if cfg!(windows) {
        if let Some(local) = env_dir("LOCALAPPDATA") {
            for (folder, exe) in match def.id {
                "zcode" => vec![("ZCode", "ZCode.exe")],
                "workbuddy" => vec![
                    ("WorkBuddy", "WorkBuddy.exe"),
                    ("CodeBuddy", "CodeBuddy.exe"),
                ],
                "zed" => vec![("Zed", "Zed.exe")],
                _ => vec![],
            } {
                paths.push(local.join("Programs").join(folder).join(exe));
                paths.push(local.join(folder).join(exe));
            }
        }
    }
    paths.into_iter().find(|path| {
        path.is_file() || path.extension().is_some_and(|ext| ext == "app") && path.is_dir()
    })
}

fn package_version(binary: Option<&Path>, def: &ClientDef) -> Option<String> {
    let package = match def.id {
        "claude-code" => "@anthropic-ai/claude-code",
        "codex" => "@openai/codex",
        "gemini-cli" => "@google/gemini-cli",
        "qoder" => "@qoder-ai/qodercli",
        "cline" => "cline",
        "opencode" => "opencode-ai",
        "kimi-code" => "@moonshot-ai/kimi-code",
        "dsh" => "@deepseek-ai/dsh",
        _ => return None,
    };
    let parent = binary?.parent()?;
    for path in [
        parent
            .join("node_modules")
            .join(package)
            .join("package.json"),
        parent
            .join("../lib/node_modules")
            .join(package)
            .join("package.json"),
    ] {
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            continue;
        };
        if let Some(version) = value.get("version").and_then(serde_json::Value::as_str) {
            return Some(version.into());
        }
    }
    None
}

pub fn kimi_legacy(path: &Path) -> bool {
    path.parent()
        .and_then(Path::file_name)
        .is_some_and(|name| name == ".kimi")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static SEQ: AtomicUsize = AtomicUsize::new(0);

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "agent2api-agent-paths-{}-{}-{tag}",
                std::process::id(),
                SEQ.fetch_add(1, Ordering::SeqCst)
            ));
            std::fs::create_dir_all(&path).expect("临时目录应当能创建");
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_missing_override_does_not_hide_an_existing_auto_path() {
        let dir = TempDir::new("override");
        let auto = dir.0.join("cordis.patch.yml");
        std::fs::write(&auto, "[]\n").unwrap();
        let missing = dir.0.join("settings.yaml");
        let (path, manual) = choose_path(Some(missing), &[auto.clone()], 0);
        assert_eq!(auto, path);
        assert!(!manual);
    }

    #[test]
    fn an_existing_override_stays_authoritative() {
        let dir = TempDir::new("manual");
        let auto = dir.0.join("auto.yml");
        let manual = dir.0.join("manual.yml");
        std::fs::write(&auto, "[]\n").unwrap();
        std::fs::write(&manual, "[]\n").unwrap();
        let (path, used_manual) = choose_path(Some(manual.clone()), &[auto], 0);
        assert_eq!(manual, path);
        assert!(used_manual);
    }

    #[test]
    fn dsh_discovery_prefers_the_desktop_profile_patch() {
        let dir = TempDir::new("dsh");
        std::fs::create_dir_all(dir.0.join("profiles/custom")).unwrap();
        std::fs::create_dir_all(dir.0.join("profiles/desktop")).unwrap();
        let candidates = dsh_candidates(&dir.0);
        assert_eq!(
            Some(&dir.0.join("profiles/desktop/cordis.patch.yml")),
            candidates.first()
        );
        assert!(candidates.contains(&dir.0.join("profiles/custom/cordis.patch.yml")));
        assert_eq!(Some(&dir.0.join("settings.yaml")), candidates.last());
    }
}
