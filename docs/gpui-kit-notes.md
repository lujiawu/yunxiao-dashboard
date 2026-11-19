# gpui-kit 0.9.1 用法笔记

只记本仓库 GUI 层真正用到、且踩过坑的 API。上游仓库在本机 `~/github/gpui-kit`，
其 `skills/gpui-kit/SKILL.md` 是更全的索引。

## 应用引导

```rust
gpui_kit::application()
    .with_assets(gpui_kit::assets::Assets)
    .run(|cx| {
        gpui_kit::init(cx);                 // 必须早于任何组件状态创建
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::centered(size(px(1320.), px(880.)), cx)),
            ..Default::default()
        };
        gpui_kit::open_window(options, cx, |window, cx| {
            window.set_window_title("任务看板 · 云效");
            cx.new(|cx| Dashboard::new(window, cx))
        })
        .expect("failed to open window");
    });
```

## 必知的 trait 导入

方法"明明存在却找不到"九成是 trait 没进作用域。本项目的导入清单可直接复用：

```rust
use gpui_kit::component::{
    ActiveTheme, Disableable as _, Icon, IconName, IndexPath, Sizable, StyledExt as _, Theme,
    button::{Button, ButtonVariants as _},   // .primary() / .ghost() 需要 ButtonVariants
    input::{Input, InputEvent, InputState},
    scroll::ScrollableElement as _,          // .overflow_y_scrollbar()
    select::{Select, SelectEvent, SelectState},
    h_flex, v_flex, v_virtual_list,
};
use gpui_kit::prelude::FluentBuilder as _;   // .when() / .when_some()
use gpui_kit::*;                             // div、Hsla、px、Context、Render…
```

| 想用的方法 | 需要的 trait |
|---|---|
| `.font_medium()` / `.font_semibold()` | `StyledExt` |
| `.primary()` / `.ghost()` | `ButtonVariants` |
| `.disabled(bool)` | `Disableable` |
| `.when(cond, f)` / `.when_some(opt, f)` | `FluentBuilder` |
| `.overflow_y_scrollbar()` | `ScrollableElement` |
| `.text_sm()` / `.text_xs()` / `.small()` | `StyledExt` / `Sizable` |

## 主题色

`cx.theme()` 返回 `&Theme`（借自 cx），`Theme` 实现了 `Deref<Target = ThemeColor>`，
所以 `cx.theme().primary` 直接可用。可用键：`background` `foreground` `muted` `muted_foreground`
`border` `primary` `primary_foreground` `accent` `selection` `info` `warning` `success` `danger` `secondary`。

**不要把 `&Theme` 借进渲染闭包**——闭包要求 `'static`，借用会编译失败。
本项目的做法：`Palette`（`#[derive(Clone, Copy)]`，全是 `Hsla`）按值拷贝后传入，
render 里一句 `let palette = Palette::of(cx.theme());` 即可。

## 虚拟列表

```rust
let sizes: Rc<Vec<Size<Pixels>>> = Rc::new(
    (0..self.visible.len()).map(|_| size(px(0.), px(ROW_HEIGHT))).collect(),
);

v_virtual_list(
    cx.entity(),          // Entity<V>
    "task-rows",          // 稳定的元素 id
    sizes,                // 每项尺寸；宽度给 0 表示按可用宽度
    move |this, range, _window, cx| {       // 必须 move，且要求 'static
        range.map(|ix| this.render_row(ix, palette, cx)).collect::<Vec<_>>()
    },
)
```

坑：闭包不 `move` 时，捕获的 `palette` 被判定"活得不够久"（E0597）；加 `move` 并按值捕获即可。

## 交互元素

`.on_click(...)` / `.hover(...)` 只在**先调 `.id(...)` 的 stateful 元素**上解析。
普通 `div()` 直接挂 `.on_click` 会报 `no method named on_click found for Div`。
列表行、关系行里每个可点区域都要有自己的 `ElementId`（本项目用 `format!("row-{id}")` 这类稳定字符串）。

```rust
div()
    .id(ElementId::Name(SharedString::from(format!("rel-{id}"))))
    .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| { ... }))
```

`.tooltip()` 的参数是**构造闭包**而不是字符串：
`impl Fn(&mut Window, &mut App) -> AnyView`。只想挂纯文本的话，本项目直接放弃 tooltip，
避免为一个提示引入 `Tooltip::new(...).build(window, cx)` 的样板。

## 下拉选择（Select）

```rust
let state = cx.new(|cx| SelectState::new(
    vec![SharedString::from("全部项目")],
    Some(IndexPath::default()),
    window,
    cx,
));

// 渲染
Select::new(&state).small().title_prefix("项目").menu_width(px(220.))

// 选中事件（注意泛型就是选项类型）
cx.subscribe_in(&state, window, |this, _, event: &SelectEvent<Vec<SharedString>>, _w, cx| {
    if let SelectEvent::Confirm(Some(value)) = event { /* ... */ }
}).detach();
```

坑：选项是**构造时确定的**。数据同步完才有项目/负责人清单，所以需要 `set_items(items, window, cx)` ——
但它要 `&mut Window`，而异步回调里只有 `&mut App`。本项目用 `pending_options` 暂存，
在下一次 `render(&mut self, window, cx)` 开头 `take()` 出来应用（一次性，不会循环渲染）。

`Select` 本身没有 `.searchable()`；可搜索是 `SelectState` 构造时决定的。

## 文本输入

```rust
let input = cx.new(|cx| InputState::new(window, cx).placeholder("搜索标题 / 编号 / 负责人"));
Input::new(&input).small().cleanable(true)

cx.subscribe_in(&input, window, |this, _, event: &InputEvent, _w, cx| {
    if matches!(event, InputEvent::Change) {
        let value = this.input.read(cx).value().to_string();  // 先取值再改 self，避免借用冲突
        this.filter.set_keyword(value);
    }
}).detach();
```

## 异步同步与刷新

```rust
cx.spawn(async move |this, cx| {
    let result = cx.background_spawn(async move { load_store(&settings, |_| {}) }).await;
    this.update(cx, |this, cx| { /* 回到前台线程改 state */ }).ok();
}).detach();
```

`background_spawn` 要求 future `Send + 'static`：传进闭包的数据必须拥有所有权（`Settings` 是 `Clone`），
返回的 `Store` 也必须是 `Send`（HashMap + String，满足）。

## Cargo 配置

```toml
[profile.dev.package.gpui-pre]
opt-level = 3
# 同理：gpui-pre-platform / gpui-component / gpui-kit / rustybuzz / taffy / ttf-parser
```

不这么做 debug 构建界面卡到不可用。注意它**只改善运行速度**，编译时间不会下降（首次 2–3 分钟）。

## 本机运行

gpui 走 Wayland 客户端。KDE 会话下从普通 shell 启动必须补环境变量：

```bash
./run-gui.sh
```

脚本按 `XDG_RUNTIME_DIR` 下的 socket 自动推断 `WAYLAND_DISPLAY`（`wayland-0` / `wayland-1` 都可能）。
截图用 `spectacle -b -n -f -o /tmp/shot.png`。
