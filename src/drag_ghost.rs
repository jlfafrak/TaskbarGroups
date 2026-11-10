use eframe::egui::ColorImage;
use windows::{
    Win32::{
        Foundation::{COLORREF, HWND, POINT, SIZE},
        Graphics::Gdi::{
            AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION,
            CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, HGDIOBJ,
            SelectObject,
        },
        UI::WindowsAndMessaging::{
            CreateWindowExW, DestroyWindow, HWND_TOPMOST, SW_HIDE, SWP_NOACTIVATE, SWP_NOSIZE,
            SWP_SHOWWINDOW, SetWindowPos, ShowWindow, ULW_ALPHA, UpdateLayeredWindow,
            WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT,
            WS_POPUP,
        },
    },
    core::w,
};

pub struct DragGhost {
    hwnd: HWND,
    visible: bool,
}

impl DragGhost {
    #[cfg(test)]
    pub fn window_handle_for_test(&self) -> HWND {
        self.hwnd
    }
    pub fn new(image: &ColorImage, dpi: u32) -> Result<Self, String> {
        if image.size != [32, 32] || image.pixels.len() != 32 * 32 {
            return Err("Unexpected drag icon dimensions".into());
        }
        let side = ((32u32.saturating_mul(dpi.max(96)) + 48) / 96).clamp(32, 96) as i32;
        unsafe {
            let hwnd = CreateWindowExW(
                WS_EX_LAYERED
                    | WS_EX_TRANSPARENT
                    | WS_EX_NOACTIVATE
                    | WS_EX_TOOLWINDOW
                    | WS_EX_TOPMOST,
                w!("STATIC"),
                w!(""),
                WS_POPUP,
                -32000,
                -32000,
                side,
                side,
                None,
                None,
                None,
                None,
            )
            .map_err(|error| error.to_string())?;
            let result = (|| {
                let dc = CreateCompatibleDC(None);
                if dc.0.is_null() {
                    return Err("Could not create drag image context".to_owned());
                }
                let info = BITMAPINFO {
                    bmiHeader: BITMAPINFOHEADER {
                        biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                        biWidth: side,
                        biHeight: -side,
                        biPlanes: 1,
                        biBitCount: 32,
                        biCompression: BI_RGB.0,
                        ..Default::default()
                    },
                    ..Default::default()
                };
                let mut bits = std::ptr::null_mut();
                let bitmap =
                    match CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0) {
                        Ok(bitmap) => bitmap,
                        Err(error) => {
                            let _ = DeleteDC(dc);
                            return Err(error.to_string());
                        }
                    };
                let pixels =
                    std::slice::from_raw_parts_mut(bits as *mut u8, (side * side * 4) as usize);
                for (index, output) in pixels.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                    let x = index % side as usize * 32 / side as usize;
                    let y = index / side as usize * 32 / side as usize;
                    let [r, g, b, a] = image.pixels[y * 32 + x].to_array();
                    output.copy_from_slice(&[b, g, r, a]);
                }
                let old = SelectObject(dc, HGDIOBJ(bitmap.0));
                let size = SIZE { cx: side, cy: side };
                let dest = POINT {
                    x: -32000,
                    y: -32000,
                };
                let origin = POINT::default();
                let blend = BLENDFUNCTION {
                    BlendOp: AC_SRC_OVER as u8,
                    BlendFlags: 0,
                    SourceConstantAlpha: 255,
                    AlphaFormat: AC_SRC_ALPHA as u8,
                };
                let result = UpdateLayeredWindow(
                    hwnd,
                    None,
                    Some(&dest),
                    Some(&size),
                    Some(dc),
                    Some(&origin),
                    COLORREF(0),
                    Some(&blend),
                    ULW_ALPHA,
                )
                .map_err(|error| error.to_string());
                let _ = SelectObject(dc, old);
                let _ = DeleteObject(HGDIOBJ(bitmap.0));
                let _ = DeleteDC(dc);
                result
            })();
            if let Err(error) = result {
                let _ = DestroyWindow(hwnd);
                return Err(error);
            }
            Ok(Self {
                hwnd,
                visible: false,
            })
        }
    }

    pub fn show_at(&mut self, point: POINT) {
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                point.x.saturating_add(12),
                point.y.saturating_add(16),
                0,
                0,
                SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
        }
        self.visible = true;
    }

    pub fn hide(&mut self) {
        if self.visible {
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
            }
            self.visible = false;
        }
    }
}

impl Drop for DragGhost {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::Color32;
    use windows::Win32::UI::WindowsAndMessaging::{
        GWL_EXSTYLE, GetForegroundWindow, GetWindowLongPtrW, GetWindowRect, IsWindow,
        WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TRANSPARENT,
    };

    #[test]
    fn drag_image_is_click_through_non_activating_scaled_and_destroyed() {
        unsafe {
            let image = ColorImage::filled([32, 32], Color32::WHITE);
            let foreground = GetForegroundWindow();
            let mut ghost = DragGhost::new(&image, 144).unwrap();
            let hwnd = ghost.hwnd;
            let styles = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
            let required =
                WS_EX_LAYERED.0 | WS_EX_NOACTIVATE.0 | WS_EX_TOOLWINDOW.0 | WS_EX_TRANSPARENT.0;
            assert_eq!(styles & required, required);
            let mut rect = Default::default();
            GetWindowRect(hwnd, &mut rect).unwrap();
            assert_eq!(rect.right - rect.left, 48);
            ghost.show_at(POINT {
                x: -32000,
                y: -32000,
            });
            assert_eq!(GetForegroundWindow(), foreground);
            drop(ghost);
            assert!(!IsWindow(Some(hwnd)).as_bool());
        }
    }
}
