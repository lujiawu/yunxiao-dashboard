# AGENTS.md

给在本仓库工作的 agent（含未来的我）的操作手册。**先读这里，再动代码。**

## 项目定位

云效工作项的任务看板：列表 + 筛选 + 选中项查看父/子/兄弟关系 + 一键在浏览器打开。
Rust workspace，`core` 无 UI 依赖、`gui` 只负责渲染，两者通过 `taskdash-core` 的公开类型通信。

## 常用命令

```bash
cd ~/projects/task-dashboard

cargo build -p taskdash-gui              # GUI（首次约 2-3 min，之后增量秒级）
cargo test  -p taskdash-core             # core 单测
cargo build -p taskdash-core --bin taskdash

# 无窗口验证（不需要 X/Wayland，排查数据问题优先用它）
./target/debug/taskdash sync                       # 全量拉取 + 打印统计（≈25 s）
./target/debug/taskdash list --space 项目B --category Task --assignee <user_id>  # 范围拉取（≈1 s）
./target/debug/taskdash list --status active       # 未完成列表
./target/debug/taskdash list --space 项目B --category Task --status all --window week  # 本周计划完成的
./target/debug/taskdash list --space 项目B --keyword 报告
./target/debug/taskdash show ABC-832              # 单个工作项 + 父子兄弟
```

跑 GUI 需要桌面会话环境变量（KDE Wayland）；从非会话 shell 启动用仓库里的 `./run-gui.sh`，
它自动按 `XDG_RUNTIME_DIR` 下的 socket 推断 `WAYLAND_DISPLAY`（值可能是 `wayland-0` 或 `wayland-1`）。

## 结构

```
crates/core/          taskdash-core（无 UI 依赖）
  src/config.rs       读 ~/.config/dashboard/config.json 的 yunxiao 段 + token 文件
  src/yunxiao.rs      REST 客户端、分页同步、RawItem 原始结构
  src/model.rs        Item / Store / Relation：内存索引与父子兄弟查询
  src/filter.rs       Filter + StatusGroup：筛选与状态分组
  src/bin/taskdash.rs CLI（sync / list / show）
crates/gui/           taskdash-gui（gpui-kit 0.9.1）
  src/main.rs         应用引导、窗口创建
  src/dashboard.rs    Dashboard 视图：筛选栏、虚拟列表 / 任务树、详情关系面板
```

## 硬性约定

- **令牌永不打印/落日志。** `config.rs` 与 `yunxiao.rs` 里所有涉及 token 的路径都不 `Debug` 输出它；GUI 状态栏只显示令牌**来源**（如 `dashboard/config.json`），不显示值。改动这两个文件时保持这条。
- **不新增凭据文件。** 复用 `~/.config/dashboard/config.json` 的 `yunxiao` 段与既有 token 文件，dashboard 自己不引入 `.env` 或第二套配置。
- **按范围拉取 + 内存索引。** GUI 只拉 `(项目集合, 类别, 负责人)` 这一份数据，范围作缓存键切回即命中。
  不做增量、不做本地缓存持久化；`taskdash sync` 保留全量用于排查。要改成常驻全量先和用户确认。
- **时间段在本地过滤。** 用 `计划完成时间`（customField `80`）比对 `TimeWindow`，
  不要下推服务端：实测只有 `updateStatusAt` 能按时间段筛，且只认下界（见 `docs/yunxiao-api.md`）。
- **中文 UI 文案 + 中文注释。**
- **core 不引入 UI 依赖**，gui 不直接调 HTTP。任何"我想在 gui 里顺手请求一下"都放回 core。
- **关系列表必须限长**（子级/全部下级 `MAX_RELATED_ROWS = 12`，兄弟 `MAX_SIBLING_ROWS = 8`）。
  根级工作项之间不是兄弟，「兄弟」只在有父项时显示，否则会把全体根项（实测 772 条）当兄弟刷屏。

## 已知坑

- `parentId` 的无父项哨兵是字符串 `EMPTY_VALUE`，不是 `null`。用 `Item.parent_id.is_none()` 判断。
- 描述有两种格式：任务（Task）是 Markdown，需求/缺陷是 `{"htmlValue":"<article>…"}`；
  `load_description` 负责把后者转纯文本。搜索结果里 `description` 恒为空，必须走单条接口。
- 父项可能在其他项目（如 `XYZ-25` 的父需求 `ABC-792` 在项目A），不在加载范围时按 id 补拉
  （`load_foreign_item`，不带项目/类别条件）；详情接口的 `idPath` 常为 `null`，跨项目祖先链只能靠子项自己的 `idPath`。
- **「项目B」这类项目没有父子关系**：任务全挂在 `parentId: EMPTY_VALUE`，真正的层级是「任务 ↔ 需求」的
  关联项（`relationRecords?relationType=ASSOCIATED`）。兄弟任务 = 同一需求下的其他关联任务，必须查接口，本地 `siblings_of` 查不到。
- **真实父子走 `SUB` / `PARENT` 关系接口**，跨项目：`ABC-832 SUB` → `ABC-835`；`ABC-792 SUB` → 别的项目的任务。
  关系记录只给 id 和 `resourceType`，标题要逐条 `GET /workitems/{id}`——`workitems:search` 必须带 `spaceId`，
  不能按 id 批量查。所以「任务树」展开一个节点是「2 次关系接口 + N 次单条详情」，只对展开的节点发。
- **任务树是森林，不是单棵树**：每条筛选结果各自上溯取根（`parent_id` → 唯一关联的需求/任务 → 自己），
  按第一次出现的顺序去重成顶级行——需求行落在它第一个任务的位置，没有需求的任务自己就是顶级。
  子项 = `SUB` + 关联项，需求挂任务和缺陷、任务只挂缺陷、缺陷只挂子缺陷。任务之间的关联是平级，
  铺进树会刷屏，不展开。
- **树的数据要控量**：可见任务 ≤ `TREE_GROUP_LIMIT`（40）才自动分组，每个任务一次 `ASSOCIATED`、
  非可见根再一次 `SUB`；超过就只展开选中项那条链。请求走 `relation_queue` / `foreign_queue`，
  并发上限 `NET_CONCURRENCY`（3）——别改回"一个 item 一个 spawn"，切树会瞬间打几十个请求。
- 树里范围外的项（跨项目需求/缺陷）补拉时带 `resourceType` 作为 `category`，浏览器链接的类别段靠它；
  从 `parent_id` 补拉的祖先没有类型信息，链接会缺类别段。
- 搜索结果外层包了 `{"result": ...}`，用 `unwrap_result()` 拆；有的接口不包，函数已兼容两种情况。
- gpui-kit 的 `.on_click` 只在 **`.id()` 之后的 stateful 元素**上可用，普通 `div()` 直接挂会报 method not found。
- 传给 `v_virtual_list` 的渲染闭包要求 `'static`，捕获的主题色结构体必须是 `Copy` 并按值 `move` 进去。
- `profile.dev` 里对 gpui 系依赖开了 `opt-level = 3`，否则 debug 渲染卡到不可用；这**不改善**编译速度。
- CLI 里恢复了 `SIGPIPE` 默认行为，所以 `taskdash list | head` 不会再 panic。

## 详细资料

- `docs/yunxiao-api.md` —— 云效 Projex 接口、字段、跳转 URL 规则（含实测证据）
- `docs/gpui-kit-notes.md` —— gpui-kit 0.9.1 在本项目用到的 API 与踩坑
