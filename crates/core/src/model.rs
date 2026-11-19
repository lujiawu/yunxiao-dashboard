//! Work item model and the parent/child/sibling index built from one sync.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use crate::config::Settings;
use crate::yunxiao::{CustomField, RawItem};

/// 状态点的配色分组，构造工作项时按配置的关键词算好。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusTone {
    Info,
    Warning,
    Success,
}

/// Yunxiao sends this sentinel instead of `null` for "no parent".
const NO_PARENT: &str = "EMPTY_VALUE";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Relation {
    Parent,
    Child,
    Sibling,
}

impl Relation {
    pub fn label(self) -> &'static str {
        match self {
            Relation::Parent => "父项",
            Relation::Child => "子项",
            Relation::Sibling => "同级",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Item {
    pub id: String,
    pub serial_number: String,
    pub subject: String,
    /// Markdown；搜索结果里为空，详情面板按需单独拉。
    pub description: String,
    pub status: String,
    pub status_id: String,
    pub workitem_type: String,
    pub category: String,
    pub space_id: String,
    pub space_name: String,
    pub assignee_id: String,
    pub assignee: String,
    pub parent_id: Option<String>,
    pub id_path: Vec<String>,
    pub sprint: String,
    pub priority: String,
    pub planned_hours: Option<String>,
    pub actual_hours: Option<String>,
    pub plan_start: Option<String>,
    pub plan_end: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub(crate) done: bool,
    pub tone: StatusTone,
}

impl Item {
    pub fn from_raw(category: &str, raw: RawItem, settings: &Settings) -> Option<Self> {
        let id = raw.id.trim().to_string();
        if id.is_empty() {
            return None;
        }
        let parent_id = raw
            .parent_id
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty() && p != NO_PARENT);
        let id_path: Vec<String> = raw
            .id_path
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty() && *s != NO_PARENT)
            .map(str::to_string)
            .collect();
        let space_id = raw.space.as_ref().and_then(|s| s.id.clone()).unwrap_or_default();
        let category = category.to_string();
        let status = raw
            .status
            .as_ref()
            .and_then(|s| s.label().map(str::to_string))
            .unwrap_or_default();
        let done = settings
            .done_statuses()
            .iter()
            .any(|hint| status.contains(hint));
        let tone = if settings
            .warning_statuses()
            .iter()
            .any(|hint| status.contains(hint))
        {
            StatusTone::Warning
        } else if settings
            .info_statuses()
            .iter()
            .any(|hint| status.contains(hint))
        {
            StatusTone::Info
        } else {
            StatusTone::Success
        };

        Some(Self {
            serial_number: raw.serial_number.unwrap_or_default(),
            subject: raw.subject.unwrap_or_default(),
            description: raw.description.unwrap_or_default(),
            status,
            status_id: raw.status.as_ref().and_then(|s| s.id.clone()).unwrap_or_default(),
            workitem_type: raw
                .workitem_type
                .as_ref()
                .and_then(|s| s.label().map(str::to_string))
                .unwrap_or_default(),
            space_name: raw
                .space
                .as_ref()
                .and_then(|s| s.label().map(str::to_string))
                .unwrap_or_default(),
            space_id,
            category,
            assignee_id: raw
                .assigned_to
                .as_ref()
                .and_then(|a| a.id.clone())
                .unwrap_or_default(),
            assignee: raw
                .assigned_to
                .as_ref()
                .and_then(|a| a.label().map(str::to_string))
                .unwrap_or_default(),
            sprint: raw
                .sprint
                .as_ref()
                .and_then(|s| s.label().map(str::to_string))
                .unwrap_or_default(),
            priority: field(&raw.custom_fields, &settings.field_labels().priority).unwrap_or_default(),
            planned_hours: field(&raw.custom_fields, &settings.field_labels().planned_hours).filter(|v| !v.is_empty()),
            actual_hours: field(&raw.custom_fields, &settings.field_labels().actual_hours).filter(|v| !v.is_empty()),
            plan_start: field(&raw.custom_fields, &settings.field_labels().plan_start).filter(|v| !v.is_empty()),
            plan_end: field(&raw.custom_fields, &settings.field_labels().plan_end).filter(|v| !v.is_empty()),
            created_at: raw.gmt_create.unwrap_or_default(),
            updated_at: raw.gmt_modified.unwrap_or_default(),
            parent_id,
            id_path,
            id,
            done,
            tone,
        })
    }

