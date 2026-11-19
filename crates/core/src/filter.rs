//! Which work items the dashboard shows.

use crate::model::{Item, Store};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StatusGroup {
    /// Anything whose status is not in the configured done-status list.
    #[default]
    Active,
    Done,
    All,
}

impl StatusGroup {
    pub const ALL: [StatusGroup; 3] = [StatusGroup::Active, StatusGroup::Done, StatusGroup::All];

    pub fn label(self) -> &'static str {
        match self {
            StatusGroup::Active => "未完成",
            StatusGroup::Done => "已完成",
            StatusGroup::All => "全部",
        }
    }

    pub fn matches(self, item: &Item) -> bool {
        match self {
            StatusGroup::All => true,
            StatusGroup::Active => !item.is_done(),
            StatusGroup::Done => item.is_done(),
        }
    }
}

/// 计划完成时间落在哪个区间。纯本地过滤，不发请求。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TimeWindow {
    #[default]
    All,
    ThisWeek,
    LastMonth,
}

impl TimeWindow {
    pub const ALL: [TimeWindow; 3] = [TimeWindow::All, TimeWindow::ThisWeek, TimeWindow::LastMonth];

    pub fn label(self) -> &'static str {
        match self {
            TimeWindow::All => "全部时间",
            TimeWindow::ThisWeek => "本周",
            TimeWindow::LastMonth => "近一月",
        }
    }

    /// 计划完成时间要落在的日期区间（含两端），按 `YYYY-MM-DD` 字符串比较。
    pub fn date_bounds(self) -> Option<(String, String)> {
        use chrono::{Datelike as _, Duration, Local};
        let today = Local::now().date_naive();
        match self {
            TimeWindow::All => None,
            TimeWindow::ThisWeek => {
                let monday =
                    today - Duration::days(today.weekday().num_days_from_monday() as i64);
                Some((monday.to_string(), (monday + Duration::days(6)).to_string()))
            }
            TimeWindow::LastMonth => {
                Some(((today - Duration::days(30)).to_string(), today.to_string()))
            }
        }
    }

    /// `plan_end` 形如 `2026-11-13 00:00:00`；没有计划完成时间的项只在「全部时间」下出现。
    pub fn matches(self, plan_end: Option<&str>) -> bool {
        let Some((from, to)) = self.date_bounds() else {
            return true;
        };
        let Some(day) = plan_end
            .map(str::trim)
            .and_then(|value| value.get(..10))
            .filter(|day| day.len() == 10)
        else {
            return false;
        };
        from.as_str() <= day && day <= to.as_str()
    }
}

#[derive(Debug, Clone, Default)]
pub struct Filter {
    space_id: Option<String>,
    category: Option<String>,
    status: StatusGroup,
    window: TimeWindow,
    assignee_id: Option<String>,
    keyword: String,
}

impl Filter {
    pub fn space_id(&self) -> Option<&str> {
        self.space_id.as_deref()
    }

    pub fn category(&self) -> Option<&str> {
        self.category.as_deref()
    }

    pub fn status(&self) -> StatusGroup {
        self.status
    }

    pub fn window(&self) -> TimeWindow {
        self.window
    }

    pub fn assignee_id(&self) -> Option<&str> {
        self.assignee_id.as_deref()
    }

    pub fn keyword(&self) -> &str {
        &self.keyword
    }

    pub fn set_space_id(&mut self, value: Option<String>) {
        self.space_id = value;
    }

    pub fn set_category(&mut self, value: Option<String>) {
        self.category = value;
    }

    pub fn set_status(&mut self, value: StatusGroup) {
        self.status = value;
    }

    pub fn set_window(&mut self, value: TimeWindow) {
        self.window = value;
    }

    pub fn set_assignee_id(&mut self, value: Option<String>) {
        self.assignee_id = value;
    }

    pub fn set_keyword(&mut self, value: String) {
        self.keyword = value.trim().to_string();
    }

    /// Same filter with a different status group (used for counters).
    pub fn with_status(&self, status: StatusGroup) -> Self {
        let mut next = self.clone();
        next.status = status;
        next
    }

