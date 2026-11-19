# task-dashboard

阿里云云效（Projex）工作项的桌面任务看板。Rust + gpui-kit 0.9.1。

看板回答三个问题：**我现在有哪些活（筛选）**、**这条活挂在谁的下面（父子兄弟）**、**去云效看原始记录（浏览器跳转）**。

## 快速开始

```bash
cd ~/projects/task-dashboard
cargo build -p taskdash-gui

# 非桌面会话的 shell 用脚本启动，自动补 Wayland 环境变量
./run-gui.sh
```

无窗口验证数据与关系：

```bash
./target/debug/taskdash sync
./target/debug/taskdash list --status active
./target/debug/taskdash show ABC-832
```

## 界面

- 顶部：关键词搜索、类别下拉（任务 / 需求 / 缺陷）、项目下拉、负责人下拉、刷新
- 筛选行：未完成 / 已完成 / 全部（带计数）、全部时间 / 本周 / 近一月（按计划完成时间，默认本周）
- 左栏：虚拟滚动列表，状态色条 + 编号 + 标题 + 状态/负责人/项目 + 子项完成比；
  筛选行可切「列表 / 任务树」，任务树把当前结果铺成森林（需求分组 + 无需求任务独立 + 任务下挂缺陷），箭头折叠/展开
- 右栏：选中项详情、描述（轻量 Markdown）、父级 + 上级链、关联工作项、子级 / 全部下级、兄弟，每行可直接跳转或点箭头用浏览器打开
- 底栏：同步时间、令牌来源（不显示令牌本身）

项目 / 类别 / 负责人任一变化都会按新范围重新拉取（负责人下推到服务端），
拉过的范围在内存里缓存，切回来瞬时响应。父项不在当前范围（比如父需求在别的项目）时，
按 id 单独拉一次 `GET /workitems/{id}` 补上，只用于展示，点它直接开浏览器。

时间段是**本地过滤**：取 `计划完成时间`（customField `80`）落在窗口内的项，
切「全部时间 / 本周 / 近一月」只重算左侧列表，不发请求。关系区不跟时间窗。

描述不在搜索结果里，选中工作项时单独拉一次 `GET /workitems/{id}` 并缓存；
任务的描述是 Markdown，需求/缺陷的是 `{"htmlValue": "<article>…"}`，会先转成纯文本。
「软件部」这类项目没有父子关系，层级来自任务与需求的**关联项**：
选中项会拉 `relationRecords?relationType=ASSOCIATED`，显示关联工作项，并把同一需求下的其他任务列为兄弟。
关联是双向的，所以从需求上也能反查它的全部任务。

「任务树」左栏把当前筛选结果全部铺成森林：每条任务向上找根（优先 `parent_id`，否则唯一关联的需求/任务），
有需求的归到需求下、没有需求的任务自己当顶级行；任务下展开关联缺陷（需求下也会带出同需求的其他任务）。
树不受状态/时间/负责人筛选影响，只看当前结果集；可见任务超过 40 条时不自动分组，只展开选中项那条链。

## 配置

复用本机既有凭据，不新增配置文件：`~/.config/dashboard/config.json` 的 `yunxiao` 段
（`org_id` / `project_id` / `token` 或 token 文件 / 可选 `user_id` 供「只看我的」使用）。

task-dashboard 自己的可选键（不改动同段里其他程序用的 `categories`、`refresh_interval`）：

- `taskdash_categories`：类别下拉的候选，默认 `["Task", "Req"]`，例如 `["Task", "Req", "Bug"]`；启动默认选第一个
- `taskdash_default_space`：启动时默认选中的项目 ID（点「全部项目」可解除）
- `taskdash_done_statuses`：视为「已完成」的状态关键词，默认 `["完成", "关闭", "取消"]`
- `taskdash_info_statuses` / `taskdash_warning_statuses`：状态点的配色分组，决定哪些状态显示蓝点/黄点（其余为绿点）
- `taskdash_custom_fields`：自定义字段的可识别名称，键为 `priority` / `planned_hours` / `actual_hours` / `plan_start` / `plan_end`，值为名称或字段 ID 数组；字段名跟租户走，按需覆盖。

状态分组和字段名都在本地算，不发额外请求。

启动默认还会打开「只看我的」，条件是配置里有 `user_id`。

## 文档

- `AGENTS.md` —— 在本仓库工作的约定与已知坑（改代码前必读）
- `docs/yunxiao-api.md` —— 云效接口与跳转 URL 规则
- `docs/gpui-kit-notes.md` —— gpui-kit API 用法与踩坑
