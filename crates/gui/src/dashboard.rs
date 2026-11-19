//! The dashboard view: filter bar, task list, and the relation panel for the
//! selected work item (parent chain / children / siblings + browser jump).

use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::component::{
    ActiveTheme, Disableable as _, Icon, IconName, IndexPath, Sizable, StyledExt as _, Theme,
    button::{Button, ButtonVariants as _},
    input::{Input, InputEvent, InputState},
    scroll::ScrollableElement as _,
    select::{Select, SelectEvent, SelectState},
    h_flex,
    v_flex,
    v_virtual_list,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use taskdash_core::{
    load_description, load_foreign_item, load_related_items, load_relations, load_spaces, load_view,
    tree_children, tree_rows, Filter, Item, Relation, RelationRecord, Settings, StatusGroup,
    StatusTone, Store, TimeWindow, TreeRow,
};

const ROW_HEIGHT: f32 = 46.0;
const LIST_WIDTH: f32 = 620.0;
const MAX_RELATED_ROWS: usize = 12;
const MAX_SIBLING_ROWS: usize = 8;
/// 任务树最多为多少条可见任务自动分组；再多就只展开选中项那条链，
/// 否则清掉「只看我的」时是几百个关联请求。
const TREE_GROUP_LIMIT: usize = 40;
/// 网络请求并发上限：逐个排队，避免切树时几十个请求同时打出去被限流。
const NET_CONCURRENCY: usize = 3;

/// 排障日志：`TASKDASH_DEBUG=1` 时打到 stderr，平时完全静默。
macro_rules! debug_log {
    ($($arg:tt)*) => {
        if std::env::var_os("TASKDASH_DEBUG").is_some() {
            eprintln!("[taskdash] {}", format_args!($($arg)*));
        }
    };
}
const ALL_SPACES: &str = "全部项目";
const ALL_ASSIGNEES: &str = "全部负责人";

/// 一次加载的范围：项目集合 × 类别 × 负责人（负责人已下推到服务端）。
type ViewKey = (Vec<String>, String, Option<String>);

/// 描述只在 `GET /workitems/{id}` 里返回，选中项时按需拉一次并缓存。
#[derive(Debug, Clone)]
enum DescState {
    Loading,
    Ready(String),
    Failed(String),
}

/// 当前加载范围之外的祖先（常见：父需求在别的项目），按 id 单独拉，只用于展示。
#[derive(Debug, Clone)]
enum ForeignState {
    Loading,
    Ready(Item),
    Failed(String),
}

/// 工作项关联（`relationType=ASSOCIATED`，双向）。
#[derive(Debug, Clone)]
enum RelState {
    Loading,
    Ready(Vec<RelationRecord>),
    Failed(String),
}

/// 真实子项（`relationType=SUB`），按父项 id 缓存。
#[derive(Debug, Clone)]
enum SubState {
    Loading,
    Ready(Vec<String>),
    Failed,
}

/// 左栏展示方式：平铺筛选结果，或按需求/关联关系展开的任务树。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ListMode {
    Flat,
    Tree,
}

impl ListMode {
    const ALL: [ListMode; 2] = [ListMode::Flat, ListMode::Tree];

    fn label(self) -> &'static str {
        match self {
            ListMode::Flat => "列表",
            ListMode::Tree => "任务树",
        }
    }
}

/// Theme colors copied out of `cx.theme()` so render helpers don't borrow the app.
#[derive(Debug, Clone, Copy)]
struct Palette {
    background: Hsla,
    foreground: Hsla,
    muted: Hsla,
    muted_foreground: Hsla,
    border: Hsla,
    primary: Hsla,
    primary_foreground: Hsla,
    accent: Hsla,
    selection: Hsla,
    info: Hsla,
    warning: Hsla,
    success: Hsla,
}

impl Palette {
    fn of(theme: &Theme) -> Self {
        Self {
            background: theme.background,
            foreground: theme.foreground,
            muted: theme.muted,
            muted_foreground: theme.muted_foreground,
            border: theme.border,
            primary: theme.primary,
            primary_foreground: theme.primary_foreground,
            accent: theme.accent,
            selection: theme.selection,
            info: theme.info,
            warning: theme.warning,
            success: theme.success,
        }
    }

    /// Status dot color: finished items fade, in-flight items are blue.
    fn status(&self, item: &Item) -> Hsla {
        if item.is_done() {
            return self.muted_foreground;
        }
        match item.tone {
            StatusTone::Info => self.info,
            StatusTone::Warning => self.warning,
            StatusTone::Success => self.success,
        }
    }
}

pub struct Dashboard {
    settings: Option<Settings>,
    config_error: Option<String>,
    store: Arc<Store>,
    loading: bool,
    pages: usize,
    filter: Filter,
    visible: Vec<String>,
    selected: Option<String>,
    space_choices: Vec<(String, Option<String>)>,
    assignee_choices: Vec<(String, Option<String>)>,
    /// 类别候选，来自配置的 `taskdash_categories`，总是具体值，没有「全部」。
    category_choices: Vec<String>,
    /// 项目名表，来自配置的项目 ID（不依赖数据）。
    spaces: Vec<(String, String)>,
    /// 拉过的范围，切回来直接命中。
    cache: HashMap<ViewKey, Arc<Store>>,
    /// 选中项的描述（Markdown），按工作项 ID 缓存。
    descriptions: HashMap<String, DescState>,
    /// 范围外祖先，按工作项 ID 缓存。
    foreign: HashMap<String, ForeignState>,
    /// 关联项（需求/任务），按工作项 ID 缓存。
    relations: HashMap<String, RelState>,
    /// 真实子项（SUB），按工作项 ID 缓存。
    sub_items: HashMap<String, SubState>,
    /// 左栏展示方式。
    list_mode: ListMode,
    /// 树里被折叠的节点。
    collapsed: HashSet<String>,
    /// 关联 / 补拉请求的排队，并发数由 `NET_CONCURRENCY` 控住。
    relation_queue: VecDeque<String>,
    relations_inflight: usize,
    foreign_queue: VecDeque<(String, Option<String>)>,
    foreign_inflight: usize,
    space_select: Entity<SelectState<Vec<SharedString>>>,
    category_select: Entity<SelectState<Vec<SharedString>>>,
    assignee_select: Entity<SelectState<Vec<SharedString>>>,
    search_input: Entity<InputState>,
    /// Select options arrive after a sync, but `set_items` needs a `Window`;
    /// render is the first place we have one.
    pending_options: Option<(Vec<SharedString>, Vec<SharedString>)>,
    /// 默认筛选只对齐一次下拉显示，避免刷新时覆盖用户手动选的筛选。
    defaults_applied: bool,
}

impl Dashboard {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        // 先读配置：类别下拉的候选在构造时就固定，不能靠异步补。
        let loaded = Settings::load();
        let mut category_choices: Vec<String> = loaded
            .as_ref()
            .ok()
            .map(|settings| settings.categories().to_vec())
            .unwrap_or_default();
        if category_choices.is_empty() {
            category_choices.push("Task".to_string());
        }

