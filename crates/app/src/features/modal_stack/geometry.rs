use iced::Size;

const GROWTH_START: Size = Size::new(1600.0, 1000.0);
const GROWTH_END: Size = Size::new(2560.0, 1440.0);

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ResponsiveSize {
    preferred: Size,
    maximum: Size,
}

impl ResponsiveSize {
    pub const fn new(preferred: Size, maximum: Size) -> Self {
        Self { preferred, maximum }
    }

    pub fn resolve(self, viewport: Size, viewport_ratio: f32) -> Size {
        Size::new(
            resolve_axis(
                self.preferred.width,
                self.maximum.width,
                viewport.width,
                GROWTH_START.width,
                GROWTH_END.width,
                viewport_ratio,
            ),
            resolve_axis(
                self.preferred.height,
                self.maximum.height,
                viewport.height,
                GROWTH_START.height,
                GROWTH_END.height,
                viewport_ratio,
            ),
        )
    }
}

/// Near-viewport size for a modal hosting an expanded embedded file preview.
/// The panel is centered, so clearance applies to both edges: the top gap
/// must clear the window chrome band (traffic lights) on inset-titlebar
/// macOS.
pub fn expanded_size(preferred: Size, viewport: Size, pad: f32, chrome_clearance: f32) -> Size {
    Size::new(
        expanded_edge(preferred.width, viewport.width, pad, 0.0),
        expanded_edge(preferred.height, viewport.height, pad, chrome_clearance),
    )
}

fn expanded_edge(preferred: f32, viewport_edge: f32, pad: f32, chrome_clearance: f32) -> f32 {
    if viewport_edge > 0.0 {
        (viewport_edge - (pad + chrome_clearance) * 2.0).max(1.0)
    } else {
        preferred
    }
}

pub fn responsive_width(
    preferred: f32,
    maximum: f32,
    viewport_width: f32,
    viewport_ratio: f32,
) -> f32 {
    resolve_axis(
        preferred,
        maximum,
        viewport_width,
        GROWTH_START.width,
        GROWTH_END.width,
        viewport_ratio,
    )
}

fn resolve_axis(
    preferred: f32,
    maximum: f32,
    viewport: f32,
    growth_start: f32,
    growth_end: f32,
    viewport_ratio: f32,
) -> f32 {
    if !viewport.is_finite() || viewport <= 0.0 {
        return preferred;
    }

    let progress = ((viewport - growth_start) / (growth_end - growth_start)).clamp(0.0, 1.0);
    let grown = preferred + (maximum - preferred) * progress;

    grown.min(viewport * viewport_ratio).max(1.0)
}

#[cfg(test)]
mod tests {
    use iced::Size;

    use super::{ResponsiveSize, responsive_width};

    #[test]
    fn laptop_viewports_keep_the_preferred_size() {
        let size = ResponsiveSize::new(Size::new(672.0, 480.0), Size::new(960.0, 720.0))
            .resolve(Size::new(1512.0, 982.0), 0.9);

        assert_eq!(size, Size::new(672.0, 480.0));
    }

    #[test]
    fn large_viewports_reach_the_modal_maximum() {
        let size = ResponsiveSize::new(Size::new(672.0, 480.0), Size::new(960.0, 720.0))
            .resolve(Size::new(2560.0, 1440.0), 0.9);

        assert_eq!(size, Size::new(960.0, 720.0));
    }

    #[test]
    fn intermediate_viewports_grow_smoothly_per_axis() {
        let size = ResponsiveSize::new(Size::new(1008.0, 704.0), Size::new(1600.0, 1100.0))
            .resolve(Size::new(2080.0, 1220.0), 0.9);

        assert_eq!(size, Size::new(1304.0, 902.0));
    }

    #[test]
    fn small_viewports_apply_the_available_space_ceiling() {
        let size = ResponsiveSize::new(Size::new(1168.0, 720.0), Size::new(1600.0, 1000.0))
            .resolve(Size::new(900.0, 700.0), 0.9);

        assert_eq!(size, Size::new(810.0, 630.0));
    }

    #[test]
    fn compact_widths_do_not_grow_when_preferred_equals_maximum() {
        assert_eq!(responsive_width(420.0, 420.0, 3840.0, 0.9), 420.0);
        assert_eq!(responsive_width(420.0, 420.0, 400.0, 0.9), 360.0);
    }
}
