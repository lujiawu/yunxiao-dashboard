//! Settings and token resolution.
//!
//! Reuses the existing `~/.config/dashboard/config.json` (`yunxiao` section) and the
//! Yunxiao token file, so the dashboard never asks for credentials of its own.
//! The token is never printed or logged.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

const DEFAULT_BASE_URL: &str = "https://openapi-rdc.aliyuncs.com";
const DEFAULT_CATEGORIES: [&str; 2] = ["Task", "Req"];
const DEFAULT_DONE_STATUSES: [&str; 3] = ["完成", "关闭", "取消"];
const DEFAULT_INFO_STATUSES: [&str; 3] = ["处理中", "开发中", "测试中"];
const DEFAULT_WARNING_STATUSES: [&str; 2] = ["待处理", "待确认"];

/// 自定义字段的可识别名称，取自配置的 `taskdash_custom_fields`。
#[derive(Debug, Clone)]
pub struct FieldLabels {
    pub priority: Vec<String>,
    pub planned_hours: Vec<String>,
    pub actual_hours: Vec<String>,
    pub plan_start: Vec<String>,
    pub plan_end: Vec<String>,
}

impl Default for FieldLabels {
    fn default() -> Self {
        Self {
            priority: vec!["priority".into(), "优先级".into()],
            planned_hours: vec!["预计工时".into()],
            actual_hours: vec!["实际工时".into()],
            plan_start: vec!["计划开始时间".into()],
            plan_end: vec!["计划完成时间".into()],
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("找不到配置文件或令牌：{0}")]
    Missing(String),
    #[error("读取 {path} 失败：{source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("{0}")]
    Invalid(String),
}

#[derive(Debug, Clone)]
pub struct Settings {
    base_url: String,
    org: String,
    spaces: Vec<String>,
    categories: Vec<String>,
    token: String,
    token_source: String,
    user_id: Option<String>,
    default_space: Option<String>,
    done_statuses: Vec<String>,
    info_statuses: Vec<String>,
    warning_statuses: Vec<String>,
    field_labels: FieldLabels,
    page_size: usize,
}

impl Settings {
    pub fn load() -> Result<Self, ConfigError> {
        let config_value = read_config()?;
        let section = config_value
            .get("yunxiao")
            .cloned()
            .unwrap_or(Value::Null);
        Self::from_section(&section)
    }

