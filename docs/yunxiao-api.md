# 云效 Projex 工作项接口参考

本文只记录**本仓库实际调用并验证过**的部分。字段名、哨兵值、URL 规则均来自实测，
改动前请用 `taskdash` CLI 复验，不要照抄官方文档的猜测量。

## 认证

| 项 | 值 |
|---|---|
| Base URL | `https://openapi-rdc.aliyuncs.com` |
| 认证头 | `x-yunxiao-token: <PAT>` |
| 组织 | 路径中的 `{org}`，来自配置 `yunxiao.org` |

令牌来源按优先级：配置 `yunxiao.pat` / `yunxiao.token` → 配置指向的 token 文件。
**令牌不进日志、不进 GUI。** GUI 状态栏只显示来源文件名。

## 检索工作项

```
POST /oapi/v1/projex/organizations/{org}/workitems:search
Content-Type: application/json
```

请求体（本仓库使用的最小可用集合）：

```json
{
  "category": "Task",
  "spaceId": "000000000000000000000000",
  "spaceType": "Project",
  "orderBy": "gmtCreate",
  "sort": "desc",
  "page": 1,
  "perPage": 100
}
```

响应外层包了 `result`：

```json
{ "result": [ {...}, {...} ], "page": 1, "perPage": 100, "total": 1432, "totalPages": 15 }
```

`unwrap_result()` 兼容"有包装"和"无包装"两种形态：有 `result` 就拆，没有就当整体。
分页元信息为 `page` / `perPage` / `total` / `totalPages`（注意是驼峰，与请求体的 `perPage` 一致，
而 `totalPages` 在响应里同样是驼峰）。

`category` 取值：`Task` / `Req` / `Bug`。本仓库默认只同步 `Task` + `Req`（见 `DEFAULT_CATEGORIES`）。

## 单个工作项

```
GET /oapi/v1/projex/organizations/{org}/workitems/{id}
```

返回单个对象（同样可能被 `result` 包裹）。用于 `taskdash show` 与详情补全。
**描述（`description`，Markdown）只有这个接口返回**：`workitems:search` 里该字段恒为空，
所以 GUI 在选中项时按 id 单独拉一次并缓存。

## 关键字段

| 字段 | 说明 |
|---|---|
| `id` | 工作项 ID（24 位十六进制），跳转 URL 用 |
| `serialNumber` | 人读编号，如 `ABC-832` |
| `subject` | 标题 |
| `status` / `statusId` | 状态名 / 状态 ID |
| `workitemType` | 工作项类型名（与 `category` 不同，别混用） |
| `space` | `{ id, name }` |
| `parentId` | **父项哨兵见下** |
| `idPath` | 祖先 ID 路径数组，可用于建层级 |
| `assignedTo` | `{ id, displayName }` |
| `sprint` | `{ id, name }`，可能为空 |
| `priority` | 优先级，可能为空 |

**父项哨兵：无父项时 `parentId` 是字符串 `EMPTY_VALUE`，不是 `null`。**
`RawItem` 在解析阶段把它转成 `None`，上层一律用 `Item.parent_id.is_none()` 判断。

`category` 不由响应体提供（`workitemType` 是另一个概念），而是由**抓取上下文**带入：
`sync()` 逐空间逐类别翻页时把当前 `category` 连同 `RawItem` 一起返回，
`Item::from_raw(category, raw)` 再写进模型。这是本项目踩过的一次语义错误，勿改回从 `workitemType` 推导。

## 浏览器跳转 URL

```
https://devops.aliyun.com/projex/project/{spaceId}/{category_lowercase}/{id}
```

实测证据（两种类别都在真实 Chrome 里打开确认过，标题与编号对得上）：

| 请求 | 打开后的页面标题 |
|---|---|
| `.../111111111111111111111111/req/aaaaaaaaaaaaaaaaaaaaaaaa` | 【2.6.0】报告管理 |
| `.../111111111111111111111111/task/bbbbbbbbbbbbbbbbbbbbbbbbbb` | 【2.6.0】【后端】报告记录 |

要点：
- 类别段是小写 `task` / `req` / `bug`，不是 `Task`。
- 用 `spaceId`，不是项目名。
- 该路由是 SPA，未登录会跳登录页；`curl` 无法验证，只能靠真实浏览器。

## 已探明的空间（本机配置内）

| 名称 | spaceId |
|---|---|
| 项目A | `111111111111111111111111` |
| 项目B | `000000000000000000000000` |
| 项目C | `222222222222222222222222` |
| 项目D | `333333333333333333333333` |

空间列表以配置 `yunxiao.spaces` 为准；上表只是排查时的对照。

## 按范围拉取与实测规模

GUI 启动只拉当前范围：`(项目集合, 类别, 负责人)`。

- `Client::sync_view(spaces, category, assignee, per_page, on_page)` 对每个空间 × 单个类别翻页，
  按 `totalPages` 终止，每页回调 `on_page(累计页数)`。
