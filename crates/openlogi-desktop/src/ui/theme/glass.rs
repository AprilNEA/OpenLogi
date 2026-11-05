//! Apply opacity to the window canvas and popup containers only.

use gpui_kit::component::Theme;
use openlogi_core::config::AppSettings;

use super::AppliedMaterial;

pub(super) fn apply(theme: &mut Theme, requested: bool, opacity: u8) -> AppliedMaterial {
    if requested && crate::platform::accessibility::allows_glass() {
        tint(theme, opacity)
    } else {
        AppliedMaterial::Opaque
    }
}

fn tint(theme: &mut Theme, opacity: u8) -> AppliedMaterial {
    let opacity = AppSettings::clamp_acrylic_opacity(opacity);
    let alpha = f32::from(opacity) / 100.;
    theme.tokens.background = theme.background.alpha(alpha).into();
    theme.popover = theme.popover.alpha(alpha);
    theme.tokens.popover = theme.popover.into();
    AppliedMaterial::Glass { alpha }
}

#[cfg(test)]
mod tests {
    use gpui_kit::component::{ThemeColor, ThemeRegistry};

    use super::*;

    #[gpui::test]
    fn every_theme_keeps_its_original_design(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            super::super::register_builtin_themes(cx);
            for config in ThemeRegistry::global(cx).sorted_themes() {
                let source = if config.mode.is_dark() {
                    ThemeColor::dark()
                } else {
                    ThemeColor::light()
                };
                let mut theme = Theme::from(source.as_ref());
                theme.apply_config(config);
                let original = theme.clone();
                for opacity in [0, 40, 80, 100] {
                    tint(&mut theme, opacity);
                    let mut colors = serde_json::to_value(theme.colors).unwrap();
                    let mut expected = serde_json::to_value(original.colors).unwrap();
                    colors.as_object_mut().unwrap().remove("popover");
                    expected.as_object_mut().unwrap().remove("popover");
                    assert_eq!(
                        colors, expected,
                        "{}: original component colors",
                        config.name
                    );
                    let mut tokens = serde_json::to_value(theme.tokens).unwrap();
                    let mut expected = serde_json::to_value(original.tokens).unwrap();
                    for key in ["background", "popover"] {
                        tokens.as_object_mut().unwrap().remove(key);
                        expected.as_object_mut().unwrap().remove(key);
                    }
                    assert_eq!(
                        tokens, expected,
                        "{}: original component tokens",
                        config.name
                    );
                    assert_eq!(theme.radius, original.radius);
                    assert_eq!(theme.tokens.background.color.a, f32::from(opacity) / 100.);
                    assert_eq!(theme.popover.a, f32::from(opacity) / 100.);
                }
            }
        });
    }

    #[test]
    fn renderable_backgrounds_receive_the_requested_opacity() {
        for source in [ThemeColor::light(), ThemeColor::dark()] {
            let mut theme = Theme::from(source.as_ref());
            let original = theme.clone();
            for opacity in [0, 40, 80, 100] {
                tint(&mut theme, opacity);
                // Root and Kit popups convert tokens to Background, not Hsla.
                let canvas: gpui::Background = theme.tokens.background.into();
                let popup: gpui::Background = theme.tokens.popover.into();
                let alpha = f32::from(opacity) / 100.;
                assert_eq!(canvas.as_solid(), Some(original.background.alpha(alpha)));
                assert_eq!(popup.as_solid(), Some(original.popover.alpha(alpha)));
                assert_eq!(canvas.as_solid(), Some(theme.tokens.background.color));
                assert_eq!(popup.as_solid(), Some(theme.tokens.popover.color));
            }
        }
    }

    #[test]
    fn opacity_changes_do_not_accumulate_or_change_surface_colors() {
        let mut theme = Theme::from(ThemeColor::dark().as_ref());
        let original = theme.clone();
        for opacity in [40, 80, 20, 100, 0, 80] {
            tint(&mut theme, opacity);
            assert_eq!(
                theme.tokens.background.color,
                original.background.alpha(f32::from(opacity) / 100.)
            );
            assert_eq!(
                theme.popover,
                original.popover.alpha(f32::from(opacity) / 100.)
            );
        }
    }
}