    pub(crate) fn from_section(section: &Value) -> Result<Self, ConfigError> {
        let base_url = env_string("YUNXIAO_API_BASE_URL")
            .or_else(|| section_string(&section, "api_base"))
            .or_else(|| section_string(&section, "domain").map(|d| with_scheme(&d)))
            .unwrap_or_else(|| DEFAULT_BASE_URL.to_string());
        let base_url = with_scheme(&base_url).trim_end_matches('/').to_string();

        let org = env_string("YUNXIAO_ORG_ID")
            .or_else(|| section_string(&section, "org_id"))
            .ok_or_else(|| ConfigError::Missing("云效企业 ID（yunxiao.org_id）".into()))?;

        let raw_spaces = env_string("YUNXIAO_PROJECT_ID")
            .or_else(|| section_string(&section, "project_id"))
            .ok_or_else(|| ConfigError::Missing("云效项目 ID（yunxiao.project_id）".into()))?;
        let spaces = split_list(&raw_spaces);
        if spaces.is_empty() {
            return Err(ConfigError::Invalid("云效项目 ID 为空".into()));
        }

        let categories = section_list(&section, "taskdash_categories")
            .unwrap_or_else(|| DEFAULT_CATEGORIES.iter().map(|c| c.to_string()).collect());

        let (token, token_source) = resolve_token(&section)?;
        let user_id = section_string(&section, "user_id");
        let default_space = section_string(&section, "taskdash_default_space");
        let done_statuses = status_list(&section, "taskdash_done_statuses", &DEFAULT_DONE_STATUSES);
        let info_statuses = status_list(&section, "taskdash_info_statuses", &DEFAULT_INFO_STATUSES);
        let warning_statuses =
            status_list(&section, "taskdash_warning_statuses", &DEFAULT_WARNING_STATUSES);

        Ok(Self {
            base_url,
            org,
            spaces,
            categories,
            token,
            token_source,
            user_id,
            default_space,
            done_statuses,
            info_statuses,
            warning_statuses,
            field_labels: field_labels(&section),
            page_size: 100,
        })
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn org(&self) -> &str {
        &self.org
    }

    pub fn spaces(&self) -> &[String] {
        &self.spaces
    }

    pub fn categories(&self) -> &[String] {
        &self.categories
    }

    pub fn page_size(&self) -> usize {
        self.page_size
    }

    /// The configured 云效 user id, used for the "只看我的" shortcut.
    pub fn user_id(&self) -> Option<&str> {
        self.user_id.as_deref()
    }

    /// 看板启动时默认选中的项目 ID（`yunxiao.taskdash_default_space`）。
    pub fn default_space(&self) -> Option<&str> {
        self.default_space.as_deref()
    }

    /// Where the token came from. Safe to display; the token itself never is.
    pub fn token_source(&self) -> &str {
        &self.token_source
    }

    /// 判定为「已完成」的状态关键词。
    pub fn done_statuses(&self) -> &[String] {
        &self.done_statuses
    }

    pub fn info_statuses(&self) -> &[String] {
        &self.info_statuses
    }

    pub fn warning_statuses(&self) -> &[String] {
        &self.warning_statuses
    }

    pub fn field_labels(&self) -> &FieldLabels {
        &self.field_labels
    }

    pub(crate) fn token(&self) -> &str {
        &self.token
    }
}

fn resolve_token(section: &Value) -> Result<(String, String), ConfigError> {
    if let Some(token) = env_string("YUNXIAO_TOKEN").or_else(|| env_string("YUNXIAO_ACCESS_TOKEN")) {
        return Ok((token, "环境变量".into()));
    }

    let explicit_file = env_string("YUNXIAO_TOKEN_FILE").map(PathBuf::from);
    if let Some(path) = explicit_file {
        let token = read_token_file(&path)?;
        return Ok((token, format!("{}", path.display())));
    }

    let default_file = home_dir().join(".config/opencode/secrets/yunxiao-token");
    if default_file.is_file() {
        let token = read_token_file(&default_file)?;
        return Ok((token, default_file.display().to_string()));
    }

    if let Some(token) = section_string(section, "pat") {
        return Ok((token, "dashboard config.json (yunxiao.pat)".into()));
    }

    Err(ConfigError::Missing(
        "访问令牌（YUNXIAO_TOKEN、~/.config/opencode/secrets/yunxiao-token 或 config.json 的 yunxiao.pat）"
            .into(),
    ))
}

fn read_token_file(path: &Path) -> Result<String, ConfigError> {
    let text = fs::read_to_string(path).map_err(|source| ConfigError::Io {
        path: path.display().to_string(),
        source,
    })?;
    let token = text.trim().to_string();
    if token.is_empty() {
        return Err(ConfigError::Invalid(format!(
            "令牌文件为空：{}",
            path.display()
        )));
    }
    Ok(token)
}

fn read_config() -> Result<Value, ConfigError> {
    let path = env_string("TASKDASH_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".config/dashboard/config.json"));
    if !path.is_file() {
        return Err(ConfigError::Missing(path.display().to_string()));
    }
    let text = fs::read_to_string(&path).map_err(|source| ConfigError::Io {
        path: path.display().to_string(),
        source,
    })?;
    serde_json::from_str(&text).map_err(|e| ConfigError::Invalid(format!("{} 不是合法 JSON：{e}", path.display())))
}

fn status_list(section: &Value, key: &str, defaults: &[&str]) -> Vec<String> {
    section_list(section, key)
        .unwrap_or_else(|| defaults.iter().map(|s| s.to_string()).collect())
}

fn field_labels(section: &Value) -> FieldLabels {
    let mut labels = FieldLabels::default();
    let Some(obj) = section.get("taskdash_custom_fields").filter(|v| v.is_object()) else {
        return labels;
    };
    if let Some(v) = section_list(obj, "priority") {
        labels.priority = v;
    }
    if let Some(v) = section_list(obj, "planned_hours") {
        labels.planned_hours = v;
    }
    if let Some(v) = section_list(obj, "actual_hours") {
        labels.actual_hours = v;
    }
    if let Some(v) = section_list(obj, "plan_start") {
        labels.plan_start = v;
    }
    if let Some(v) = section_list(obj, "plan_end") {
        labels.plan_end = v;
    }
    labels
}

fn section_string(section: &Value, key: &str) -> Option<String> {
    section
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn section_list(section: &Value, key: &str) -> Option<Vec<String>> {
    let list: Vec<String> = section
        .get(key)?
        .as_array()?
        .iter()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    if list.is_empty() {
        None
    } else {
        Some(list)
    }
}

fn split_list(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

fn with_scheme(value: &str) -> String {
    if value.contains("://") {
        value.to_string()
    } else {
        format!("https://{value}")
    }
}

fn env_string(key: &str) -> Option<String> {
    env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn home_dir() -> PathBuf {
    env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn taskdash_tunables_read_from_config_with_defaults() {
        let section: Value = serde_json::from_str(
            r#"{
                "taskdash_done_statuses": ["已完结", "关闭"],
                "taskdash_warning_statuses": ["待复核"],
                "taskdash_custom_fields": {"plan_end": ["交付日期"]}
            }"#,
        )
        .unwrap();

        assert_eq!(status_list(&section, "taskdash_done_statuses", &DEFAULT_DONE_STATUSES), ["已完结", "关闭"]);
        assert_eq!(status_list(&section, "taskdash_info_statuses", &DEFAULT_INFO_STATUSES), DEFAULT_INFO_STATUSES);
        assert_eq!(status_list(&section, "taskdash_warning_statuses", &DEFAULT_WARNING_STATUSES), ["待复核"]);

        let labels = field_labels(&section);
        assert_eq!(labels.plan_end, ["交付日期"]);
        assert_eq!(labels.plan_start, ["计划开始时间"]);
        assert_eq!(labels.priority, ["priority", "优先级"]);
    }
}
