use eframe::egui::Color32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContrastColors {
    pub background: Color32,
    pub text: Color32,
    pub surface: Color32,
    pub accent: Color32,
    pub accent_text: Color32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SystemTheme {
    pub dark: bool,
    pub accent: Color32,
    pub accent_on_taskbar: bool,
    pub contrast: Option<ContrastColors>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    pub dark: bool,
    pub background: Color32,
    pub header: Color32,
    pub surface: Color32,
    pub flyout: Color32,
    pub hover: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub border: Color32,
    pub accent: Color32,
    pub accent_text: Color32,
    pub marker: Color32,
}

fn luminance(color: Color32) -> f32 {
    let channel = |value: u8| {
        let value = value as f32 / 255.0;
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(color.r()) + 0.7152 * channel(color.g()) + 0.0722 * channel(color.b())
}

fn contrast(a: Color32, b: Color32) -> f32 {
    let a = luminance(a);
    let b = luminance(b);
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

fn best_text(background: Color32) -> Color32 {
    if contrast(Color32::BLACK, background) > contrast(Color32::WHITE, background) {
        Color32::BLACK
    } else {
        Color32::WHITE
    }
}

fn mix(a: Color32, b: Color32, amount: f32) -> Color32 {
    let channel = |x: u8, y: u8| (x as f32 * (1.0 - amount) + y as f32 * amount).round() as u8;
    Color32::from_rgb(
        channel(a.r(), b.r()),
        channel(a.g(), b.g()),
        channel(a.b(), b.b()),
    )
}

fn visible_accent(accent: Color32, background: Color32) -> Color32 {
    if contrast(accent, background) >= 3.0 {
        return accent;
    }
    let toward = best_text(background);
    for step in 1..=16 {
        let color = mix(accent, toward, step as f32 / 16.0);
        if contrast(color, background) >= 3.0 {
            return color;
        }
    }
    toward
}

impl Palette {
    pub fn from_system(theme: SystemTheme) -> Self {
        if let Some(colors) = theme.contrast {
            let surface = if contrast(colors.text, colors.surface) >= 4.5 {
                colors.surface
            } else {
                colors.background
            };
            return Self {
                dark: luminance(colors.background) < 0.5,
                background: colors.background,
                header: colors.background,
                surface,
                flyout: colors.background,
                hover: surface,
                text: colors.text,
                muted: colors.text,
                border: colors.text,
                accent: colors.accent,
                accent_text: colors.accent_text,
                marker: colors.accent,
            };
        }
        let (background, header, surface, hover, text, border) = if theme.dark {
            (
                Color32::from_gray(28),
                Color32::from_gray(36),
                Color32::from_gray(46),
                Color32::from_gray(62),
                Color32::from_gray(248),
                Color32::from_gray(85),
            )
        } else {
            (
                Color32::from_gray(245),
                Color32::from_gray(252),
                Color32::WHITE,
                Color32::from_gray(230),
                Color32::from_gray(25),
                Color32::from_gray(187),
            )
        };
        let tint = if theme.accent_on_taskbar { 0.20 } else { 0.0 };
        let background = mix(background, theme.accent, tint);
        let header = mix(header, theme.accent, tint);
        let surface = mix(surface, theme.accent, tint * 0.5);
        let flyout = header;
        Self {
            dark: theme.dark,
            background,
            header,
            surface,
            flyout,
            hover: mix(hover, theme.accent, tint * 0.5),
            text,
            muted: mix(text, header, 0.2),
            border,
            accent: theme.accent,
            accent_text: best_text(theme.accent),
            marker: visible_accent(theme.accent, flyout),
        }
    }
}

pub fn configure(ctx: &eframe::egui::Context, palette: Palette) {
    use eframe::egui::{self, Color32};
    let mode = if palette.dark {
        egui::Theme::Dark
    } else {
        egui::Theme::Light
    };
    let mut style = (*ctx.style_of(mode)).clone();
    style.visuals = if palette.dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    style.visuals.panel_fill = palette.background;
    style.visuals.window_fill = palette.background;
    style.visuals.extreme_bg_color = palette.surface;
    style.visuals.text_edit_bg_color = Some(palette.surface);
    style.visuals.widgets.noninteractive.fg_stroke.color = palette.text;
    style.visuals.widgets.inactive.fg_stroke.color = palette.text;
    style.visuals.widgets.hovered.fg_stroke.color = palette.text;
    style.visuals.widgets.active.fg_stroke.color = palette.text;
    style.visuals.widgets.open.fg_stroke.color = palette.text;
    style.visuals.widgets.noninteractive.bg_fill = palette.background;
    style.visuals.widgets.inactive.bg_fill = palette.surface;
    style.visuals.widgets.hovered.bg_fill = palette.hover;
    style.visuals.widgets.active.bg_fill = palette.hover;
    style.visuals.widgets.open.bg_fill = palette.hover;
    style.visuals.widgets.inactive.bg_stroke.color = palette.border;
    style.visuals.widgets.hovered.bg_stroke.color = palette.marker;
    style.visuals.selection.bg_fill = palette.accent;
    style.visuals.selection.stroke.color = palette.accent_text;
    style.visuals.hyperlink_color = palette.marker;
    style.visuals.warn_fg_color = if palette.dark {
        Color32::from_rgb(255, 195, 112)
    } else {
        Color32::from_rgb(126, 60, 0)
    };
    style.spacing.item_spacing = egui::vec2(10.0, 10.0);
    style.spacing.button_padding = egui::vec2(12.0, 8.0);
    style
        .text_styles
        .insert(egui::TextStyle::Body, egui::FontId::proportional(15.0));
    style
        .text_styles
        .insert(egui::TextStyle::Button, egui::FontId::proportional(14.0));
    ctx.set_style_of(mode, style);
    ctx.set_theme(mode);
}

pub struct ThemeTracker {
    current: SystemTheme,
    palette: Palette,
    checked: std::time::Instant,
    titlebar: Option<(isize, Palette)>,
}

impl ThemeTracker {
    pub fn new(ctx: &eframe::egui::Context) -> Self {
        let current = crate::platform::windows_theme();
        let palette = Palette::from_system(current);
        configure(ctx, palette);
        Self {
            current,
            palette,
            checked: std::time::Instant::now(),
            titlebar: None,
        }
    }

    pub fn update(&mut self, ctx: &eframe::egui::Context, window: Option<isize>) -> Palette {
        if self.checked.elapsed() >= std::time::Duration::from_secs(1) {
            self.checked = std::time::Instant::now();
            let current = crate::platform::windows_theme();
            if current != self.current {
                self.current = current;
                self.palette = Palette::from_system(current);
                configure(ctx, self.palette);
                ctx.request_repaint();
            }
        }
        if let Some(window) = window
            && self.titlebar != Some((window, self.palette))
        {
            crate::platform::set_titlebar_theme(window, self.palette);
            self.titlebar = Some((window, self.palette));
        }
        self.palette
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ratio(a: Color32, b: Color32) -> f32 {
        fn lum(c: Color32) -> f32 {
            let channel = |x: u8| {
                let x = x as f32 / 255.0;
                if x <= 0.04045 {
                    x / 12.92
                } else {
                    ((x + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * channel(c.r()) + 0.7152 * channel(c.g()) + 0.0722 * channel(c.b())
        }
        let (a, b) = (lum(a), lum(b));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    #[test]
    fn palettes_follow_the_taskbar_mode_with_readable_text() {
        for dark in [false, true] {
            let palette = Palette::from_system(SystemTheme {
                dark,
                accent: Color32::from_rgb(0, 120, 215),
                accent_on_taskbar: false,
                contrast: None,
            });
            assert_eq!(palette.dark, dark);
            assert!(ratio(palette.text, palette.background) >= 4.5);
            assert!(ratio(palette.muted, palette.header) >= 4.5);
            assert!(ratio(palette.accent_text, palette.accent) >= 4.5);
            assert!(ratio(palette.marker, palette.flyout) >= 3.0);
        }
    }

    #[test]
    fn bright_and_dark_accents_keep_selection_and_marker_readable() {
        for accent in [
            Color32::from_rgb(250, 245, 20),
            Color32::from_rgb(15, 25, 35),
        ] {
            for dark in [false, true] {
                let palette = Palette::from_system(SystemTheme {
                    dark,
                    accent,
                    accent_on_taskbar: true,
                    contrast: None,
                });
                assert!(ratio(palette.accent_text, palette.accent) >= 4.5);
                assert!(ratio(palette.text, palette.flyout) >= 4.5);
                assert!(ratio(palette.marker, palette.flyout) >= 3.0);
            }
        }
    }

    #[test]
    fn tinted_taskbar_colors_keep_manager_and_flyout_text_readable() {
        for r in [0, 128, 255] {
            for g in [0, 128, 255] {
                for b in [0, 128, 255] {
                    for dark in [false, true] {
                        let palette = Palette::from_system(SystemTheme {
                            dark,
                            accent: Color32::from_rgb(r, g, b),
                            accent_on_taskbar: true,
                            contrast: None,
                        });
                        assert!(ratio(palette.text, palette.surface) >= 4.5);
                        assert!(ratio(palette.text, palette.flyout) >= 4.5);
                        assert!(ratio(palette.muted, palette.header) >= 4.5);
                        assert!(ratio(palette.accent_text, palette.accent) >= 4.5);
                    }
                }
            }
        }
    }

    #[test]
    fn contrast_theme_uses_windows_system_colors() {
        let colors = ContrastColors {
            background: Color32::BLACK,
            text: Color32::WHITE,
            surface: Color32::BLACK,
            accent: Color32::YELLOW,
            accent_text: Color32::BLACK,
        };
        let palette = Palette::from_system(SystemTheme {
            dark: false,
            accent: Color32::RED,
            accent_on_taskbar: false,
            contrast: Some(colors),
        });
        assert_eq!(palette.background, colors.background);
        assert_eq!(palette.text, colors.text);
        assert_eq!(palette.accent, colors.accent);
        assert_eq!(palette.accent_text, colors.accent_text);
    }

    #[test]
    fn contrast_theme_preserves_readability_when_button_and_window_colors_differ() {
        let colors = ContrastColors {
            background: Color32::BLACK,
            text: Color32::WHITE,
            surface: Color32::WHITE,
            accent: Color32::YELLOW,
            accent_text: Color32::BLACK,
        };
        let palette = Palette::from_system(SystemTheme {
            dark: true,
            accent: Color32::RED,
            accent_on_taskbar: false,
            contrast: Some(colors),
        });
        assert!(ratio(palette.text, palette.surface) >= 4.5);
    }

    #[test]
    fn applying_a_new_windows_theme_updates_the_egui_style() {
        let ctx = eframe::egui::Context::default();
        let base = SystemTheme {
            dark: false,
            accent: Color32::from_rgb(0, 120, 215),
            accent_on_taskbar: false,
            contrast: None,
        };
        configure(&ctx, Palette::from_system(base));
        assert_eq!(ctx.theme(), eframe::egui::Theme::Light);
        assert!(!ctx.style_of(eframe::egui::Theme::Light).visuals.dark_mode);
        assert_eq!(
            ctx.style_of(eframe::egui::Theme::Light).visuals.panel_fill,
            Palette::from_system(base).background
        );
        let dark = SystemTheme { dark: true, ..base };
        configure(&ctx, Palette::from_system(dark));
        assert_eq!(ctx.theme(), eframe::egui::Theme::Dark);
        assert!(ctx.style_of(eframe::egui::Theme::Dark).visuals.dark_mode);
        assert_eq!(
            ctx.style_of(eframe::egui::Theme::Dark).visuals.panel_fill,
            Palette::from_system(dark).background
        );
    }
}
