//! Yunxiao Projex REST client (work item search / detail).
//!
//! Endpoints mirror the ones the local `yunxiao-devops` skill uses:
//! `POST /oapi/v1/projex/organizations/{org}/workitems:search`,
//! `GET  /oapi/v1/projex/organizations/{org}/workitems/{id}`.
//! Auth is the `x-yunxiao-token` header; the token is never logged.

use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Value};

use crate::config::Settings;

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("HTTP {status} {path}：{message}")]
    Http {
        status: u16,
        path: String,
        message: String,
    },
    #[error("网络错误：{0}")]
    Network(String),
    #[error("响应解析失败：{0}")]
    Parse(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageMeta {
    pub page: usize,
    pub per_page: usize,
    pub total: usize,
    pub total_pages: usize,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Named {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default, rename = "displayName")]
    pub display_name: Option<String>,
}

impl Named {
    pub fn label(&self) -> Option<&str> {
        self.display_name.as_deref().or(self.name.as_deref())
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct FieldValue {
    #[serde(default)]
    pub identifier: Option<String>,
    #[serde(default, rename = "displayValue")]
    pub display_value: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct CustomField {
    #[serde(default, rename = "fieldId")]
    pub field_id: Option<String>,
    #[serde(default, rename = "fieldName")]
    pub field_name: Option<String>,
    #[serde(default)]
    pub values: Vec<FieldValue>,
}

impl CustomField {
    /// First non-empty value, preferring the human-readable one.
    pub fn value(&self) -> Option<&str> {
        self.values
            .iter()
            .find_map(|v| v.display_value.as_deref().or(v.identifier.as_deref()))
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct RelationRecord {
    #[serde(default, rename = "relationType")]
    pub relation_type: String,
    #[serde(default, rename = "resourceType")]
    pub resource_type: String,
    #[serde(default, rename = "resourceId")]
    pub resource_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawItem {
    #[serde(default)]
    pub id: String,
    #[serde(default, rename = "serialNumber")]
    pub serial_number: Option<String>,
    #[serde(default)]
    pub subject: Option<String>,
    /// 只有 `GET /workitems/{id}` 会返回，搜索结果里恒为空。
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default, rename = "parentId")]
    pub parent_id: Option<String>,
    #[serde(default, rename = "idPath")]
    pub id_path: Option<String>,
    #[serde(default)]
    pub status: Option<Named>,
    #[serde(default, rename = "workitemType")]
    pub workitem_type: Option<Named>,
    #[serde(default)]
    pub space: Option<Named>,
    #[serde(default, rename = "assignedTo")]
    pub assigned_to: Option<Named>,
    #[serde(default)]
    pub sprint: Option<Named>,
    #[serde(default, rename = "gmtCreate")]
    pub gmt_create: Option<i64>,
    #[serde(default, rename = "gmtModified")]
    pub gmt_modified: Option<i64>,
    #[serde(default, rename = "customFieldValues")]
    pub custom_fields: Vec<CustomField>,
}

pub struct Client {
    agent: ureq::Agent,
    base_url: String,
    org: String,
    token: String,
}

impl Client {
    pub fn new(settings: &Settings) -> Self {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(10))
            .timeout(Duration::from_secs(45))
            .user_agent("taskdash/0.1")
            .build();
        Self {
            agent,
            base_url: settings.base_url().to_string(),
            org: settings.org().to_string(),
            token: settings.token().to_string(),
        }
    }

    fn path(&self, suffix: &str) -> String {
        format!(
            "{}/oapi/v1/projex/organizations/{}/{suffix}",
            self.base_url, self.org
        )
    }

    /// One page of work items in a project. `page` is 1-based.
    ///
    /// `assignee` is pushed to the server (`conditions`), so only that person's items come back.
    pub fn search_page(
        &self,
        space_id: &str,
        category: &str,
        assignee: Option<&str>,
        page: usize,
        per_page: usize,
    ) -> Result<(Vec<RawItem>, PageMeta), ApiError> {
        let path = self.path("workitems:search");
        let mut body = json!({
            "category": category,
            "spaceId": space_id,
            "spaceType": "Project",
            "orderBy": "gmtCreate",
            "sort": "desc",
            "page": page,
            "perPage": per_page,
        });
        if let Some(user) = assignee {
            // 服务端只认 JSON 字符串形式的 conditions。
            body["conditions"] = json!(json!({
                "conditionGroups": [[{
                    "fieldIdentifier": "assignedTo",
                    "operator": "CONTAINS",
                    "value": [user],
                    "toValue": null,
                    "className": "user",
                    "format": "list",
                }]]
            })
            .to_string());
        }
        let request = self
            .agent
            .post(&path)
            .set("x-yunxiao-token", &self.token)
            .set("Accept", "application/json");
        let resp = match request.send_json(body) {
            Ok(resp) => resp,
            Err(ureq::Error::Status(status, resp)) => {
                let text = resp.into_string().unwrap_or_default();
                return Err(ApiError::Http {
                    status,
                    path,
                    message: error_message(&text),
                });
            }
            Err(ureq::Error::Transport(err)) => return Err(ApiError::Network(err.to_string())),
        };

        let header_int = |name: &str| -> Option<usize> {
            resp.header(name).and_then(|v| v.trim().parse().ok())
        };
        let total_pages = header_int("x-total-pages");
        let total = header_int("x-total");

        let value = resp
            .into_json::<Value>()
            .map_err(|e| ApiError::Parse(e.to_string()))?;
        let items: Vec<RawItem> = match unwrap_result(value) {
            Value::Array(items) => {
                serde_json::from_value(Value::Array(items)).map_err(|e| ApiError::Parse(e.to_string()))?
            }
            Value::Null => Vec::new(),
            other => {
                return Err(ApiError::Parse(format!(
                    "工作项搜索结果结构未知：{}",
                    truncate(&other.to_string(), 120)
                )))
            }
        };

        let meta = PageMeta {
            page,
            per_page,
            total: total.unwrap_or(items.len()),
            total_pages: total_pages
                .unwrap_or_else(|| total.unwrap_or(items.len()).div_ceil(per_page).max(1)),
        };
        Ok((items, meta))
    }

    pub fn get_item(&self, item_id: &str) -> Result<RawItem, ApiError> {
        let path = self.path(&format!("workitems/{item_id}"));
        let value = self.get_value(path)?;
        serde_json::from_value(unwrap_result(value)).map_err(|e| ApiError::Parse(e.to_string()))
    }

    /// 工作项关联项。`relation_type` 实测支持 `ASSOCIATED` / `PARENT` / `SUB`。
    /// 关联是双向的：任务关联了需求，从需求上也能查到所有关联任务。
    pub fn relation_records(
        &self,
        item_id: &str,
        relation_type: &str,
    ) -> Result<Vec<RelationRecord>, ApiError> {
        let path = self.path(&format!(
            "workitems/{item_id}/relationRecords?relationType={relation_type}"
        ));
        let value = self.get_value(path)?;
        serde_json::from_value(unwrap_result(value)).map_err(|e| ApiError::Parse(e.to_string()))
    }

    /// 项目名，供项目下拉使用（配置里只有 ID）。
    pub fn project(&self, space_id: &str) -> Result<Named, ApiError> {        let path = self.path(&format!("projects/{space_id}"));
        let value = self.get_value(path)?;
        serde_json::from_value(unwrap_result(value)).map_err(|e| ApiError::Parse(e.to_string()))
    }

    fn get_value(&self, path: String) -> Result<Value, ApiError> {
        let resp = match self
            .agent
            .get(&path)
            .set("x-yunxiao-token", &self.token)
            .set("Accept", "application/json")
            .call()
        {
            Ok(resp) => resp,
            Err(ureq::Error::Status(status, resp)) => {
                let text = resp.into_string().unwrap_or_default();
                return Err(ApiError::Http {
                    status,
                    path,
                    message: error_message(&text),
                });
            }
            Err(ureq::Error::Transport(err)) => return Err(ApiError::Network(err.to_string())),
        };
        resp.into_json::<Value>()
            .map_err(|e| ApiError::Parse(e.to_string()))
    }

    /// 拉取「若干项目 × 单个类别」的全部工作项，`assignee` 下推到服务端。
    pub fn sync_view(
        &self,
        spaces: &[String],
        category: &str,
        assignee: Option<&str>,
        per_page: usize,
        mut on_page: impl FnMut(usize),
    ) -> Result<Vec<RawItem>, ApiError> {
        let mut out = Vec::new();
        let mut pages = 0usize;
        for space in spaces {
            let mut page = 1usize;
            loop {
                let (items, meta) = self.search_page(space, category, assignee, page, per_page)?;
                let count = items.len();
                out.extend(items);
                pages += 1;
                on_page(pages);
                if count == 0 || page >= meta.total_pages {
                    break;
                }
                page += 1;
            }
        }
        Ok(out)
    }

    /// Fetch every configured project/category. `on_page` is called after each page.
    pub fn sync(
        &self,
        spaces: &[String],
        categories: &[String],
        per_page: usize,
        mut on_page: impl FnMut(usize),
    ) -> Result<Vec<(String, RawItem)>, ApiError> {
        let mut out = Vec::new();
        for category in categories {
            for item in self.sync_view(spaces, category, None, per_page, &mut on_page)? {
                out.push((category.clone(), item));
            }
        }
        Ok(out)
    }
}

fn unwrap_result(value: Value) -> Value {
    match value {
        Value::Object(mut map) => match map.remove("result") {
            Some(inner) => inner,
            None => Value::Object(map),
        },
        other => other,
    }
}

fn error_message(body: &str) -> String {
    if let Ok(value) = serde_json::from_str::<Value>(body) {
        for key in ["errorMessage", "errorDescription", "message"] {
            if let Some(msg) = value.get(key).and_then(Value::as_str) {
                if !msg.is_empty() {
                    return msg.to_string();
                }
            }
        }
    }
    truncate(body, 200)
}

fn truncate(text: &str, max: usize) -> String {
    let cleaned = text.replace('\n', " ");
    if cleaned.chars().count() <= max {
        cleaned
    } else {
        cleaned.chars().take(max).collect::<String>() + "…"
    }
}