- 负责人下推到服务端，`conditions` 是 **JSON 字符串**（不是对象）：

  ```json
  {"conditionGroups": [[{"fieldIdentifier": "assignedTo", "operator": "CONTAINS",
    "value": ["<user_id>"], "toValue": null, "className": "user", "format": "list"}]]}
  ```

- 项目名用 `GET /projects/{id}` 逐个取（配置里只有 ID），不依赖全量数据。

实测（`perPage = 100`）：

| 范围 | 项数 | 耗时 |
|---|---|---|
| 项目B × Task × 我 | 数十项 | 亚秒级 |
| 项目B × Task | 数页 | 亚秒级 |
| 多个空间 × {Task, Req, Bug}（`taskdash sync` 全量） | 数千项 | 数十秒 |

全量只保留在 CLI（`taskdash sync`）以及 `--space`/`--category` 未同时给出时使用；
GUI 不再全量同步，因此关系链只在加载范围内自洽。

## 时间段条件（实测，2026-11）

分页总数在响应头 `X-Total` / `X-Total-Pages`，响应体是裸数组。

| 写法 | 结果 |
|---|---|
| `updateStatusAt` + `BETWEEN` + `format:"dateRange"` + `["2026-11-09 00:00:00"]` | ✅ 生效 |
| 同上给两个值（带结束时间） | ⚠️ 只认第一个值，结束时间被忽略 |
| 同上但 `GT` / `LT` / `format:"singleDate"` / 毫秒数 | ❌ 静默忽略，返回全量 |
| `gmtCreate` / `gmtModified` | ❌ 静默忽略，返回全量 |
| 计划开始/完成、实际开始/结束（customField `79`/`80`/…） | ❌ 0 项或忽略，不可用于条件 |

因此本仓库的时间段一律在本地按 `计划完成时间` 过滤（`TimeWindow`），不下推。

## 工作项关联项（实测，2026-11）

```
GET /oapi/v1/projex/organizations/{org}/workitems/{id}/relationRecords?relationType=ASSOCIATED
```

- `relationType` 必填，实测支持 `ASSOCIATED` / `PARENT` / `SUB`；`DEPENDENT`、`CHILD`、`RELATED` 报「不支持该关联类型」。
- 返回 `[{id, relationType, resourceType, resourceId, gmtCreate}]`，`resourceType` 是 `Task` / `Req` / `Bug`。
- **关联是双向的**：任务 `XYZ-228` → 需求 `ABC-832`；反过来查 `ABC-832` 的 `ASSOCIATED`，得到它的全部 4 个关联任务。
  这是「同一需求下的兄弟任务」的唯一来源，`parentId` 在这类项目里全是 `EMPTY_VALUE`。
- 这个接口不吃 `spaceId`/`category`，跨项目、跨类别都能取到，正好用来补当前加载范围之外的数据。
- `PARENT` / `SUB` 给的是真实的父项 / 子项，同样跨项目：
  `ABC-832`（需求）`SUB` → `ABC-835`（任务，`parentId` 也指向它）；
  `ABC-792`（项目A的需求）`SUB` → 两个其他项目的任务。
- 关系记录只有 `resourceId` 和 `resourceType`，要拿标题/状态得逐条 `GET /workitems/{id}`。
  `workitems:search` 必须带 `spaceId`，不能按 id 批量查（传 id 条件报「项目id或项目集id不能为空」）。

## 关系查询（本地计算，不额外发请求）

同步完成后 `Store` 用 `parent_id` 建索引：

| 方法 | 语义 |
|---|---|
| `children_of(id)` | 直接子项 |
| `parent_of(id)` | 直接父项（可能不在同步范围内 → `None`） |
| `siblings_of(id)` | 同父兄弟；**根级项共享"无父"分组，可能数百条** |
| `ancestors_of(id)` | 从根到父的链路，供面包屑 |
| `descendants_of(id)` | 全部下级，递归 |
| `done_descendants(id)` | 下级中已完成计数，列表显示 `子N 完成/总数` |

因为不依赖 `parentId` 服务端过滤，父子链只在同步范围内自洽；父项属于未同步的项目/类别时，
GUI 用 `GET /workitems/{id}` 按 id 补拉一次（不带项目/类别条件），只用于展示。
注意详情接口的 `idPath` 常为 `null`，跨项目的多层祖先只能靠子项自己搜索响应里的 `idPath`。

## 描述字段的两种格式（实测）

| 类别 | `description` 形态 |
|---|---|
| 任务（Task） | Markdown 原文，如 `## 背景\n\n报告管理 PRD…` |
| 需求（Req） | `{"htmlValue":"<article>…","jsonMLValue":[…]} `，需先解 HTML |
| 缺陷（Bug） | 同需求，HTML 包裹 |

`taskdash_core::load_description` 已把 HTML 变体转成纯文本（块级标签换行、`<li>` → `- `、解常见实体）。