    /// Browser link for this work item.
    pub fn url(&self) -> String {
        let category = self.category.to_lowercase();
        format!(
            "https://devops.aliyun.com/projex/project/{}/{}/{}",
            self.space_id, category, self.id
        )
    }

    pub fn is_done(&self) -> bool {
        self.done
    }

    /// `ABC-835` → 835, used for stable numeric sorting.
    pub fn serial_no(&self) -> u64 {
        self.serial_number
            .rsplit_once('-')
            .map(|(_, tail)| tail)
            .unwrap_or(&self.serial_number)
            .trim()
            .parse()
            .unwrap_or(0)
    }
}

fn field(fields: &[CustomField], labels: &[String]) -> Option<String> {
    fields
        .iter()
        .find(|f| {
            labels.iter().any(|label| {
                f.field_id.as_deref() == Some(label.as_str())
                    || f.field_name.as_deref() == Some(label.as_str())
            })
        })
        .and_then(|f| f.value().map(str::trim).map(str::to_string))
}

/// Immutable snapshot of every synced work item plus the relations between them.
#[derive(Debug, Default)]
pub struct Store {
    items: HashMap<String, Item>,
    children: HashMap<String, Vec<String>>,
    roots: Vec<String>,
    fetched_at: i64,
    last_error: Option<String>,
}

impl Store {
    pub fn new(items: Vec<Item>, fetched_at: i64) -> Self {
        let mut by_id = HashMap::with_capacity(items.len());
        let mut children: HashMap<String, Vec<String>> = HashMap::new();
        let mut roots = Vec::new();

        for item in items {
            match &item.parent_id {
                Some(parent) => children.entry(parent.clone()).or_default().push(item.id.clone()),
                None => roots.push(item.id.clone()),
            }
            by_id.insert(item.id.clone(), item);
        }
        for ids in children.values_mut() {
            ids.sort_by_key(|id| by_id.get(id).map(Item::serial_no).unwrap_or(0));
        }
        roots.sort_by_key(|id| by_id.get(id).map(Item::serial_no).unwrap_or(0));

        Self {
            items: by_id,
            children,
            roots,
            fetched_at,
            last_error: None,
        }
    }

    pub fn with_error(mut self, error: impl Into<String>) -> Self {
        self.last_error = Some(error.into());
        self
    }

