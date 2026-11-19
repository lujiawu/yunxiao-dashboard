//! `taskdash` — CLI over the same data layer the GUI uses, so the hierarchy and
//! filters can be verified without a window.
//!
//! ```text
//! taskdash sync
//! taskdash list --status active --space 项目A --keyword 报告
//! taskdash show ABC-832
//! ```

use std::process::ExitCode;

use serde_json::json;

use taskdash_core::model::Relation;
use taskdash_core::{load_spaces, load_store, load_view, Filter, Item, Settings, StatusGroup, Store, TimeWindow};

/// Root-level items share the "no parent" group, so siblings can number in the hundreds.
const RELATED_LIMIT: usize = 12;

fn main() -> ExitCode {
    // Restore default SIGPIPE so `taskdash list | head` exits instead of panicking.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }

    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("taskdash: {err}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String]) -> Result<(), String> {
    let Some(command) = args.first().map(String::as_str) else {
        return Err(usage());
    };
    let flags = Flags::parse(&args[1..]);

    let settings = Settings::load().map_err(|e| e.to_string())?;

    // 项目 + 类别都给全（且不是 show）时只拉这个范围；负责人下推到服务端。
    let scope = match flags.value("space").zip(flags.value("category")) {
        Some(scope) if command != "show" => Some(scope),
        _ => None,
    };
    // 范围拉取时已解析出的项目 ID，供筛选阶段直接使用（0 项也不会反查失败）。
    let mut scoped: Option<String> = None;
    let store = match scope {
        Some((space, category)) => {
            let space_id = resolve_space_id(&settings, space)?;
            scoped = Some(space_id.clone());
            load_view(
                &settings,
                &[space_id],
                category,
                flags.value("assignee"),
                on_page(&flags),
            )
        }
        None => load_store(&settings, on_page(&flags)),
    }
    .map_err(|e| e.to_string())?;

    match command {
        "sync" | "list" => {
            if command == "list" {
                list(&store, &settings, &flags, scoped.as_deref())?;
            } else {
                println!(
                    "同步完成：{} 个工作项，{} 个项目，类别 {:?}，令牌来源 {}",
                    store.len(),
                    store.spaces().len(),
                    settings.categories(),
                    settings.token_source()
                );
            }
            Ok(())
        }
        "show" => {
            let Some(needle) = args.get(1).filter(|a| !a.starts_with("--")) else {
                return Err("show 需要工作项编号或 ID".into());
            };
            show(&store, needle, &flags)
        }
        "help" | "--help" | "-h" => {
            println!("{}", usage());
            Ok(())
        }
        other => Err(format!("未知命令 {other}\n{}", usage())),
    }
}

fn on_page(flags: &Flags) -> impl FnMut(usize) + '_ {
    move |page| {
        if flags.has("json") {
            eprintln!("[sync] 第 {page} 页");
        }
    }
}

/// 范围拉取只接受项目 ID（名字需要全量数据才能反查，这里按配置的项目现问一次）。
fn resolve_space_id(settings: &Settings, space: &str) -> Result<String, String> {
    if settings.spaces().iter().any(|id| id == space) {
        return Ok(space.to_string());
    }
    load_spaces(settings)
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|(name, _)| name == space)
        .map(|(_, id)| id)
        .ok_or_else(|| format!("没有这个项目：{space}"))
}

fn usage() -> String {
    "用法：
  taskdash sync                    拉取全部工作项并报告数量
  taskdash list [筛选] [--json]     列出过滤后的工作项
  taskdash show <编号|ID> [--json]  查看单个工作项及其父/子/兄弟

筛选：--space <项目名或ID>  --category <Task|Req|Bug>  --status <active|done|all>
      --window <week|month|all>（按计划完成时间，本地过滤）
      --assignee <姓名或ID>  --keyword <关键词>

--space 与 --category 同时给出时只拉这个范围（快，≈1 s）；否则全量（≈25 s）。
范围拉取时 --assignee 只认 user_id（下推到服务端），姓名需要全量数据才能反查。".into()
}

