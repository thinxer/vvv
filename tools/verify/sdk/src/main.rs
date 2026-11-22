use apple_screen_sharing::{ExternalScreen, capture::{Frame, FrameBuffer}, encode::TileLayout};
use std::time::Duration;

struct Bitmap {
    width: u32,
    height: u32,
}

impl FrameBuffer for Bitmap {
    fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }
}

struct Desktop;

impl ExternalScreen<Bitmap> for Desktop {
    fn size(&self) -> (u16, u16) {
        (1280, 720)
    }

    fn next_frame(&self, _: u64, _: Duration) -> Option<Frame<Bitmap>> {
        None
    }
}

fn main() {
    let (width, height) = Desktop.size();
    let layout = TileLayout::new(width.into(), height.into());
    assert_eq!(layout.tile_rows(0), 192);
}
