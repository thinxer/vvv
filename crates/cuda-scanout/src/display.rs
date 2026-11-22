/// Geometry of a viewer's remote desktop. Both extents are in desktop orientation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DisplayLayout {
    pub backing: (u16, u16),
    pub logical: (u16, u16),
}

impl DisplayLayout {
    pub fn from_rdp(backing: (u16, u16), desktop_scale: Option<u32>) -> Self {
        let scale = desktop_scale
            .filter(|s| (100..=500).contains(s))
            .unwrap_or(100);
        let logical = |n| ((u32::from(n) * 100 + scale / 2) / scale) as u16;
        Self {
            backing,
            logical: (logical(backing.0), logical(backing.1)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_scale_changes_only_the_logical_extent() {
        let pixels = (2106, 1418);
        let layout = DisplayLayout::from_rdp(pixels, Some(200));
        assert_eq!(layout.backing, pixels);
        assert_eq!(layout.logical, (1053, 709));
        assert_eq!(DisplayLayout::from_rdp(pixels, None).logical, pixels);
        assert_eq!(DisplayLayout::from_rdp(pixels, Some(0)).logical, pixels);
    }
}