    pub fn empty_with_error(error: impl Into<String>) -> Self {
        let mut store = Self::default();
        store.last_error = Some(error.into());
        store
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn fetched_at(&self) -> i64 {
        self.fetched_at
    }

    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    pub fn get(&self, id: &str) -> Option<&Item> {
        self.items.get(id)
    }

    /// Accepts either an id or a serial number (`ABC-835`).
    pub fn find(&self, needle: &str) -> Option<&Item> {
        let needle = needle.trim();
        if let Some(item) = self.items.get(needle) {
            return Some(item);
        }
        self.items
            .values()
            .find(|item| item.serial_number.eq_ignore_ascii_case(needle))
    }

    pub fn sorted_items(&self) -> Vec<&Item> {
        let mut items: Vec<&Item> = self.items.values().collect();
        items.sort_by(|a, b| {
            b.updated_at
                .cmp(&a.updated_at)
                .then_with(|| b.serial_no().cmp(&a.serial_no()))
        });
        items
    }

    pub fn children_of(&self, id: &str) -> Vec<&Item> {
        self.link_items(self.children.get(id))
    }

    pub fn parent_of(&self, id: &str) -> Option<&Item> {
        self.items
            .get(id)
            .and_then(|item| item.parent_id.as_deref())
            .and_then(|parent| self.items.get(parent))
    }

    /// Siblings share a parent; roots without a parent are siblings of each other.
    pub fn siblings_of(&self, id: &str) -> Vec<&Item> {
        let Some(item) = self.items.get(id) else {
            return Vec::new();
        };
        let pool = match &item.parent_id {
            Some(parent) => self.children.get(parent),
            None => Some(&self.roots),
        };
        self.link_items(pool)
            .into_iter()
            .filter(|sibling| sibling.id != id)
            .collect()
    }

    /// Ancestors from the top down, skipping entries that are not in the snapshot.
    pub fn ancestors_of(&self, id: &str) -> Vec<&Item> {
        let Some(item) = self.items.get(id) else {
            return Vec::new();
        };
        item.id_path
            .iter()
            .filter(|path_id| path_id.as_str() != item.id)
            .filter_map(|path_id| self.items.get(path_id))
            .collect()
    }

    /// Whole subtree below `id`, breadth first.
    pub fn descendants_of(&self, id: &str) -> Vec<&Item> {
        let mut out = Vec::new();
        let mut queue: VecDeque<&str> = self
            .children
            .get(id)
            .map(|ids| ids.iter().map(String::as_str).collect())
            .unwrap_or_default();
        while let Some(next) = queue.pop_front() {
            if let Some(item) = self.items.get(next) {
                out.push(item);
                if let Some(kids) = self.children.get(next) {
                    queue.extend(kids.iter().map(String::as_str));
                }
            }
        }
        out
    }

    pub fn done_descendants(&self, id: &str) -> usize {
        self.descendants_of(id).iter().filter(|i| i.is_done()).count()
    }

    /// Distinct projects, sorted by name.
    pub fn spaces(&self) -> Vec<(String, String)> {
        let mut seen: BTreeMap<String, String> = BTreeMap::new();
        for item in self.items.values() {
            if !item.space_id.is_empty() {
                seen.entry(item.space_name.clone())
                    .or_insert_with(|| item.space_id.clone());
            }
        }
        seen.into_iter().collect()
    }

    /// Distinct assignees as (name, id), sorted by name.
    pub fn assignees(&self) -> Vec<(String, String)> {
        let mut seen: BTreeMap<String, String> = BTreeMap::new();
        for item in self.items.values() {
            if !item.assignee.is_empty() {
                seen.entry(item.assignee.clone())
                    .or_insert_with(|| item.assignee_id.clone());
            }
        }
        seen.into_iter().collect()
    }

    pub fn categories(&self) -> Vec<String> {
        let mut set: BTreeMap<String, ()> = BTreeMap::new();
        for item in self.items.values() {
            set.insert(item.category.clone(), ());
        }
        set.into_keys().collect()
    }

    fn link_items(&self, ids: Option<&Vec<String>>) -> Vec<&Item> {
        ids.map(|ids| ids.iter().filter_map(|id| self.items.get(id)).collect())
            .unwrap_or_default()
    }
}

/// 树里的一行：工作项 id 和缩进层级（0 是根）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeRow {
    pub id: String,
    pub depth: usize,
}

/// 把一组根节点展开成带层级的行；`collapsed` 里的节点不再向下走。
/// `seen` 跨根共享：同一项挂在两个根下时只出现一次，也顺带防住关联成环。
pub fn tree_rows(
    roots: &[String],
    children: impl Fn(&str) -> Vec<String>,
    collapsed: &HashSet<String>,
) -> Vec<TreeRow> {
    fn walk(
        id: &str,
        depth: usize,
        children: &dyn Fn(&str) -> Vec<String>,
        collapsed: &HashSet<String>,
        seen: &mut HashSet<String>,
        out: &mut Vec<TreeRow>,
    ) {
        if !seen.insert(id.to_string()) {
            return;
        }
        out.push(TreeRow {
            id: id.to_string(),
            depth,
        });
        if collapsed.contains(id) {
            return;
        }
        for kid in children(id) {
            walk(&kid, depth + 1, children, collapsed, seen, out);
        }
    }

    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for root in roots {
        walk(root, 0, &children, collapsed, &mut seen, &mut out);
    }
    out
}