    pub fn matches(&self, item: &Item) -> bool {
        if let Some(space) = &self.space_id {
            if &item.space_id != space {
                return false;
            }
        }
        if let Some(category) = &self.category {
            if &item.category != category {
                return false;
            }
        }
        if !self.status.matches(item) {
            return false;
        }
        if !self.window.matches(item.plan_end.as_deref()) {
            return false;
        }
        if let Some(assignee) = &self.assignee_id {
            if &item.assignee_id != assignee {
                return false;
            }
        }
        if !self.keyword.is_empty() {
            let needle = self.keyword.to_lowercase();
            let haystack = format!("{} {} {}", item.subject, item.serial_number, item.assignee).to_lowercase();
            if !haystack.contains(&needle) {
                return false;
            }
        }
        true
    }

    /// Filtered items, most recently updated first.
    pub fn apply<'a>(&self, store: &'a Store) -> Vec<&'a Item> {
        let mut items: Vec<&Item> = store
            .sorted_items()
            .into_iter()
            .filter(|item| self.matches(item))
            .collect();
        items.shrink_to_fit();
        items
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::StatusTone;

    fn sample(status: &str, space: &str, category: &str, assignee: &str, subject: &str) -> Item {
        Item {
            id: "id".into(),
            serial_number: "ABC-1".into(),
            subject: subject.into(),
            description: String::new(),
            status: status.into(),
            status_id: String::new(),
            workitem_type: category.into(),
            category: category.into(),
            space_id: space.into(),
            space_name: "项目A".into(),
            assignee_id: assignee.into(),
            assignee: assignee.into(),
            parent_id: None,
            id_path: vec![],
            sprint: String::new(),
            priority: String::new(),
            planned_hours: None,
            actual_hours: None,
            plan_start: None,
            plan_end: None,
            created_at: 0,
            updated_at: 0,
            done: ["完成", "关闭", "取消"].iter().any(|hint| status.contains(hint)),
            tone: StatusTone::Info,
        }
    }

    #[test]
    fn window_keeps_only_plan_end_inside_the_week() {
        let (from, to) = TimeWindow::ThisWeek.date_bounds().unwrap();
        let mut filter = Filter::default();
        filter.set_window(TimeWindow::ThisWeek);

        let mut inside = sample("待处理", "s", "Task", "u", "x");
        inside.plan_end = Some(format!("{from} 00:00:00"));
        assert!(filter.matches(&inside));

        let mut weekend = sample("待处理", "s", "Task", "u", "x");
        weekend.plan_end = Some(format!("{to} 00:00:00"));
        assert!(filter.matches(&weekend));

        let mut outside = sample("待处理", "s", "Task", "u", "x");
        outside.plan_end = Some("2000-01-01 00:00:00".into());
        assert!(!filter.matches(&outside));

        // 没有计划完成时间的项只在「全部时间」下出现。
        let mut missing = sample("待处理", "s", "Task", "u", "x");
        missing.plan_end = None;
        assert!(!filter.matches(&missing));

        filter.set_window(TimeWindow::All);
        assert!(filter.matches(&missing));
    }

    #[test]
    fn active_is_the_default_and_hides_done_items() {
        let filter = Filter::default();
        assert!(filter.matches(&sample("待处理", "s", "Task", "u", "x")));
        assert!(filter.matches(&sample("开发中", "s", "Task", "u", "x")));
        assert!(!filter.matches(&sample("已完成", "s", "Task", "u", "x")));
        assert!(!filter.matches(&sample("已取消", "s", "Task", "u", "x")));
    }

    #[test]
    fn done_group_only_shows_finished_items() {
        let mut filter = Filter::default();
        filter.set_status(StatusGroup::Done);
        assert!(filter.matches(&sample("已完成", "s", "Task", "u", "x")));
        assert!(!filter.matches(&sample("处理中", "s", "Task", "u", "x")));
    }

    #[test]
    fn space_category_assignee_and_keyword_all_apply() {
        let mut filter = Filter::default();
        filter.set_space_id(Some("s1".into()));
        filter.set_category(Some("Task".into()));
        filter.set_assignee_id(Some("u1".into()));
        filter.set_keyword("报告".into());

        assert!(filter.matches(&sample("待处理", "s1", "Task", "u1", "季度报告")));
        assert!(!filter.matches(&sample("待处理", "s2", "Task", "u1", "季度报告")));
        assert!(!filter.matches(&sample("待处理", "s1", "Req", "u1", "季度报告")));
        assert!(!filter.matches(&sample("待处理", "s1", "Task", "u2", "季度报告")));
        assert!(!filter.matches(&sample("待处理", "s1", "Task", "u1", "其他标题")));
    }
}
