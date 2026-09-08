//! Platinum のウィンドウシェード。PTY は畳む前のグリッド寸法を保つ。

use winit::dpi::{LogicalSize, PhysicalSize};
use winit::window::Window;

#[derive(Default)]
pub struct WindowShade {
    restore: Option<Restore>,
}

struct Restore {
    size: PhysicalSize<u32>,
    resizable: bool,
    maximized: bool,
}

impl WindowShade {
    pub fn is_shaded(&self) -> bool {
        self.restore.is_some()
    }

    pub fn toggle(&mut self, window: &Window, scale: f64, minimum: Option<LogicalSize<f64>>) {
        if let Some(restore) = self.restore.take() {
            window.set_min_inner_size(minimum);
            window.set_resizable(restore.resizable);
            let _ = window.request_inner_size(restore.size);
            if restore.maximized {
                window.set_maximized(true);
            }
        } else {
            let size = window.inner_size();
            self.restore = Some(Restore {
                size,
                resizable: window.is_resizable(),
                maximized: window.is_maximized(),
            });
            window.set_maximized(false);
            window.set_min_inner_size(Some(LogicalSize::new(1.0, 25.0)));
            window.set_resizable(false);
            let height = (25.0 * scale).round().max(1.0) as u32;
            let _ = window.request_inner_size(PhysicalSize::new(size.width, height));
        }
        window.request_redraw();
    }

    /// 別の解像度の画面へ動かしてもタイトルバーが切れないようにする。
    pub fn update_scale(&self, window: &Window, scale: f64) {
        if self.is_shaded() {
            let height = (25.0 * scale).round().max(1.0) as u32;
            let _ = window.request_inner_size(PhysicalSize::new(window.inner_size().width, height));
        }
    }
}
