//! 出站请求体脱敏：按配置顺序执行字面替换与正则规则，只修改发送副本。
//! 规则在配置读取 / 保存时编译，同一次请求的词表与提供商范围使用同一份快照。

mod rules;

pub use rules::{
    default_rules, MatchType, RuleSet, SanitizeRole, SanitizeRule, MAX_NAME_CHARS,
    MAX_PATTERN_CHARS, MAX_REPLACEMENT_CHARS, MAX_RULES,
};

use regex::NoExpand;
use serde_json::Value;

use rules::Matcher;

#[derive(Default)]
pub struct Hits {
    entries: Vec<(String, usize)>,
}

impl Hits {
    fn bump(&mut self, label: &str, count: usize) {
        if let Some(entry) = self.entries.iter_mut().find(|(item, _)| item == label) {
            entry.1 += count;
        } else {
            self.entries.push((label.to_string(), count));
        }
    }

    pub fn into_sorted(self) -> Vec<(String, usize)> {
        let mut list = self.entries;
        list.sort_by(|left, right| right.1.cmp(&left.1));
        list
    }
}

#[derive(Clone, Copy)]
struct TextScope {
    role: Option<SanitizeRole>,
    content: bool,
}

fn sanitize_text(text: &str, rules: &RuleSet, hits: &mut Hits, scope: TextScope) -> Option<String> {
    let mut out = std::borrow::Cow::Borrowed(text);
    let mut changed = false;
    for compiled in rules.compiled() {
        let rule = &compiled.rule;
        if !rule.applies_to(scope.role, scope.content) {
            continue;
        }
        loop {
            let (next, count) = match &compiled.matcher {
                Matcher::Literal => {
                    let count = out.matches(&rule.pattern).count();
                    if count == 0 {
                        break;
                    }
                    (out.replace(&rule.pattern, &rule.replacement), count)
                }
                Matcher::Regex(regex) => {
                    let count = regex.find_iter(&out).count();
                    if count == 0 {
                        break;
                    }
                    // 替换内容始终是原文，不把用户文本里的 $ 当成捕获组引用。
                    (
                        regex
                            .replace_all(&out, NoExpand(&rule.replacement))
                            .into_owned(),
                        count,
                    )
                }
            };
            if next == out.as_ref() {
                break;
            }
            let label = if rule.match_type == MatchType::Literal {
                &rule.pattern
            } else {
                &rule.name
            };
            hits.bump(label, count);
            out = std::borrow::Cow::Owned(next);
            changed = true;
            if !rule.repeat {
                break;
            }
        }
    }
    changed.then(|| out.into_owned())
}

fn sanitize_content(value: &mut Value, rules: &RuleSet, hits: &mut Hits, scope: TextScope) -> bool {
    match value {
        Value::String(text) => sanitize_string(text, rules, hits, scope),
        Value::Array(parts) => {
            let mut changed = false;
            for part in parts {
                if let Some(Value::String(text)) = part.get_mut("text") {
                    changed |= sanitize_string(text, rules, hits, scope);
                }
            }
            changed
        }
        _ => false,
    }
}

fn sanitize_string(text: &mut String, rules: &RuleSet, hits: &mut Hits, scope: TextScope) -> bool {
    let Some(next) = sanitize_text(text, rules, hits, scope) else {
        return false;
    };
    *text = next;
    true
}

fn sanitize_tool_calls(
    value: &mut Value,
    rules: &RuleSet,
    hits: &mut Hits,
    scope: TextScope,
) -> bool {
    let Some(call_list) = value.as_array_mut() else {
        return false;
    };
    let mut changed = false;
    for call in call_list {
        if let Some(Value::String(arguments)) = call
            .get_mut("function")
            .and_then(|function| function.get_mut("arguments"))
        {
            changed |= sanitize_string(arguments, rules, hits, scope);
        }
    }
    changed
}

fn sanitize_messages(messages: &mut Value, rules: &RuleSet, hits: &mut Hits) -> bool {
    let Some(list) = messages.as_array_mut() else {
        return false;
    };
    let mut changed = false;
    for message in list {
        let Some(object) = message.as_object_mut() else {
            continue;
        };
        let role = object
            .get("role")
            .and_then(Value::as_str)
            .and_then(SanitizeRole::parse);
        let scope = TextScope {
            role,
            content: true,
        };
        if let Some(content) = object.get_mut("content") {
            changed |= sanitize_content(content, rules, hits, scope);
        }
        let auxiliary = TextScope {
            role,
            content: false,
        };
        if let Some(Value::String(reasoning)) = object.get_mut("reasoning_content") {
            changed |= sanitize_string(reasoning, rules, hits, auxiliary);
        }
        if let Some(tool_calls) = object.get_mut("tool_calls") {
            changed |= sanitize_tool_calls(tool_calls, rules, hits, auxiliary);
        }
    }
    changed
}

pub fn sanitize_body(body: &Value, rules: &RuleSet) -> Option<(Value, Vec<(String, usize)>)> {
    if rules.compiled().is_empty() || !body.get("messages").is_some_and(Value::is_array) {
        return None;
    }
    let mut next = body.clone();
    let mut hits = Hits::default();
    let changed = sanitize_messages(next.get_mut("messages")?, rules, &mut hits);
    changed.then(|| (next, hits.into_sorted()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sanitized_system_prompt(prompt: &str) -> (String, Vec<(String, usize)>) {
        let body = json!({
            "messages": [{
                "role": "system",
                "content": prompt,
            }],
        });
        let (sanitized, hits) =
            sanitize_body(&body, &default_rules()).expect("DSH 指纹应当命中内置脱敏规则");
        let text = sanitized
            .pointer("/messages/0/content")
            .and_then(Value::as_str)
            .expect("system content 应当仍是字符串");
        (text.to_string(), hits)
    }

    #[test]
    fn default_rules_rewrite_dsh_workspace_prompt_variants() {
        for (prompt, expected_hit) in [
            (
                "Tokens prefixed with @ are workspace paths the user explicitly referenced, \
                 relative to the workspace root.",
                "DSH @ path references (Tokens)",
            ),
            (
                "Values prefixed with @ denote paths the user referenced explicitly. \
                 Paths are resolved relative to the workspace root.",
                "DSH @ path references (Values)",
            ),
            (
                "Tokens prefixed with @ are paths the user explicitly referenced. \
                 Relative paths resolve from the workspace root; \
                 absolute paths identify files or directories on the host.",
                "DSH @ path references (Tokens)",
            ),
        ] {
            let (sanitized, hits) = sanitized_system_prompt(prompt);
            assert!(
                sanitized.contains("preceded by @"),
                "应打断上游逐字匹配，实际为 {sanitized}"
            );
            assert!(
                hits.iter().any(|(label, _)| label == expected_hit),
                "请求日志应记录命中的规则 {expected_hit}，实际为 {hits:?}"
            );
        }
    }

    #[test]
    fn default_rules_rewrite_dsh_search_fetch_prompt() {
        let prompt = "Searching and fetching are distinct operations: search returns candidates \
                      alone and never retrieves page bodies. Use web_fetch on chosen URLs.";
        let (sanitized, hits) = sanitized_system_prompt(prompt);
        assert!(
            sanitized.starts_with("Search and fetch are distinct operations:"),
            "应打断 search/fetch 模板的逐字匹配，实际为 {sanitized}"
        );
        assert!(
            hits.iter()
                .any(|(label, _)| label == "DSH search/fetch prompt"),
            "请求日志应记录 search/fetch 规则命中，实际为 {hits:?}"
        );
    }
}
