//! GPUI Kit desktop dashboard for Yunxiao work items.

mod dashboard;

use dashboard::Dashboard;
use gpui_kit::*;

fn main() {
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(|cx| {
            // Must run before any component state is created.
            gpui_kit::init(cx);

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
}
