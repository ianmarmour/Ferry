//! The app icon: a ferry on a speech bubble, from `assets/ferry.svg`, rendered at the
//! sizes in `assets/icons`. The tray adds a red dot while there are unread messages.

/// The rendered sizes, as PNG files.
pub const PNGS: [(u32, &[u8]); 9] = [
    (16, include_bytes!("../../assets/icons/ferry-16.png")),
    (22, include_bytes!("../../assets/icons/ferry-22.png")),
    (24, include_bytes!("../../assets/icons/ferry-24.png")),
    (32, include_bytes!("../../assets/icons/ferry-32.png")),
    (48, include_bytes!("../../assets/icons/ferry-48.png")),
    (64, include_bytes!("../../assets/icons/ferry-64.png")),
    (128, include_bytes!("../../assets/icons/ferry-128.png")),
    (256, include_bytes!("../../assets/icons/ferry-256.png")),
    (512, include_bytes!("../../assets/icons/ferry-512.png")),
];

/// The scalable icon.
pub const SVG: &[u8] = include_bytes!("../../assets/ferry.svg");

/// Decodes a PNG into RGBA pixels.
fn decode(png: &[u8]) -> Option<(Vec<u8>, u32, u32)> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(png));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().ok()?;
    let mut pixels = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut pixels).ok()?;
    pixels.truncate(info.buffer_size());

    let rgba = match info.color_type {
        png::ColorType::Rgba => pixels,
        png::ColorType::Rgb => pixels
            .chunks_exact(3)
            .flat_map(|pixel| [pixel[0], pixel[1], pixel[2], 255])
            .collect(),
        _ => return None,
    };
    Some((rgba, info.width, info.height))
}

/// Coverage of a pixel by a circle, by 4×4 supersampling.
fn circle_coverage(x: u32, y: u32, size: f32, cx: f32, cy: f32, r: f32) -> f32 {
    let mut hits = 0;
    for sy in 0..4 {
        for sx in 0..4 {
            let px = (x as f32 + (sx as f32 + 0.5) / 4.0) / size;
            let py = (y as f32 + (sy as f32 + 0.5) / 4.0) / size;
            if (px - cx).powi(2) + (py - cy).powi(2) <= r * r {
                hits += 1;
            }
        }
    }
    hits as f32 / 16.0
}

/// Blends `color` with `coverage` over a straight RGBA pixel.
fn over(pixel: &mut [u8], color: [u8; 3], coverage: f32) {
    let below = f32::from(pixel[3]) / 255.0;
    let alpha = coverage + below * (1.0 - coverage);
    if alpha > 0.0 {
        for i in 0..3 {
            let mixed = (f32::from(color[i]) * coverage
                + f32::from(pixel[i]) * below * (1.0 - coverage))
                / alpha;
            pixel[i] = mixed.round() as u8;
        }
    }
    pixel[3] = (alpha * 255.0).round() as u8;
}

/// The icon as RGBA pixels at the rendered size closest to `size` (at least as large),
/// with the unread badge if `badge`.
pub fn rgba(size: u32, badge: bool) -> (Vec<u8>, u32) {
    let (_, png) = PNGS
        .iter()
        .find(|(rendered, _)| *rendered >= size)
        .unwrap_or(&PNGS[PNGS.len() - 1]);
    let (mut pixels, width, _) = decode(png).expect("the bundled icons are valid PNGs");

    if badge {
        let n = width as f32;
        for (i, pixel) in pixels.chunks_exact_mut(4).enumerate() {
            let (x, y) = (i as u32 % width, i as u32 / width);
            over(
                pixel,
                [255, 255, 255],
                circle_coverage(x, y, n, 0.80, 0.20, 0.20),
            );
            over(
                pixel,
                [255, 59, 48],
                circle_coverage(x, y, n, 0.80, 0.20, 0.155),
            );
        }
    }
    (pixels, width)
}

/// The icon for the window and the taskbar.
pub fn window_icon() -> egui::IconData {
    let (rgba, size) = rgba(256, false);
    egui::IconData {
        rgba,
        width: size,
        height: size,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_every_size_and_adds_the_badge() {
        for (size, png) in PNGS {
            let (pixels, width, height) = decode(png).unwrap();
            assert_eq!((size, size), (width, height));
            assert_eq!((size * size * 4) as usize, pixels.len());
        }

        let pixel = |image: &[u8], size: u32, x: u32, y: u32| {
            let i = ((y * size + x) * 4) as usize;
            [image[i], image[i + 1], image[i + 2], image[i + 3]]
        };
        let (plain, size) = rgba(32, false);
        assert_eq!(32, size);
        assert_eq!(0, pixel(&plain, size, 0, 0)[3], "corners are transparent");

        let (badged, _) = rgba(32, true);
        let dot = pixel(&badged, size, 25, 6);
        assert!(dot[0] > 200 && dot[1] < 100, "the badge is red: {:?}", dot);
    }
}
