//! Data layer for the Yunxiao task dashboard.
//!
//! No UI dependency: the GUI crate only renders what this crate produces.

pub mod config;
pub mod filter;
pub mod model;
pub mod yunxiao;

pub use config::{ConfigError, Settings};
pub use filter::{Filter, StatusGroup, TimeWindow};
pub use model::{tree_children, tree_rows, Item, Relation, StatusTone, Store, TreeRow};
pub use yunxiao::{ApiError, Client, PageMeta, RawItem, RelationRecord};

/// Fetch every configured project/category and build the relation index.
///
/// `on_page` fires after each API page so a caller can show progress.
pub fn load_store(
    settings: &Settings,
    on_page: impl FnMut(usize),
) -> Result<Store, ApiError> {
    let client = Client::new(settings);
    let raw = client.sync(
        settings.spaces(),
        settings.categories(),
        settings.page_size(),
        on_page,
    )?;
    let items: Vec<Item> = raw
        .into_iter()
        .filter_map(|(category, raw)| Item::from_raw(&category, raw, settings))
        .collect();
    Ok(Store::new(items, now_secs()))
}

/// Fetch one view: the given projects × a single category, optionally only one
/// assignee (filtered server-side). This is what the GUI loads on demand.
pub fn load_view(
    settings: &Settings,
    spaces: &[String],
    category: &str,
    assignee: Option<&str>,
    on_page: impl FnMut(usize),
) -> Result<Store, ApiError> {
    let client = Client::new(settings);
    let raw = client.sync_view(spaces, category, assignee, settings.page_size(), on_page)?;
    let items: Vec<Item> = raw
        .into_iter()
        .filter_map(|raw| Item::from_raw(category, raw, settings))
        .collect();
    Ok(Store::new(items, now_secs()))
}

/// 单条工作项的描述。任务的 `description` 是 Markdown，需求/缺陷是
/// `{"htmlValue": "<article>…"}`，后者先转成纯文本。
/// 搜索结果不带描述，详情面板选中时现拉。
pub fn load_description(settings: &Settings, item_id: &str) -> Result<String, ApiError> {
    let client = Client::new(settings);
    let raw = client.get_item(item_id)?;
    Ok(plain_description(&raw.description.unwrap_or_default()))
}

/// 工作项的关联需求 / 关联任务（`ASSOCIATED`，双向）。
pub fn load_relations(
    settings: &Settings,
    item_id: &str,
) -> Result<Vec<yunxiao::RelationRecord>, ApiError> {
    let client = Client::new(settings);
    client.relation_records(item_id, "ASSOCIATED")
}

/// 真实子项 / 父项（`relationType=SUB` / `PARENT`），跨项目、跨类别都有。
///
/// 关系记录只给 id 和 `resourceType`，所以逐条取详情；单条失败就跳过，
/// 不让一个坏节点拖垮整棵子树。
pub fn load_related_items(
    settings: &Settings,
    item_id: &str,
    relation_type: &str,
) -> Result<Vec<Item>, ApiError> {
    let client = Client::new(settings);
    let records = client.relation_records(item_id, relation_type)?;
    Ok(records
        .iter()
        .filter_map(|record| {
            client
                .get_item(&record.resource_id)
                .ok()
                .and_then(|raw| Item::from_raw(&record.resource_type, raw, settings))
        })
        .collect())
}

/// 按 id 取当前加载范围之外的工作项（比如其他项目下的父需求）。
/// 不带任何项目/类别条件，只用于展示。
/// `category` 取自关系记录的 `resourceType`，决定浏览器链接里的类别段。
pub fn load_foreign_item(
    settings: &Settings,
    item_id: &str,
    category: Option<&str>,
) -> Result<Item, ApiError> {
    let client = Client::new(settings);
    let raw = client.get_item(item_id)?;
    Item::from_raw(category.unwrap_or(""), raw, settings)
        .ok_or_else(|| ApiError::Parse("工作项缺少 id".to_string()))
}

fn plain_description(raw: &str) -> String {
    let raw = raw.trim();
    if !raw.starts_with('{') {
        return raw.to_string();
    }
    let html = serde_json::from_str::<serde_json::Value>(raw)
        .ok()
        .and_then(|value| value.get("htmlValue")?.as_str().map(str::to_string));
    match html {
        Some(html) => html_to_text(&html),
        None => String::new(),
    }
}

/// 够用的 HTML → 文本：块级标签换行、`<li>` 变 `- `、去掉其余标签、解常见实体。
fn html_to_text(html: &str) -> String {
    let mut text = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(open) = rest.find('<') {
        text.push_str(&rest[..open]);
        let Some(close) = rest[open..].find('>') else {
            rest = &rest[open..];
            break;
        };
        let tag = &rest[open + 1..open + close];
        let name: String = tag
            .trim_start_matches('/')
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        if name == "li" {
            if tag.starts_with('/') {
                text.push('\n');
            } else {
                text.push_str("\n- ");
            }
        } else if matches!(
            name.as_str(),
            "p" | "div" | "article" | "section" | "br" | "tr" | "h1" | "h2" | "h3" | "h4"
        ) {
            text.push('\n');
        }
        rest = &rest[open + close + 1..];
    }
    text.push_str(rest);

    let decoded = text
        .replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&");

    decoded
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Project names for the configured ids, sorted by name. One tiny request per project.
pub fn load_spaces(settings: &Settings) -> Result<Vec<(String, String)>, ApiError> {
    let client = Client::new(settings);
    let mut out = Vec::new();
    for id in settings.spaces() {
        let name = client
            .project(id)?
            .label()
            .map(str::to_string)
            .unwrap_or_else(|| id.clone());
        out.push((name, id.clone()));
    }
    out.sort();
    Ok(out)
}

pub fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{html_to_text, plain_description};

    #[test]
    fn html_description_becomes_plain_text() {
        let raw = r#"{"htmlValue":"<article><h2>背景</h2><p>原型&nbsp;链接</p><ul><li>渠道</li><li>模板</li></ul></article>","jsonMLValue":[]}"#;
        assert_eq!(plain_description(raw), "背景\n原型 链接\n- 渠道\n- 模板");
    }

    #[test]
    fn markdown_description_is_left_alone() {
        assert_eq!(plain_description("## 背景\n\n正文"), "## 背景\n\n正文");
    }

    #[test]
    fn html_to_text_handles_unclosed_tag_tail() {
        assert_eq!(html_to_text("a <b>b"), "a b");
    }
}