        let space_select = cx.new(|cx| {
            SelectState::new(
                vec![SharedString::from(ALL_SPACES)],
                Some(IndexPath::default()),
                window,
                cx,
            )
        });
        let category_select = cx.new(|cx| {
            SelectState::new(
                category_choices
                    .iter()
                    .map(|category| SharedString::from(category_label(category).to_string()))
                    .collect::<Vec<_>>(),
                Some(IndexPath::default()),
                window,
                cx,
            )
        });
        let assignee_select = cx.new(|cx| {
            SelectState::new(
                vec![SharedString::from(ALL_ASSIGNEES)],
                Some(IndexPath::default()),
                window,
                cx,
            )
        });
        let search_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("搜索标题 / 编号 / 负责人")
        });

        let mut this = Self {
            settings: None,
            config_error: None,
            store: Arc::new(Store::default()),
            loading: false,
            pages: 0,
            filter: Filter::default(),
            visible: Vec::new(),
            selected: None,
            space_choices: vec![(ALL_SPACES.to_string(), None)],
            assignee_choices: vec![(ALL_ASSIGNEES.to_string(), None)],
            category_choices,
            spaces: Vec::new(),
            cache: HashMap::new(),
            descriptions: HashMap::new(),
            foreign: HashMap::new(),
            relations: HashMap::new(),
            sub_items: HashMap::new(),
            list_mode: ListMode::Flat,
            collapsed: HashSet::new(),
            relation_queue: VecDeque::new(),
            relations_inflight: 0,
            foreign_queue: VecDeque::new(),
            foreign_inflight: 0,
            space_select,
            category_select,
            assignee_select,
            search_input,
            pending_options: None,
            defaults_applied: false,
        };

        cx.subscribe_in(
            &this.space_select,
            window,
            |this, _, event: &SelectEvent<Vec<SharedString>>, _window, cx| {
                let SelectEvent::Confirm(Some(name)) = event else {
                    return;
                };
                let id = this
                    .space_choices
                    .iter()
                    .find(|(label, _)| label == name.as_ref())
                    .and_then(|(_, id)| id.clone());
                this.filter.set_space_id(id);
                this.sync(cx);
            },
        )
        .detach();

        cx.subscribe_in(
            &this.category_select,
            window,
            |this, _, event: &SelectEvent<Vec<SharedString>>, _window, cx| {
                let SelectEvent::Confirm(Some(name)) = event else {
                    return;
                };
                let category = this
                    .category_choices
                    .iter()
                    .find(|category| category_label(category) == name.as_ref())
                    .cloned();
                if let Some(category) = category {
                    this.filter.set_category(Some(category));
                    this.sync(cx);
                }
            },
        )
        .detach();

        cx.subscribe_in(
            &this.assignee_select,
            window,
            |this, _, event: &SelectEvent<Vec<SharedString>>, _window, cx| {
                let SelectEvent::Confirm(Some(name)) = event else {
                    return;
                };
                let id = this
                    .assignee_choices
                    .iter()
                    .find(|(label, _)| label == name.as_ref())
                    .and_then(|(_, id)| id.clone());
                this.filter.set_assignee_id(id);
                this.sync(cx);
            },
        )
        .detach();

        cx.subscribe_in(
            &this.search_input,
            window,
            |this, _, event: &InputEvent, _window, cx| {
                if matches!(event, InputEvent::Change) {
                    let value = this.search_input.read(cx).value().to_string();
                    this.filter.set_keyword(value);
                    this.refresh_visible(cx);
                }
            },
        )
        .detach();

        match loaded {
            Ok(settings) => {
                // 启动默认：只看我的 + 配置指定的项目 + 配置里的第一个类别（任务）。
                this.filter
                    .set_space_id(settings.default_space().map(str::to_string));
                this.filter
                    .set_assignee_id(settings.user_id().map(str::to_string));
                this.filter
                    .set_category(this.category_choices.first().cloned());
                // 默认视图：本周 + 全部状态。
                this.filter.set_window(TimeWindow::ThisWeek);
                this.filter.set_status(StatusGroup::All);
                this.settings = Some(settings);
                this.sync(cx);
            }
            Err(err) => this.config_error = Some(err.to_string()),
        }

        this
    }

    /// 当前筛选对应的加载范围。负责人为空表示不过滤。
    fn view_key(&self) -> ViewKey {
        let spaces = match self.filter.space_id() {
            Some(id) => vec![id.to_string()],
            None => self
                .settings
                .as_ref()
                .map(|settings| settings.spaces().to_vec())
                .unwrap_or_default(),
        };
        (
            spaces,
            self.category().to_string(),
            self.filter.assignee_id().map(str::to_string),
        )
    }

    /// 当前类别，总有一个具体值。
    fn category(&self) -> &str {
        self.filter.category().unwrap_or("Task")
    }

    fn sync(&mut self, cx: &mut Context<Self>) {
        let Some(settings) = self.settings.clone() else {
            return;
        };
        if self.loading {
            return;
        }

        let key = self.view_key();
        if let Some(cached) = self.cache.get(&key).cloned() {
            self.apply_store(cached, cx);
            return;
        }

        self.loading = true;
        self.pages = 0;
        cx.notify();

        let need_spaces = self.spaces.is_empty();
        cx.spawn(async move |this, cx| {
            let fetch_key = key.clone();
            let result = cx
                .background_spawn(async move {
                    let spaces = if need_spaces {
                        load_spaces(&settings).unwrap_or_default()
                    } else {
                        Vec::new()
                    };
                    load_view(
                        &settings,
                        &fetch_key.0,
                        &fetch_key.1,
                        fetch_key.2.as_deref(),
                        |_| {},
                    )
                    .map(|store| (spaces, store))
                })
                .await;

            this.update(cx, |this, cx| {
                this.loading = false;
                match result {
                    Ok((spaces, store)) => {
                        if !spaces.is_empty() {
                            this.spaces = spaces;
                        }
                        this.config_error = None;
                        let store = Arc::new(store);
                        // shortcut: 缓存不淘汰，超过 12 个范围就整表清空；真要细化再换成 LRU。
                        if this.cache.len() >= 12 {
                            this.cache.clear();
                        }
                        this.cache.insert(key, store.clone());
                        this.apply_store(store, cx);
                    }
                    Err(err) => {
                        let message = err.to_string();
                        this.store = Arc::new(Store::empty_with_error(message.clone()));
                        this.visible.clear();
                        this.selected = None;
                        this.config_error = Some(message);
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    /// 刷新当前范围：丢掉缓存重拉。
    fn refresh(&mut self, cx: &mut Context<Self>) {
        let key = self.view_key();
        self.cache.remove(&key);
        self.descriptions.clear();
        self.foreign.clear();
        self.relations.clear();
        self.sub_items.clear();
        self.collapsed.clear();
        self.relation_queue.clear();
        self.foreign_queue.clear();
        self.sync(cx);
    }

    fn apply_store(&mut self, store: Arc<Store>, cx: &mut Context<Self>) {
        self.store = store;
        self.pages = self.store.len();
        self.rebuild_choices();
        self.refresh_visible(cx);
    }

    fn rebuild_choices(&mut self) {
        // 项目候选来自配置（+ 本次结果里出现的其他项目），不再依赖全量同步数据。
        let mut space_choices = vec![(ALL_SPACES.to_string(), None)];
        space_choices.extend(self.spaces.iter().map(|(name, id)| (name.clone(), Some(id.clone()))));
        for (name, id) in self.store.spaces() {
            if !space_choices.iter().any(|(_, choice)| choice.as_deref() == Some(id.as_str())) {
                space_choices.push((name, Some(id)));
            }
        }

        // 配置里的默认项目不在本次同步结果中时退回「全部项目」，否则会筛出空列表。
        let known = self
            .filter
            .space_id()
            .map(|id| space_choices.iter().any(|(_, choice)| choice.as_deref() == Some(id)))
            .unwrap_or(true);
        if !known {
            self.filter.set_space_id(None);
        }

        let mut assignee_choices = vec![(ALL_ASSIGNEES.to_string(), None)];
        assignee_choices.extend(
            self.store
                .assignees()
                .into_iter()
                .map(|(name, id)| (name, Some(id))),
        );

        self.pending_options = Some((
            space_choices
                .iter()
                .map(|(label, _)| SharedString::from(label.clone()))
                .collect(),
            assignee_choices
                .iter()
                .map(|(label, _)| SharedString::from(label.clone()))
                .collect(),
        ));
        self.space_choices = space_choices;
        self.assignee_choices = assignee_choices;
    }

    /// 把两个下拉的选中项对齐到当前筛选，避免显示「全部项目 / 全部负责人」而实际已过滤。
    /// 只在首次同步后调用，之后不再覆盖用户手选。
    fn align_selects(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let space_ix = self.filter.space_id().and_then(|id| {
            self.space_choices
                .iter()
                .position(|(_, choice)| choice.as_deref() == Some(id))
        });
        if let Some(ix) = space_ix {
            self.space_select.update(cx, |state, cx| {
                state.set_selected_index(Some(IndexPath::new(ix)), window, cx)
            });
        }

        let me_ix = self.filter.assignee_id().and_then(|id| {
            self.assignee_choices
                .iter()
                .position(|(_, choice)| choice.as_deref() == Some(id))
        });
        if let Some(ix) = me_ix {
            self.assignee_select.update(cx, |state, cx| {
                state.set_selected_index(Some(IndexPath::new(ix)), window, cx)
            });
        }
    }

    fn refresh_visible(&mut self, cx: &mut Context<Self>) {
        self.visible = self
            .filter
            .apply(&self.store)
            .into_iter()
            .map(|item| item.id.clone())
            .collect();

        let keep_selection = self
            .selected
            .as_ref()
            .map(|id| {
                self.visible.iter().any(|visible| visible == id)
                    || matches!(self.foreign.get(id), Some(ForeignState::Ready(_)))
            })
            .unwrap_or(false);
        if !keep_selection {
            self.selected = self.visible.first().cloned();
        }
        if let Some(id) = self.selected.clone() {
            self.ensure_description(id.clone(), cx);
            self.ensure_ancestors(&id, cx);
            self.ensure_relations(id, cx);
        }
        self.ensure_tree(cx);
        cx.notify();
    }

    fn select(&mut self, id: String, cx: &mut Context<Self>) {
        debug_log!("select {id}");
        // 范围外的项（关联需求/兄弟任务）也能在详情里看，只是不在左侧列表。
        self.selected = Some(id.clone());
        self.ensure_description(id.clone(), cx);
        self.ensure_ancestors(&id, cx);
        self.ensure_relations(id, cx);
        self.ensure_tree(cx);
        cx.notify();
    }

    // ------------------------------------------------------------------ tree

    /// 单个工作项的树根：沿父级上溯，真实父项优先，其次唯一关联的需求/任务；
    /// 一路上没有就自己当根。
    fn root_of(&self, id: &str) -> String {
        let mut id = id.to_string();
        let mut seen = HashSet::new();
        while seen.insert(id.clone()) {
            let Some(next) = self.parent_of_known(&id) else {
                break;
            };
            if next == id {
                break;
            }
            id = next;
        }
        id
    }

    /// 顶级行：当前每条筛选结果各自的根，按第一次出现的顺序去重
    /// （需求行落在它第一个任务的位置）。可见任务太多时不分组，直接平铺。
    fn tree_roots(&self) -> Vec<String> {
        let group = self.visible.len() <= TREE_GROUP_LIMIT;
        let mut roots: Vec<String> = Vec::new();
        for id in &self.visible {
            let root = if group { self.root_of(id) } else { id.clone() };
            if !roots.contains(&root) {
                roots.push(root);
            }
        }
        roots
    }

    /// 分组还没算完时先不画树，否则几十行会一条条跳进需求里。
    fn tree_ready(&self) -> bool {
        if self.visible.len() > TREE_GROUP_LIMIT {
            return true;
        }
        self.visible.iter().all(|id| {
            matches!(
                self.relations.get(id),
                Some(RelState::Ready(_)) | Some(RelState::Failed(_))
            )
        })
    }

    /// 树模式要补的数据：每条可见任务的关联（分组 + 缺陷）、各根的子项，
    /// 以及缺父项的补拉。请求都排队，并发上限见 `NET_CONCURRENCY`。
    fn ensure_tree(&mut self, cx: &mut Context<Self>) {
        if self.list_mode != ListMode::Tree {
            return;
        }
        let group = self.visible.len() <= TREE_GROUP_LIMIT;
        if group {
            for id in self.visible.clone() {
                self.ensure_relations(id.clone(), cx);
                self.ensure_missing_parent(&id, cx);
            }
        }
        let roots = self.tree_roots();
        for root in &roots {
            self.ensure_relations(root.clone(), cx);
            // 需求、以及上溯出来的父任务（它本身不在筛选结果里）必须拉子项；
            // 筛选结果里的任务只靠关联就能带出缺陷，不用再多发一次 SUB。
            let is_req = self
                .item_by_id(root)
                .is_some_and(|item| item.category == "Req");
            if is_req || !self.visible.contains(root) {
                self.ensure_sub(root.clone(), cx);
            }
        }
        // 选中项到根的链路始终展开，哪怕上面因为数量上限没分组。
        if let Some(selected) = self.selected.clone() {
            let chain_root = self.root_of(&selected);
            let mut node = Some(selected);
            while let Some(id) = node {
                self.ensure_relations(id.clone(), cx);
                self.ensure_sub(id.clone(), cx);
                self.ensure_missing_parent(&id, cx);
                if id == chain_root {
                    break;
                }
                node = self.parent_of_known(&id);
            }
        }
        debug_log!(
            "树：{} 组，{} 行，关联队列 {}，补拉队列 {}",
            roots.len(),
            self.list_rows().len(),
            self.relation_queue.len(),
            self.foreign_queue.len()
        );
    }

    /// `parent_id` 指向范围外的父项时按 id 补拉，才能继续上溯分组。
    fn ensure_missing_parent(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(item) = self.item_by_id(id) else {
            return;
        };
        let Some(parent) = item.parent_id.clone() else {
            return;
        };
        if self.store.get(&parent).is_some() || self.foreign.contains_key(&parent) {
            return;
        }
        self.fetch_foreign(parent, None, cx);
    }

    fn ensure_edges(&mut self, id: String, cx: &mut Context<Self>) {
        // 展开时重试上次失败的子项请求，否则失败的节点永远打不开。
        if matches!(self.sub_items.get(&id), Some(SubState::Failed)) {
            self.sub_items.remove(&id);
        }
        self.ensure_relations(id.clone(), cx);
        self.ensure_sub(id, cx);
    }

    /// 真实子项按需拉一次；子项同时进 `foreign`，详情和树共用同一份。
    fn ensure_sub(&mut self, id: String, cx: &mut Context<Self>) {
        if self.sub_items.contains_key(&id) {
            return;
        }
        let Some(settings) = self.settings.clone() else {
            return;
        };

        self.sub_items.insert(id.clone(), SubState::Loading);
        cx.spawn(async move |this, cx| {
            let fetch_id = id.clone();
            let result = cx
                .background_spawn(async move { load_related_items(&settings, &fetch_id, "SUB") })
                .await;
            this.update(cx, |this, cx| {
                let state = match result {
                    Ok(items) => {
                        let ids = items.iter().map(|item| item.id.clone()).collect();
                        for item in items {
                            this.foreign.insert(item.id.clone(), ForeignState::Ready(item));
                        }
                        SubState::Ready(ids)
                    }
                    Err(err) => {
                        debug_log!("子项 {id} 失败：{err}");
                        SubState::Failed
                    }
                };
                this.sub_items.insert(id, state);
                this.ensure_tree(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 一个节点在树里的子项 id，按编号排序。
    fn tree_children_of(&self, id: &str) -> Vec<String> {
        let Some(item) = self.item_by_id(id) else {
            return Vec::new();
        };
        let sub: Vec<&Item> = match self.sub_items.get(id) {
            Some(SubState::Ready(ids)) => ids.iter().filter_map(|id| self.item_by_id(id)).collect(),
            _ => Vec::new(),
        };
        let assoc: Vec<&Item> = match self.relations.get(id) {
            Some(RelState::Ready(records)) => records
                .iter()
                .filter_map(|record| self.item_by_id(&record.resource_id))
                .collect(),
            _ => Vec::new(),
        };
        let mut ids = tree_children(item, &sub, &assoc);
        ids.sort_by_key(|id| self.item_by_id(id).map(Item::serial_no).unwrap_or(0));
        ids
    }

    /// 左栏当前要显示的行：平铺筛选结果，或从各根展开的任务树。
    fn list_rows(&self) -> Vec<TreeRow> {
        match self.list_mode {
            ListMode::Flat => self
                .visible
                .iter()
                .map(|id| TreeRow {
                    id: id.clone(),
                    depth: 0,
                })
                .collect(),
            ListMode::Tree => {
                if !self.tree_ready() {
                    return Vec::new();
                }
                tree_rows(
                    &self.tree_roots(),
                    |id| self.tree_children_of(id),
                    &self.collapsed,
                )
            }
        }
    }

    /// 父项：真实 `parent_id` 优先；没有时看关联里唯一的需求/任务
    /// （「项目B」的任务挂在需求上、项目A的缺陷挂在任务上，都只有这一条路）。
    fn parent_of_known(&self, id: &str) -> Option<String> {
        let item = self.item_by_id(id)?;
        if let Some(parent) = item.parent_id.clone() {
            return Some(parent);
        }
        let Some(RelState::Ready(records)) = self.relations.get(id) else {
            return None;
        };
        let mut parents: Vec<&str> = Vec::new();
        for record in records {
            let Some(item) = self.item_by_id(&record.resource_id) else {
                continue;
            };
            if matches!(item.category.as_str(), "Req" | "Task")
                && !parents.contains(&record.resource_id.as_str())
            {
                parents.push(record.resource_id.as_str());
            }
        }
        match parents.as_slice() {
            [] => None,
            [only] => Some(only.to_string()),
            // 多个关联上级时优先需求，否则不猜（平级关联任务不铺进树）。
            many => many
                .iter()
                .find(|id| self.item_by_id(id).is_some_and(|item| item.category == "Req"))
                .map(|id| id.to_string()),
        }
    }

    /// 节点是否还在等边数据（SUB / 关联），用来决定先显示折叠箭头。
    fn tree_pending(&self, id: &str) -> bool {
        !self.sub_items.contains_key(id) || !self.relations.contains_key(id)
    }

    /// 关联项：任务→需求、需求→关联任务都是同一接口，双向都能查到。
    /// 请求进队列，避免切树时几十个一起打出去。
    fn ensure_relations(&mut self, id: String, cx: &mut Context<Self>) {
        if self.relations.contains_key(&id) {
            return;
        }
        self.relations.insert(id.clone(), RelState::Loading);
        self.relation_queue.push_back(id);
        self.pump_relations(cx);
    }

    fn pump_relations(&mut self, cx: &mut Context<Self>) {
        while self.relations_inflight < NET_CONCURRENCY {
            let Some(id) = self.relation_queue.pop_front() else {
                break;
            };
            let Some(settings) = self.settings.clone() else {
                return;
            };
            self.relations_inflight += 1;
            cx.spawn(async move |this, cx| {
                let fetch_id = id.clone();
                let result = cx
                    .background_spawn(async move { load_relations(&settings, &fetch_id) })
                    .await;
                this.update(cx, |this, cx| {
                    this.relations_inflight = this.relations_inflight.saturating_sub(1);
                    match result {
                        Ok(records) => {
                            debug_log!(
                                "关联 {id}：{} 条 {:?}",
                                records.len(),
                                records
                                    .iter()
                                    .map(|r| (r.resource_type.as_str(), r.resource_id.as_str()))
                                    .collect::<Vec<_>>()
                            );
                            // 需求上的关联就是全部关联任务，递归一层即可拿到同需求的兄弟。
                            for record in records.iter().filter(|r| r.resource_type == "Req") {
                                this.ensure_relations(record.resource_id.clone(), cx);
                            }
                            // 关联项不在当前加载范围时按 id 补，只用于展示。
                            for record in &records {
                                if this.item_by_id(&record.resource_id).is_none()
                                    && !this.foreign.contains_key(&record.resource_id)
                                {
                                    this.fetch_foreign(
                                        record.resource_id.clone(),
                                        Some(record.resource_type.clone()),
                                        cx,
                                    );
                                }
                            }
                            this.relations.insert(id, RelState::Ready(records));
                        }
                        Err(err) => {
                            debug_log!("关联 {id} 失败：{err}");
                            this.relations.insert(id, RelState::Failed(err.to_string()));
                        }
                    }
                    this.ensure_tree(cx);
                    this.pump_relations(cx);
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
    }

    fn item_by_id(&self, id: &str) -> Option<&Item> {
        self.store.get(id).or_else(|| match self.foreign.get(id) {
            Some(ForeignState::Ready(item)) => Some(item),
            _ => None,
        })
    }

    /// 父项/祖先不在当前加载范围时，按 id 单独拉（不带项目/类别条件）。
    fn ensure_ancestors(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(item) = self.store.get(id) else {
            return;
        };
        let mut missing: Vec<String> = item
            .id_path
            .iter()
            .filter(|path| path.as_str() != id)
            .filter(|path| self.store.get(path).is_none() && !self.foreign.contains_key(*path))
            .cloned()
            .collect();
        if let Some(parent) = item.parent_id.as_deref() {
            if self.store.get(parent).is_none()
                && !self.foreign.contains_key(parent)
                && !missing.iter().any(|known| known == parent)
            {
                missing.push(parent.to_string());
            }
        }
        debug_log!(
            "选中 {id}：store {} 项，idPath {:?}，需补拉 {:?}",
            self.store.len(),
            item.id_path,
            missing
        );
        for id in missing {
            self.fetch_foreign(id, None, cx);
        }
    }

    /// 按 id 补拉范围外的工作项。`category` 来自关系记录的 `resourceType`，
    /// 决定浏览器链接里的类别段；从 `parent_id` 补拉时没有类型信息。
    /// 同样进队列，和关联请求共用并发上限。
    fn fetch_foreign(&mut self, id: String, category: Option<String>, cx: &mut Context<Self>) {
        if self.foreign.contains_key(&id) {
            return;
        }
        self.foreign.insert(id.clone(), ForeignState::Loading);
        self.foreign_queue.push_back((id, category));
        self.pump_foreign(cx);
    }

    fn pump_foreign(&mut self, cx: &mut Context<Self>) {
        while self.foreign_inflight < NET_CONCURRENCY {
            let Some((id, category)) = self.foreign_queue.pop_front() else {
                break;
            };
            let Some(settings) = self.settings.clone() else {
                return;
            };
            self.foreign_inflight += 1;
            cx.spawn(async move |this, cx| {
                let fetch_id = id.clone();
                let result = cx
                    .background_spawn(async move {
                        load_foreign_item(&settings, &fetch_id, category.as_deref())
                    })
                    .await;
                this.update(cx, |this, cx| {
                    this.foreign_inflight = this.foreign_inflight.saturating_sub(1);
                    let state = match result {
                        Ok(item) => {
                            debug_log!("补拉成功 {} {}", item.serial_number, item.subject);
                            ForeignState::Ready(item)
                        }
                        Err(err) => {
                            debug_log!("补拉失败 {id}：{err}");
                            ForeignState::Failed(err.to_string())
                        }
                    };
                    this.foreign.insert(id, state);
                    this.ensure_tree(cx);
                    this.pump_foreign(cx);
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
    }

    /// 拉一次描述并缓存；已缓存（含加载中）直接跳过。
    fn ensure_description(&mut self, id: String, cx: &mut Context<Self>) {
        if self.descriptions.contains_key(&id) {
            return;
        }
        let Some(settings) = self.settings.clone() else {
            return;
        };

        self.descriptions.insert(id.clone(), DescState::Loading);
        cx.spawn(async move |this, cx| {
            let fetch_id = id.clone();
            let result = cx
                .background_spawn(async move { load_description(&settings, &fetch_id) })
                .await;
            this.update(cx, |this, cx| {
                let state = match result {
                    Ok(text) => DescState::Ready(text),
                    Err(err) => DescState::Failed(err.to_string()),
                };
                this.descriptions.insert(id, state);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn count_for(&self, status: StatusGroup) -> usize {
        self.filter.with_status(status).apply(&self.store).len()
    }

    // ---------------------------------------------------------------- header

    fn header(&self, palette: Palette, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .w_full()
            .px_4()
            .py_3()
            .gap_3()
            .items_center()
            .border_b_1()
            .border_color(palette.border)
            .child(
                div()
                    .text_lg()
                    .font_semibold()
                    .child("任务看板"),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(palette.muted_foreground)
                    .child(format!(
                        "{} 项 · {} 个项目",
                        self.store.len(),
                        self.spaces.len()
                    )),
            )
            .child(div().flex_1())
            .child(
                div()
                    .w(px(240.))
                    .child(Input::new(&self.search_input).small().cleanable(true)),
            )
            .child(
                div().w(px(110.)).child(
                    Select::new(&self.category_select)
                        .small()
                        .title_prefix("类别")
                        .menu_width(px(140.)),
                ),
            )
            .child(
                div().w(px(150.)).child(
                    Select::new(&self.space_select)
                        .small()
                        .title_prefix("项目")
                        .menu_width(px(220.)),
                ),
            )
            .child(
                div().w(px(150.)).child(
                    Select::new(&self.assignee_select)
                        .small()
                        .title_prefix("负责人")
                        .menu_width(px(200.)),
                ),
            )
            .child(
                Button::new("sync")
                    .small()
                    .icon(IconName::RefreshCw)
                    .label(if self.loading { "同步中" } else { "刷新" })
                    .disabled(self.loading)
                    .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
            )
    }

    // ------------------------------------------------------------ filter bar

    fn filter_bar(&self, palette: Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let mut row = h_flex().w_full().px_4().py_2().gap_2().items_center();

        // Status groups.
        for group in StatusGroup::ALL {
            let selected = self.filter.status() == group;
            let label = format!("{} {}", group.label(), self.count_for(group));
            row = row.child(chip(
                &label,
                selected,
                palette,
                cx.listener(move |this, _: &ClickEvent, _window, cx| {
                    this.filter.set_status(group);
                    this.refresh_visible(cx);
                }),
            ));
        }

        row = row.child(div().w(px(1.)).h(px(16.)).bg(palette.border));

        // 计划完成时间窗口：纯本地过滤，切了不重新拉取。
        for window in TimeWindow::ALL {
            let selected = self.filter.window() == window;
            row = row.child(chip(
                window.label(),
                selected,
                palette,
                cx.listener(move |this, _: &ClickEvent, _window, cx| {
                    this.filter.set_window(window);
                    this.refresh_visible(cx);
                }),
            ));
        }

        row = row.child(div().w(px(1.)).h(px(16.)).bg(palette.border));

        // 左栏展示方式：平铺列表 / 以选中项所在需求为根的树。
        for mode in ListMode::ALL {
            let selected = self.list_mode == mode;
            row = row.child(chip(
                mode.label(),
                selected,
                palette,
                cx.listener(move |this, _: &ClickEvent, _window, cx| {
                    this.list_mode = mode;
                    this.ensure_tree(cx);
                    cx.notify();
                }),
            ));
        }

        row.child(div().flex_1()).child(
            div()
                .text_xs()
                .text_color(palette.muted_foreground)
                .child(self.list_caption()),
        )
    }

    /// 筛选行右侧的说明：列表看命中数，树看分组和行数。
    fn list_caption(&self) -> String {
        match self.list_mode {
            ListMode::Flat => format!(
                "{} · 命中 {}",
                window_caption(self.filter.window()),
                self.visible.len()
            ),
            ListMode::Tree => {
                if !self.tree_ready() {
                    return "正在整理任务分组…".to_string();
                }
                format!(
                    "{} 组 · {} 行",
                    self.tree_roots().len(),
                    self.list_rows().len()
                )
            }
        }
    }

    // ----------------------------------------------------------------- list

    fn list_panel(&self, palette: Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let rows: Rc<Vec<TreeRow>> = Rc::new(self.list_rows());
        let sizes: Rc<Vec<Size<Pixels>>> = Rc::new(
            (0..rows.len())
                .map(|_| size(px(0.), px(ROW_HEIGHT)))
                .collect(),
        );
        let empty = rows.is_empty();

        v_flex()
            .w(px(LIST_WIDTH))
            .h_full()
            .min_h_0()
            .when(empty, |this| {
                this.child(
                    div()
                        .p_6()
                        .text_sm()
                        .text_color(palette.muted_foreground)
                        .child(if self.loading {
                            "正在从云效拉取工作项…"
                        } else if self.config_error.is_some() {
                            "没有数据：检查配置或点刷新"
                        } else if self.list_mode == ListMode::Tree {
                            if self.tree_ready() {
                                "当前筛选没有工作项"
                            } else {
                                "正在整理任务分组…"
                            }
                        } else if self.filter.window() != TimeWindow::All {
                            "这个时间段没有计划完成的工作项（切到「全部时间」看看）"
                        } else {
                            "当前范围没有工作项，换个类别或项目试试"
                        }),
                )
            })
            .when(!empty, |this| {
                this.child(v_virtual_list(
                    cx.entity(),
                    "task-rows",
                    sizes,
                    move |this, range, _window, cx| {
                        range
                            .map(|ix| this.render_row(ix, &rows, palette, cx))
                            .collect::<Vec<_>>()
                    },
                ))
            })
    }

    fn render_row(
        &self,
        index: usize,
        rows: &Rc<Vec<TreeRow>>,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(row) = rows.get(index) else {
            return div().into_any_element();
        };
        let id = row.id.clone();
        let depth = row.depth;
        let Some(item) = self.item_by_id(&id).cloned() else {
            return div().into_any_element();
        };
        let selected = self.selected.as_deref() == Some(item.id.as_str());
        let flat = self.list_mode == ListMode::Flat;
        let children = self.store.children_of(&item.id).len();
        let done_subtree = self.store.done_descendants(&item.id);
        let subtree = self.store.descendants_of(&item.id).len();
        let click_id = item.id.clone();
        // 树模式：已知有子项、或边还没拉过时都显示箭头，点了才请求。
        let tree_kids = if flat {
            Vec::new()
        } else {
            self.tree_children_of(&item.id)
        };
        let collapsed = self.collapsed.contains(&item.id);
        let show_chevron = !flat && (!tree_kids.is_empty() || self.tree_pending(&item.id));
        let toggle_id = item.id.clone();

        h_flex()
            .id(ElementId::Name(SharedString::from(format!("row-{}", item.id))))
            .h(px(ROW_HEIGHT))
            .w_full()
            .pl(px(12. + depth as f32 * 16.))
            .pr(px(12.))
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(palette.border)
            .when(selected, |this| this.bg(palette.selection))
            .hover(|this| this.bg(palette.muted))
            .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                this.select(click_id.clone(), cx);
            }))
            .when(show_chevron, |this| {
                this.child(
                    div()
                        .id(ElementId::Name(SharedString::from(format!(
                            "chev-{}",
                            item.id
                        ))))
                        .w(px(14.))
                        .h_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .child(
                            Icon::new(if collapsed {
                                IconName::ChevronRight
                            } else {
                                IconName::ChevronDown
                            })
                            .xsmall()
                            .text_color(palette.muted_foreground),
                        )
                        .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                            cx.stop_propagation();
                            if this.collapsed.remove(&toggle_id) {
                                this.ensure_edges(toggle_id.clone(), cx);
                            } else {
                                this.collapsed.insert(toggle_id.clone());
                            }
                            cx.notify();
                        })),
                )
            })
            .when(!flat && !show_chevron, |this| this.child(div().w(px(14.))))
            .child(
                div()
                    .w(px(3.))
                    .h(px(18.))
                    .rounded_full()
                    .bg(palette.status(&item)),
            )
            .child(
                div()
                    .w(px(84.))
                    .text_xs()
                    .font_medium()
                    .text_color(palette.muted_foreground)
                    .child(item.serial_number.clone()),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_0p5()
                    .child(
                        div()
                            .truncate()
                            .text_sm()
                            .child(item.subject.clone()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(palette.muted_foreground)
                            .child(format!(
                                "{} · {} · {}{}",
                                item.status,
                                if item.assignee.is_empty() {
                                    "未指派"
                                } else {
                                    item.assignee.as_str()
                                },
                                item.space_name,
                                if item.sprint.is_empty() {
                                    String::new()
                                } else {
                                    format!(" · {}", item.sprint)
                                }
                            )),
                    ),
            )
            .when(children > 0 && flat, |this| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(palette.muted_foreground)
                        .child(format!("子{children} {done_subtree}/{subtree}")),
                )
            })
            .into_any_element()
    }

    // --------------------------------------------------------------- detail

    fn detail_panel(&self, palette: Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match self.selected.as_ref().and_then(|id| self.item_by_id(id)) {
            Some(item) => self.detail_body(item, palette, cx).into_any_element(),
            None => div()
                .p_6()
                .text_sm()
                .text_color(palette.muted_foreground)
                .child("选中左侧一条工作项查看详情与关联")
                .into_any_element(),
        };

        v_flex()
            .flex_1()
            .h_full()
            .min_w_0()
            .min_h_0()
            .overflow_y_scrollbar()
            .child(body)
    }

    fn detail_body(&self, item: &Item, palette: Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let url = item.url();
        // 关系是导航信息，不跟时间窗走；时间窗只筛左侧列表。
        let parent = item.parent_id.as_deref().and_then(|id| self.item_by_id(id));
        let ancestors: Vec<&Item> = item
            .id_path
            .iter()
            .filter(|id| id.as_str() != item.id)
            .filter_map(|id| self.item_by_id(id))
            .filter(|ancestor| parent.map(|p| p.id.as_str()) != Some(ancestor.id.as_str()))
            .collect();
        let children = self.store.children_of(&item.id);
        // 关联工作项（双向）：任务 ↔ 需求。
        let own_relations = self.relations.get(&item.id);
        let related: Vec<&Item> = match own_relations {
            Some(RelState::Ready(records)) => records
                .iter()
                .filter_map(|record| self.item_by_id(&record.resource_id))
                .collect(),
            _ => Vec::new(),
        };
        let related_pending = match own_relations {
            Some(RelState::Loading) => true,
            Some(RelState::Ready(records)) => records
                .iter()
                .any(|record| self.item_by_id(&record.resource_id).is_none()),
            _ => false,
        };
        // 兄弟 = 同父级 + 同一关联需求下的其他任务（后者才是「项目B」这类项目的常态）。
        let mut siblings: Vec<&Item> = if item.parent_id.is_some() {
            self.store.siblings_of(&item.id)
        } else {
            Vec::new()
        };
        if let Some(RelState::Ready(records)) = own_relations {
            for relation in records.iter().filter(|r| r.resource_type == "Req") {
                if let Some(RelState::Ready(tasks)) = self.relations.get(&relation.resource_id) {
                    for task in tasks
                        .iter()
                        .filter(|r| r.resource_type == "Task" && r.resource_id != item.id)
                    {
                        if let Some(sibling) = self.item_by_id(&task.resource_id) {
                            if !siblings.iter().any(|known| known.id == sibling.id) {
                                siblings.push(sibling);
                            }
                        }
                    }
                }
            }
        }
        let descendants = self.store.descendants_of(&item.id);
        let child_count = children.len();
        let sibling_count = siblings.len();
        let descendant_count = descendants.len();

        debug_log!(
            "详情 {}：parent_id={:?} 父级{} 祖先{} 关联{} 子级{} 兄弟{} 下级{}",
            item.serial_number,
            item.parent_id,
            parent.map(|p| p.serial_number.as_str()).unwrap_or("无"),
            ancestors.len(),
            related.len(),
            child_count,
            sibling_count,
            descendant_count
        );

        let mut body = v_flex()
            .w_full()
            .p_5()
            .gap_4()
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(
                                div()
                                    .text_xs()
                                    .font_medium()
                                    .text_color(palette.muted_foreground)
                                    .child(item.serial_number.clone()),
                            )
                            .child(status_pill(&item.status, palette.status(item), palette))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(palette.muted_foreground)
                                    .child(item.workitem_type.clone()),
                            ),
                    )
                    .child(
                        div()
                            .text_xl()
                            .font_semibold()
                            .child(item.subject.clone()),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("open-browser")
                            .small()
                            .primary()
                            .icon(IconName::ExternalLink)
                            .label("在浏览器打开")
                            .on_click(move |_, _, cx| cx.open_url(&url)),
                    )
                    .child({
                        let serial = item.serial_number.clone();
                        Button::new("copy-serial")
                            .small()
                            .icon(IconName::Copy)
                            .label("复制编号")
                            .on_click(move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(serial.clone()));
                            })
                    }),
            )
            .child(self.description_section(palette))
            .child(self.meta_grid(item, palette));

        if parent.is_some() || !ancestors.is_empty() {
            let mut block = v_flex().gap_1().child(section_title("父级", palette));
            if let Some(parent) = parent {
                block = block.child(relation_list(vec![parent], Relation::Parent, palette, cx, 1));
            }
            if !ancestors.is_empty() {
                block = block.child(ancestor_path(ancestors, palette, cx));
            }
            body = body.child(block);
        } else if item.parent_id.is_some() {
            let state = item.parent_id.as_deref().and_then(|id| self.foreign.get(id));
            let hint = match state {
                Some(ForeignState::Failed(err)) => format!("父项读取失败：{err}"),
                _ => "正在从云效读取父项…".to_string(),
            };
            body = body.child(div().text_xs().text_color(palette.muted_foreground).child(hint));
        }

        if !related.is_empty() || related_pending {
            let title = if related.is_empty() {
                "关联工作项".to_string()
            } else {
                format!("关联工作项 {} 条", related.len())
            };
            let mut block = v_flex().gap_1().child(section_title(&title, palette));
            if !related.is_empty() {
                block = block.child(relation_list(
                    related,
                    Relation::Parent,
                    palette,
                    cx,
                    MAX_RELATED_ROWS,
                ));
            }
            if related_pending {
                block = block.child(
                    div()
                        .text_xs()
                        .text_color(palette.muted_foreground)
                        .child("正在读取关联工作项…"),
                );
            }
            body = body.child(block);
        } else if let Some(RelState::Failed(err)) = own_relations {
            body = body.child(
                div()
                    .text_xs()
                    .text_color(palette.muted_foreground)
                    .child(format!("关联工作项读取失败：{err}")),
            );
        }

        if child_count > 0 {
            body = body.child(
                v_flex()
                    .gap_1()
                    .child(section_title(&format!("子级 {child_count} 条"), palette))
                    .child(relation_list(
                        children,
                        Relation::Child,
                        palette,
                        cx,
                        MAX_RELATED_ROWS,
                    )),
            );
        } else if descendant_count > 0 {
            body = body.child(
                v_flex()
                    .gap_1()
                    .child(section_title(
                        &format!("全部下级 {descendant_count} 条"),
                        palette,
                    ))
                    .child(relation_list(
                        descendants,
                        Relation::Child,
                        palette,
                        cx,
                        MAX_RELATED_ROWS,
                    )),
            );
        }

        if sibling_count > 0 {
            body = body.child(
                v_flex()
                    .gap_1()
                    .child(section_title(&format!("兄弟 {sibling_count} 条"), palette))
                    .child(relation_list(
                        siblings,
                        Relation::Sibling,
                        palette,
                        cx,
                        MAX_SIBLING_ROWS,
                    )),
            );
        } else {
            body = body.child(
                div()
                    .text_xs()
                    .text_color(palette.muted_foreground)
                    .child(if item.parent_id.is_some() {
                        "同一父级下没有其他兄弟"
                    } else {
                        "没有兄弟任务（同一父级或同一关联需求下只有自己）"
                    }),
            );
        }

        body
    }

    fn description_section(&self, palette: Palette) -> AnyElement {
        let state = self.selected.as_ref().and_then(|id| self.descriptions.get(id));
        let body = match state {
            Some(DescState::Ready(text)) if !text.trim().is_empty() => markdown_body(text, palette),
            Some(DescState::Ready(_)) => div()
                .text_sm()
                .text_color(palette.muted_foreground)
                .child("无描述")
                .into_any_element(),
            Some(DescState::Failed(err)) => div()
                .text_xs()
                .text_color(palette.muted_foreground)
                .child(format!("描述加载失败：{err}"))
                .into_any_element(),
            _ => div()
                .text_xs()
                .text_color(palette.muted_foreground)
                .child("正在加载描述…")
                .into_any_element(),
        };

        v_flex()
            .gap_1()
            .child(section_title("描述", palette))
            .child(div().w_full().max_h(px(360.)).overflow_y_scrollbar().child(body))
            .into_any_element()
    }

    fn meta_grid(&self, item: &Item, palette: Palette) -> impl IntoElement {
        let pairs: Vec<(&str, String)> = vec![
            ("状态", item.status.clone()),
            ("负责人", or_dash(&item.assignee)),
            ("项目", or_dash(&item.space_name)),
            ("迭代", or_dash(&item.sprint)),
            ("优先级", or_dash(&item.priority)),
            ("计划", plan_range(item)),
            ("工时 预计/实际", hours(item)),
            ("更新时间", format_time(item.updated_at)),
        ];

        v_flex().gap_1().children(
            pairs
                .into_iter()
                .map(|(label, value)| {
                    h_flex()
                        .gap_2()
                        .items_start()
                        .child(
                            div()
                                .w(px(104.))
                                .text_xs()
                                .text_color(palette.muted_foreground)
                                .child(label.to_string()),
                        )
                        .child(div().flex_1().min_w_0().text_xs().child(value))
                        .into_any_element()
                })
                .collect::<Vec<_>>(),
        )
    }

    // ----------------------------------------------------------- status bar

    fn status_bar(&self, palette: Palette) -> impl IntoElement {
        let left = match (&self.config_error, self.loading) {
            (Some(err), _) => format!("错误：{err}"),
            (None, true) => "正在同步…".to_string(),
            (None, false) if self.store.is_empty() => "尚无数据".to_string(),
            (None, false) => format!(
                "最近同步 {} · {} 项 · {}",
                format_time(self.store.fetched_at()),
                self.pages,
                window_caption(self.filter.window())
            ),
        };
        let source = self
            .settings
            .as_ref()
            .map(|settings| settings.token_source().to_string())
            .unwrap_or_else(|| "未加载配置".to_string());

        h_flex()
            .w_full()
            .px_4()
            .py_1()
            .gap_2()
            .items_center()
            .border_t_1()
            .border_color(palette.border)
            .text_xs()
            .text_color(palette.muted_foreground)
            .child(div().flex_1().min_w_0().truncate().child(left))
            .child(div().truncate().child(format!("令牌：{source}")))
    }
}

impl Render for Dashboard {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some((spaces, assignees)) = self.pending_options.take() {
            self.space_select
                .update(cx, |state, cx| state.set_items(spaces, window, cx));
            self.assignee_select
                .update(cx, |state, cx| state.set_items(assignees, window, cx));

            if !self.defaults_applied {
                self.defaults_applied = true;
                self.align_selects(window, cx);
            }
        }

        let palette = Palette::of(cx.theme());

        v_flex()
            .size_full()
            .bg(palette.background)
            .text_color(palette.foreground)
            .child(self.header(palette, cx))
            .child(self.filter_bar(palette, cx))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .child(self.list_panel(palette, cx))
                    .child(div().w(px(1.)).h_full().bg(palette.border))
                    .child(self.detail_panel(palette, cx)),
            )
            .child(self.status_bar(palette))
    }
}

// ---------------------------------------------------------------- helpers

fn chip(
    label: &str,
    selected: bool,
    palette: Palette,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    div()
        .id(ElementId::Name(SharedString::from(format!("chip-{label}"))))
        .px_2()
        .py_0p5()
        .rounded(px(999.))
        .text_xs()
        .cursor_pointer()
        .border_1()
        .border_color(if selected { palette.primary } else { palette.border })
        .bg(if selected {
            palette.primary
        } else {
            palette.background
        })
        .text_color(if selected {
            palette.primary_foreground
        } else {
            palette.muted_foreground
        })
        .hover(|this| this.opacity(0.85))
        .child(label.to_string())
        .on_click(on_click)
        .into_any_element()
}

fn status_pill(label: &str, color: Hsla, palette: Palette) -> AnyElement {
    h_flex()
        .gap_1()
        .items_center()
        .px_2()
        .py_0p5()
        .rounded(px(999.))
        .bg(palette.muted)
        .child(div().size(px(6.)).rounded_full().bg(color))
        .child(div().text_xs().child(label.to_string()))
        .into_any_element()
}

fn section_title(label: &str, palette: Palette) -> AnyElement {
    div()
        .text_xs()
        .font_medium()
        .text_color(palette.muted_foreground)
        .child(label.to_string())
        .into_any_element()
}

/// 直接父级之上的祖先，用一排小 chip 表示，保留跨层级跳转。
fn ancestor_path(
    ancestors: Vec<&Item>,
    palette: Palette,
    cx: &mut Context<Dashboard>,
) -> AnyElement {
    let mut row = h_flex()
        .flex_wrap()
        .gap_1()
        .items_center()
        .child(
            div()
                .text_xs()
                .text_color(palette.muted_foreground)
                .child("上级"),
        );

    for ancestor in ancestors {
        let id = ancestor.id.clone();
        row = row.child(
            div()
                .id(ElementId::Name(SharedString::from(format!(
                    "anc-{}",
                    ancestor.id
                ))))
                .px_2()
                .py_0p5()
                .rounded(px(4.))
                .bg(palette.muted)
                .text_xs()
                .cursor_pointer()
                .hover(|this| this.bg(palette.accent))
                .child(format!(
                    "{} {}",
                    ancestor.serial_number,
                    short(&ancestor.subject, 18)
                ))
                .on_click(
                    cx.listener(move |this, _: &ClickEvent, _window, cx| {
                        this.select(id.clone(), cx);
                    }),
                ),
        );
    }

    row.into_any_element()
}

/// 轻量 Markdown：标题、无序列表、代码块，行内加粗/行内代码/链接。
/// shortcut: 不处理表格、图片、引用和嵌套列表；真需要富文本再上渲染库。
fn markdown_body(text: &str, palette: Palette) -> AnyElement {
    let mut blocks: Vec<AnyElement> = Vec::new();
    let mut fence: Vec<&str> = Vec::new();
    let mut in_fence = false;

    for line in text.lines() {
        let trimmed = line.trim_end();
        if trimmed.trim_start().starts_with("```") {
            if in_fence {
                blocks.push(code_block(&fence.join("\n"), palette));
                fence.clear();
            }
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            fence.push(trimmed);
            continue;
        }

        let content = trimmed.trim_start();
        if content.is_empty() {
            continue;
        }
        let indent = (trimmed.len() - content.len()) as f32;

        let hashes = content.chars().take_while(|c| *c == '#').count();
        if hashes > 0 && content[hashes..].starts_with(' ') {
            let label = content[hashes..].trim();
            let mut heading = div()
                .font_semibold()
                .pt_1()
                .child(inline_text(label));
            heading = match hashes {
                1 => heading.text_lg(),
                2 => heading.text_base(),
                _ => heading.text_sm(),
            };
            blocks.push(heading.into_any_element());
            continue;
        }

        if let Some(rest) = content
            .strip_prefix("- ")
            .or_else(|| content.strip_prefix("* "))
        {
            blocks.push(
                h_flex()
                    .gap_1()
                    .items_start()
                    .pl(px(indent))
                    .child(
                        div()
                            .text_sm()
                            .text_color(palette.muted_foreground)
                            .child("•"),
                    )
                    .child(div().flex_1().min_w_0().text_sm().child(inline_text(rest)))
                    .into_any_element(),
            );
            continue;
        }

        blocks.push(
            div()
                .text_sm()
                .pl(px(indent))
                .child(inline_text(content))
                .into_any_element(),
        );
    }

    if in_fence && !fence.is_empty() {
        blocks.push(code_block(&fence.join("\n"), palette));
    }

    v_flex().gap_1().children(blocks).into_any_element()
}

fn code_block(code: &str, palette: Palette) -> AnyElement {
    div()
        .w_full()
        .p_2()
        .rounded(px(4.))
        .bg(palette.muted)
        .text_xs()
        .font_family("monospace")
        .child(code.to_string())
        .into_any_element()
}

/// 去掉行内标记；加粗用 highlight 区间保留，换行仍由 gpui 正常处理。
fn strip_inline(text: &str) -> (String, Vec<std::ops::Range<usize>>) {
    let mut out = String::new();
    let mut bold = Vec::new();
    let mut rest = text;

    while let Some(ch) = rest.chars().next() {
        if let Some(after) = rest.strip_prefix("**") {
            if let Some(end) = after.find("**") {
                let start = out.len();
                out.push_str(&after[..end]);
                bold.push(start..out.len());
                rest = &after[end + 2..];
                continue;
            }
        }
        if let Some(after) = rest.strip_prefix('`') {
            if let Some(end) = after.find('`') {
                out.push_str(&after[..end]);
                rest = &after[end + 1..];
                continue;
            }
        }
        if let Some(after) = rest.strip_prefix('[') {
            if let Some(mid) = after.find("](") {
                if let Some(end) = after[mid + 2..].find(')') {
                    out.push_str(&after[..mid]);
                    rest = &after[mid + 2 + end + 1..];
                    continue;
                }
            }
        }
        out.push(ch);
        rest = &rest[ch.len_utf8()..];
    }

    (out, bold)
}

fn inline_text(text: &str) -> AnyElement {
    let (plain, bold) = strip_inline(text);
    if bold.is_empty() {
        return div().child(plain).into_any_element();
    }

    let highlights = bold.into_iter().map(|range| {
        (
            range,
            HighlightStyle {
                font_weight: Some(FontWeight::BOLD),
                ..Default::default()
            },
        )
    });
    div()
        .child(StyledText::new(plain).with_highlights(highlights))
        .into_any_element()
}

fn relation_list(
    items: Vec<&Item>,
    relation: Relation,
    palette: Palette,
    cx: &mut Context<Dashboard>,
    limit: usize,
) -> AnyElement {
    let total = items.len();
    let mut rows: Vec<AnyElement> = items
        .into_iter()
        .take(limit)
        .map(|item| relation_row(item, relation, palette, cx))
        .collect();
    if total > limit {
        rows.push(
            div()
                .px_2()
                .py_1()
                .text_xs()
                .text_color(palette.muted_foreground)
                .child(format!("还有 {} 条未展开（用筛选缩小范围）", total - limit))
                .into_any_element(),
        );
    }

    v_flex().w_full().gap_0p5().children(rows).into_any_element()
}

fn relation_row(
    item: &Item,
    relation: Relation,
    palette: Palette,
    cx: &mut Context<Dashboard>,
) -> AnyElement {
    let id = item.id.clone();
    let url = item.url();

    let _ = relation;

    h_flex()
        .id(ElementId::Name(SharedString::from(format!("rel-{}", item.id))))
        .w_full()
        .px_2()
        .py_1()
        .gap_2()
        .items_center()
        .rounded(px(4.))
        .hover(|this| this.bg(palette.muted))
        .child(div().size(px(6.)).rounded_full().bg(palette.status(item)))
        .child(
            div()
                .w(px(88.))
                .text_xs()
                .text_color(palette.muted_foreground)
                .child(item.serial_number.clone()),
        )
        .child(
            div()
                .id(ElementId::Name(SharedString::from(format!(
                    "rel-subject-{}",
                    item.id
                ))))
                .flex_1()
                .min_w_0()
                .truncate()
                .text_xs()
                .cursor_pointer()
                .child(item.subject.clone())
                .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                    this.select(id.clone(), cx);
                })),
        )
        .child(
            div()
                .text_xs()
                .text_color(palette.status(item))
                .child(item.status.clone()),
        )
        .child(
            div()
                .id(ElementId::Name(SharedString::from(format!("rel-open-{}", item.id))))
                .px_1()
                .rounded(px(4.))
                .cursor_pointer()
                .child(Icon::new(IconName::ExternalLink).xsmall())
                .on_click(move |_, _, cx| cx.open_url(&url)),
        )
        .into_any_element()
}

fn category_label(category: &str) -> &str {
    match category {
        "Task" => "任务",
        "Req" => "需求",
        "Bug" => "缺陷",
        other => other,
    }
}

/// 筛选行右侧的窗口说明：`本周 11-09~11-15` / `全部时间`。
fn window_caption(window: TimeWindow) -> String {
    match window.date_bounds() {
        Some((from, to)) => format!("{} {}~{}", window.label(), short_day(&from), short_day(&to)),
        None => window.label().to_string(),
    }
}

/// `2026-11-09` → `11-09`。
fn short_day(day: &str) -> &str {
    day.get(5..).unwrap_or(day)
}

fn short(text: &str, max: usize) -> String {    if text.chars().count() <= max {
        text.to_string()
    } else {
        text.chars().take(max).collect::<String>() + "…"
    }
}

fn or_dash(value: &str) -> String {
    if value.is_empty() {
        "—".to_string()
    } else {
        value.to_string()
    }
}

fn plan_range(item: &Item) -> String {
    match (&item.plan_start, &item.plan_end) {
        (Some(start), Some(end)) => format!("{start} → {end}"),
        (Some(start), None) => start.clone(),
        (None, Some(end)) => format!("→ {end}"),
        (None, None) => "—".to_string(),
    }
}

fn hours(item: &Item) -> String {
    let planned = item.planned_hours.as_deref().unwrap_or("—");
    let actual = item.actual_hours.as_deref().unwrap_or("—");
    format!("{planned} / {actual}")
}

fn format_time(epoch_secs: i64) -> String {
    if epoch_secs <= 0 {
        return "—".to_string();
    }
    use chrono::{Local, TimeZone as _};
    match Local.timestamp_opt(epoch_secs, 0).single() {
        Some(dt) => dt.format("%m-%d %H:%M").to_string(),
        None => "—".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::strip_inline;

    #[test]
    fn strip_inline_keeps_bold_ranges_over_the_plain_text() {
        let (plain, bold) = strip_inline("范围＝**渠道配置**与 `代码`，见[文档](https://example.com)");
        assert_eq!(plain, "范围＝渠道配置与 代码，见文档");
        assert_eq!(bold.len(), 1);
        assert_eq!(&plain[bold[0].clone()], "渠道配置");
    }

    #[test]
    fn strip_inline_leaves_plain_text_alone() {
        let (plain, bold) = strip_inline("**没有闭合");
        assert_eq!(plain, "**没有闭合");
        assert!(bold.is_empty());
    }
}