/// 节点在树里的子项：真实子项（SUB）+ 关联项（ASSOCIATED）。
///
/// 需求下面挂任务和缺陷，任务下面只挂缺陷，缺陷只挂子缺陷。
/// 任务之间的关联是平级关系，铺进树会把兄弟任务变成子级，所以不展开。
pub fn tree_children(node: &Item, sub: &[&Item], assoc: &[&Item]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for item in sub {
        if item.id != node.id && !out.contains(&item.id) {
            out.push(item.id.clone());
        }
    }
    for item in assoc {
        let wanted = match node.category.as_str() {
            "Task" => item.category == "Bug",
            "Bug" => false,
            // 需求，以及类别未知的范围外项：任务和缺陷都算它的子项。
            _ => item.category != "Req",
        };
        if wanted && item.id != node.id && !out.contains(&item.id) {
            out.push(item.id.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, serial: &str, parent: Option<&str>, path: &[&str]) -> Item {
        Item {
            id: id.to_string(),
            serial_number: serial.to_string(),
            subject: format!("item {serial}"),
            description: String::new(),
            status: "待处理".into(),
            status_id: "100005".into(),
            workitem_type: "任务".into(),
            category: "Task".into(),
            space_id: "space".into(),
            space_name: "项目A".into(),
            assignee_id: "u1".into(),
            assignee: "某人".into(),
            parent_id: parent.map(str::to_string),
            id_path: path.iter().map(|p| p.to_string()).collect(),
            sprint: "2.6.0".into(),
            priority: "高".into(),
            planned_hours: None,
            actual_hours: None,
            plan_start: None,
            plan_end: None,
            created_at: 0,
            updated_at: 0,
            done: false,
            tone: StatusTone::Info,
        }
    }

    fn store() -> Store {
        Store::new(
            vec![
                item("req", "ABC-800", None, &["req"]),
                item("t1", "ABC-801", Some("req"), &["req", "t1"]),
                item("t2", "ABC-802", Some("req"), &["req", "t2"]),
                item("s1", "ABC-803", Some("t1"), &["req", "t1", "s1"]),
                item("s2", "ABC-804", Some("t1"), &["req", "t1", "s2"]),
            ],
            0,
        )
    }

    #[test]
    fn children_are_sorted_by_serial() {
        let store = store();
        let ids: Vec<&str> = store
            .children_of("req")
            .iter()
            .map(|i| i.id.as_str())
            .collect();
        assert_eq!(ids, vec!["t1", "t2"]);
    }

    #[test]
    fn siblings_exclude_self_and_share_parent() {
        let store = store();
        let ids: Vec<&str> = store
            .siblings_of("t1")
            .iter()
            .map(|i| i.id.as_str())
            .collect();
        assert_eq!(ids, vec!["t2"]);
        let root_ids: Vec<&str> = store
            .siblings_of("req")
            .iter()
            .map(|i| i.id.as_str())
            .collect();
        assert!(root_ids.is_empty());
    }

    #[test]
    fn ancestors_follow_id_path() {
        let store = store();
        let ids: Vec<&str> = store
            .ancestors_of("s1")
            .iter()
            .map(|i| i.id.as_str())
            .collect();
        assert_eq!(ids, vec!["req", "t1"]);
    }

    #[test]
    fn descendants_walk_the_whole_subtree() {
        let store = store();
        let ids: Vec<&str> = store
            .descendants_of("req")
            .iter()
            .map(|i| i.id.as_str())
            .collect();
        assert_eq!(ids.len(), 4);
        assert!(ids.contains(&"s1"));
    }

    #[test]
    fn configured_statuses_and_field_names_drive_item_flags() {
        let section = serde_json::json!({
            "pat": "test-token",
            "org_id": "org",
            "project_id": "space",
            "taskdash_done_statuses": ["原型验收"],
            "taskdash_warning_statuses": ["待复核"],
            "taskdash_custom_fields": {"plan_end": ["交付日期"]}
        });
        let settings = Settings::from_section(&section).unwrap();

        let raw: RawItem = serde_json::from_str(
            r#"{
                "id": "i1",
                "serialNumber": "ABC-1",
                "status": {"displayName": "原型验收"},
                "customFieldValues": [
                    {"fieldName": "交付日期", "values": [{"displayValue": "2026-11-20 00:00:00"}]}
                ]
            }"#,
        )
        .unwrap();
        let item = Item::from_raw("Task", raw, &settings).unwrap();
        assert!(item.is_done());
        assert_eq!(item.plan_end.as_deref(), Some("2026-11-20 00:00:00"));

        let raw: RawItem =
            serde_json::from_str(r#"{"id": "i2", "status": {"displayName": "待复核"}}"#).unwrap();
        let item = Item::from_raw("Task", raw, &settings).unwrap();
        assert!(!item.is_done());
        assert_eq!(item.tone, StatusTone::Warning);
    }

    #[test]
    fn find_accepts_serial_number() {
        let store = store();
        assert_eq!(store.find("abc-803").map(|i| i.id.as_str()), Some("s1"));
        assert_eq!(store.find("t2").map(|i| i.id.as_str()), Some("t2"));
    }

    #[test]
    fn tree_children_mix_sub_and_associated_by_node_type() {
        let req = item("req", "ABC-900", None, &["req"]);
        let mut req = req;
        req.category = "Req".into();
        let sub_task = item("t1", "ABC-901", Some("req"), &["req", "t1"]);
        let assoc_task = item("t2", "XYZ-1", None, &[]);
        let mut other_req = item("req2", "ABC-902", None, &[]);
        other_req.category = "Req".into();
        let mut bug = item("b1", "ABC-903", None, &[]);
        bug.category = "Bug".into();

        let ids = tree_children(&req, &[&sub_task], &[&assoc_task, &other_req, &bug]);
        assert_eq!(ids, vec!["t1", "t2", "b1"]);

        let ids = tree_children(&sub_task, &[&bug], &[&req, &assoc_task]);
        assert_eq!(ids, vec!["b1"]);

        let ids = tree_children(&bug, &[], &[&sub_task]);
        assert!(ids.is_empty());
    }

    #[test]
    fn tree_rows_stop_at_collapsed_nodes_and_cycles() {
        let children = |id: &str| match id {
            "a" => vec!["b".to_string(), "c".to_string()],
            "b" => vec!["a".to_string(), "d".to_string()],
            _ => Vec::new(),
        };
        let roots = vec!["a".to_string()];
        let rows = tree_rows(&roots, children, &HashSet::new());
        let flat: Vec<(&str, usize)> = rows.iter().map(|r| (r.id.as_str(), r.depth)).collect();
        assert_eq!(flat, vec![("a", 0), ("b", 1), ("d", 2), ("c", 1)]);

        let collapsed = HashSet::from(["b".to_string()]);
        let rows = tree_rows(&roots, children, &collapsed);
        let flat: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(flat, vec!["a", "b", "c"]);
    }

    #[test]
    fn tree_rows_share_one_seen_set_across_roots() {
        let children = |id: &str| match id {
            "req" => vec!["t1".to_string(), "t2".to_string()],
            "t1" => vec!["bug".to_string()],
            _ => Vec::new(),
        };
        let roots = vec!["req".to_string(), "t1".to_string()];
        let rows = tree_rows(&roots, children, &HashSet::new());
        let flat: Vec<(&str, usize)> = rows.iter().map(|r| (r.id.as_str(), r.depth)).collect();
        // t1 已经作为 req 的子项出现，不再作为根重复一遍。
        assert_eq!(flat, vec![("req", 0), ("t1", 1), ("bug", 2), ("t2", 1)]);
    }
}
