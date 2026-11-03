//! Interface-scale-aware spacing tokens.
//!
//! Values name their size at OpenLogi's 16 px baseline and resolve to rems, so
//! the existing interface-scale setting changes text and spacing together.

use gpui::{Rems, rems};

const BASE_REM_SIZE_IN_PX: f32 = 16.;

/// A finite spacing scale for layout that should follow the interface size.
///
/// The variant name is the token's size in pixels at the standard 16 px rem.
/// Fixed geometry such as one-pixel borders and device artwork should continue
/// to use pixels directly.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DynamicSpacing {
    /// 12 px at standard scale.
    Base12,
    /// 16 px at standard scale.
    Base16,
    /// 20 px at standard scale.
    Base20,
}

impl DynamicSpacing {
    /// Resolve this token in rems so the window's interface scale applies it.
    #[must_use]
    pub const fn rems(self) -> Rems {
        rems(self.base_pixels() / BASE_REM_SIZE_IN_PX)
    }

    const fn base_pixels(self) -> f32 {
        match self {
            Self::Base12 => 12.,
            Self::Base16 => 16.,
            Self::Base20 => 20.,
        }
    }
}

#[cfg(test)]
mod tests {
    use gpui::px;

    use super::*;

    #[test]
    fn tokens_preserve_their_baseline_pixel_values() {
        let cases = [
            (DynamicSpacing::Base12, 12.),
            (DynamicSpacing::Base16, 16.),
            (DynamicSpacing::Base20, 20.),
        ];

        for (spacing, expected) in cases {
            assert_eq!(
                spacing.rems().to_pixels(px(BASE_REM_SIZE_IN_PX)),
                px(expected)
            );
        }
    }
}