fn list(
    store: &Store,
    settings: &Settings,
    flags: &Flags,
    scoped: Option<&str>,
) -> Result<(), String> {
    let filter = build_filter(store, flags, scoped)?;
    let items = filter.apply(store);
    let as_json = flags.has("json");

    if as_json {
        let rows: Vec<_> = items
            .iter()
            .map(|item| {
                json!({
                    "serialNumber": item.serial_number,
                    "subject": item.subject,
                    "status": item.status,
                    "category": item.category,
                    "space": item.space_name,
                    "assignee": item.assignee,
                    "sprint": item.sprint,
                    "priority": item.priority,
                    "parentId": item.parent_id,
                    "children": store.children_of(&item.id).len(),
                    "url": item.url(),
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "total": store.len(),
                "matched": items.len(),
                "spaces": settings.spaces().len(),
                "items": rows,
            }))
            .map_err(|e| e.to_string())?
        );
        return Ok(());
    }

    println!(
        "共 {} 项，命中 {} 项（未完成 {} / 已完成 {}）",
        store.len(),
        items.len(),
        filter.with_status(StatusGroup::Active).apply(store).len(),
        filter.with_status(StatusGroup::Done).apply(store).len()
    );
    for item in items {
        let children = store.children_of(&item.id).len();
        println!(
            "{:<10} {:<6} {:<8} {:<8} {}{}",
            item.serial_number,
            item.status,
            item.assignee,
            item.space_name,
            item.subject,
            if children > 0 {
                format!("  (+{children} 子项)")
            } else {
                String::new()
            }
        );
    }
    Ok(())
}

fn show(store: &Store, needle: &str, flags: &Flags) -> Result<(), String> {
    let item = store
        .find(needle)
        .ok_or_else(|| format!("没有找到工作项 {needle}（缓存 {} 项）", store.len()))?;

    let section = |relation: Relation, items: Vec<&Item>| {
        items
            .into_iter()
            .map(|i| {
                json!({
                    "relation": relation.label(),
                    "serialNumber": i.serial_number,
                    "subject": i.subject,
                    "status": i.status,
                    "assignee": i.assignee,
                    "url": i.url(),
                })
            })
            .collect::<Vec<_>>()
    };

    let mut related = Vec::new();
    if let Some(parent) = store.parent_of(&item.id) {
        related.extend(section(Relation::Parent, vec![parent]));
    }
    related.extend(section(Relation::Child, store.children_of(&item.id)));
    related.extend(section(Relation::Sibling, store.siblings_of(&item.id)));

    let payload = json!({
        "item": {
            "id": item.id,
            "serialNumber": item.serial_number,
            "subject": item.subject,
            "status": item.status,
            "category": item.category,
            "workitemType": item.workitem_type,
            "space": item.space_name,
            "assignee": item.assignee,
            "sprint": item.sprint,
            "priority": item.priority,
            "plannedHours": item.planned_hours,
            "actualHours": item.actual_hours,
            "planStart": item.plan_start,
            "planEnd": item.plan_end,
            "parentId": item.parent_id,
            "url": item.url(),
        },
        "ancestors": store.ancestors_of(&item.id).iter().map(|a| &a.serial_number).collect::<Vec<_>>(),
        "descendants": store.descendants_of(&item.id).len(),
        "related": related,
    });

    if flags.has("json") {
        println!(
            "{}",
            serde_json::to_string_pretty(&payload).map_err(|e| e.to_string())?
        );
        return Ok(());
    }

    println!("{} {}", item.serial_number, item.subject);
    println!(
        "状态 {} · 类型 {} · 项目 {} · 负责人 {} · 迭代 {}",
        item.status, item.workitem_type, item.space_name, item.assignee, item.sprint
    );
    println!("链接 {}", item.url());
    let ancestors = store.ancestors_of(&item.id);
    if !ancestors.is_empty() {
        println!(
            "上级链 {}",
            ancestors
                .iter()
                .map(|a| a.serial_number.as_str())
                .collect::<Vec<_>>()
                .join(" > ")
        );
    }
    for (label, items) in [
        ("子项", store.children_of(&item.id)),
        ("同级", store.siblings_of(&item.id)),
    ] {
        if items.is_empty() {
            continue;
        }
        let total = items.len();
        println!("{label}（{total}）");
        for related in items.iter().take(RELATED_LIMIT) {
            println!(
                "  {:<10} {:<6} {:<8} {}",
                related.serial_number, related.status, related.assignee, related.subject
            );
        }
        if total > RELATED_LIMIT {
            println!("  …还有 {} 条（GUI 里按筛选收窄）", total - RELATED_LIMIT);
        }
    }
    let total = store.descendants_of(&item.id).len();
    if total > 0 {
        println!("子树共 {} 项，其中已完成 {}", total, store.done_descendants(&item.id));
    }
    Ok(())
}

fn build_filter(store: &Store, flags: &Flags, scoped: Option<&str>) -> Result<Filter, String> {
    let mut filter = Filter::default();

    if let Some(space) = flags.value("space") {
        let id = match scoped {
            // 范围拉取时 store 可能为空，项目 ID 已经有现成答案。
            Some(id) => id.to_string(),
            None => store
                .spaces()
                .into_iter()
                .find(|(name, id)| name == space || id == space)
                .map(|(_, id)| id)
                .ok_or_else(|| format!("没有这个项目：{space}"))?,
        };
        filter.set_space_id(Some(id));
    }

    if let Some(category) = flags.value("category") {
        filter.set_category(Some(category.to_string()));
    }

    if let Some(status) = flags.value("status") {
        let group = match status {
            "active" => StatusGroup::Active,
            "done" => StatusGroup::Done,
            "all" => StatusGroup::All,
            other => return Err(format!("--status 只支持 active/done/all，收到 {other}")),
        };
        filter.set_status(group);
    }

    if let Some(assignee) = flags.value("assignee") {
        let found = store
            .assignees()
            .into_iter()
            .find(|(name, id)| name == assignee || id == assignee)
            .map(|(_, id)| id);
        // 范围拉取时服务端已按负责人过滤，结果为空也要能按原值继续筛。
        let id = found
            .or_else(|| scoped.map(|_| assignee.to_string()))
            .ok_or_else(|| format!("没有这个负责人：{assignee}"))?;
        filter.set_assignee_id(Some(id));
    }

    if let Some(window) = flags.value("window") {
        let window = match window {
            "week" => TimeWindow::ThisWeek,
            "month" => TimeWindow::LastMonth,
            "all" => TimeWindow::All,
            other => return Err(format!("--window 只支持 week/month/all，收到 {other}")),
        };
        filter.set_window(window);
    }

    if let Some(keyword) = flags.value("keyword") {
        filter.set_keyword(keyword.to_string());
    }

    Ok(filter)
}

struct Flags {
    values: Vec<(String, Option<String>)>,
}

impl Flags {
    fn parse(args: &[String]) -> Self {
        let mut values = Vec::new();
        let mut iter = args.iter().peekable();
        while let Some(arg) = iter.next() {
            let Some(name) = arg.strip_prefix("--") else {
                continue;
            };
            let value = match iter.peek() {
                Some(next) if !next.starts_with("--") => {
                    let value = (*next).clone();
                    iter.next();
                    Some(value)
                }
                _ => None,
            };
            values.push((name.to_string(), value));
        }
        Self { values }
    }

    fn has(&self, name: &str) -> bool {
        self.values.iter().any(|(key, _)| key == name)
    }

    fn value(&self, name: &str) -> Option<&str> {
        self.values
            .iter()
            .find(|(key, _)| key == name)
            .and_then(|(_, value)| value.as_deref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_parse_values_and_switches() {
        let args: Vec<String> = ["--space", "项目A", "--json", "--keyword", "报告"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let flags = Flags::parse(&args);
        assert_eq!(flags.value("space"), Some("项目A"));
        assert_eq!(flags.value("keyword"), Some("报告"));
        assert!(flags.has("json"));
    }
}
