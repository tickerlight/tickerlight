use tickerlight_backend_wgpu::{Gpu, render_to_rgba};
use tickerlight_core::{Color, Direction, Rect, Scene};

fn pixel(rgba: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * width + x) * 4) as usize;
    rgba[i..i + 4].try_into().unwrap()
}

#[test]
fn draws_background_and_rects_at_the_right_pixels() {
    let gpu = match Gpu::new_headless() {
        Ok(gpu) => gpu,
        Err(e) => {
            eprintln!("skipped: {e}");
            return;
        }
    };
    let mut scene = Scene::new();
    scene.background = Color::rgb(10, 20, 30);
    scene
        .rects
        .push(Rect::new(10.0, 5.0, 20.0, 10.0, Color::rgb(255, 0, 0)));
    // 300 px wide: the row is not a multiple of 256 bytes, so this also checks readback padding.
    let (w, h) = (300, 40);
    let rgba = render_to_rgba(&gpu, &scene, w, h);

    assert_eq!(rgba.len(), (w * h * 4) as usize);
    assert_eq!(pixel(&rgba, w, 0, 0), [10, 20, 30, 255]); // background
    assert_eq!(pixel(&rgba, w, 15, 8), [255, 0, 0, 255]); // inside the rect
    assert_eq!(pixel(&rgba, w, 9, 8), [10, 20, 30, 255]); // just left of it
    assert_eq!(pixel(&rgba, w, 15, 15), [10, 20, 30, 255]); // just below it
    assert_eq!(pixel(&rgba, w, 299, 39), [10, 20, 30, 255]); // last pixel of the last row
}

#[test]
fn an_outline_leaves_the_inside_alone() {
    let gpu = match Gpu::new_headless() {
        Ok(gpu) => gpu,
        Err(e) => {
            eprintln!("skipped: {e}");
            return;
        }
    };
    let mut scene = Scene::new();
    scene.background = Color::rgb(0, 0, 0);
    let white = Color::rgb(255, 255, 255);
    scene.stroke_rounded(10.0, 10.0, 20.0, 20.0, 0.0, 2.0, white);
    // A ring: a circle 20 px across, 2 px wide.
    scene.stroke_rounded(40.0, 10.0, 20.0, 20.0, 10.0, 2.0, white);
    let (w, h) = (64, 40);
    let rgba = render_to_rgba(&gpu, &scene, w, h);
    assert_eq!(pixel(&rgba, w, 10, 20), [255, 255, 255, 255]); // the left edge of the box
    assert_eq!(pixel(&rgba, w, 11, 20), [255, 255, 255, 255]);
    assert_eq!(pixel(&rgba, w, 13, 20), [0, 0, 0, 255]); // inside it
    assert_eq!(pixel(&rgba, w, 20, 20), [0, 0, 0, 255]);
    assert!(pixel(&rgba, w, 40, 20)[0] > 240); // the ring's left edge
    assert_eq!(pixel(&rgba, w, 50, 20), [0, 0, 0, 255]); // its middle
    assert_eq!(pixel(&rgba, w, 41, 11), [0, 0, 0, 255]); // outside its curve, in the box's corner
}

#[test]
fn a_gradient_to_nothing_fades_without_darkening() {
    let gpu = match Gpu::new_headless() {
        Ok(gpu) => gpu,
        Err(e) => {
            eprintln!("skipped: {e}");
            return;
        }
    };
    let mut scene = Scene::new();
    scene.background = Color::rgb(255, 255, 255);
    let red = Color::rgb(255, 0, 0);
    let nothing = Color { r: 0, g: 0, b: 0, a: 0 };
    scene.fill_gradient(0.0, 0.0, 100.0, 10.0, (red, nothing), Direction::Right);
    scene.fill_gradient(0.0, 10.0, 10.0, 100.0, (red, nothing), Direction::Down);
    let (w, h) = (100, 110);
    let rgba = render_to_rgba(&gpu, &scene, w, h);
    let [r, g, ..] = pixel(&rgba, w, 0, 5);
    assert!(r > 250 && g < 8, "red at the start: {r} {g}");
    // Halfway: half red over white, not red mixed with black.
    let [r, g, b, _] = pixel(&rgba, w, 50, 5);
    assert!(r > 245, "not darkened: {r}");
    assert!((115..140).contains(&g) && (115..140).contains(&b), "half way: {g} {b}");
    assert!(pixel(&rgba, w, 99, 5)[1] > 245, "nothing at the end");
    let down = |y| pixel(&rgba, w, 5, y)[1];
    assert!(down(12) < down(60) && down(60) < down(108), "fades downwards");
}

#[test]
fn a_feathered_rect_fades_in_from_its_edge() {
    let gpu = match Gpu::new_headless() {
        Ok(gpu) => gpu,
        Err(e) => {
            eprintln!("skipped: {e}");
            return;
        }
    };
    let mut scene = Scene::new();
    scene.background = Color::rgb(0, 0, 0);
    scene.fill_soft(0.0, 0.0, 60.0, 60.0, 0.0, 10.0, Color::rgb(255, 255, 255));
    let (w, h) = (60, 60);
    let rgba = render_to_rgba(&gpu, &scene, w, h);
    let at = |x| pixel(&rgba, w, x, 30)[0];
    assert!(at(0) < 10, "dark at the edge: {}", at(0));
    assert!((60..200).contains(&at(5)), "half way in: {}", at(5));
    assert_eq!(at(15), 255, "full past the feather");
    assert_eq!(at(30), 255);
}

#[test]
fn draws_text_where_it_was_asked_to() {
    let gpu = match Gpu::new_headless() {
        Ok(gpu) => gpu,
        Err(e) => {
            eprintln!("skipped: {e}");
            return;
        }
    };
    let mut scene = Scene::new();
    let bg = Color::rgb(0, 0, 0);
    scene.background = bg;
    // Centered on x = 100, the middle of the capitals on y = 30.
    scene.text(
        100.0,
        30.0,
        20.0,
        Color::rgb(255, 255, 255),
        tickerlight_core::Align::Center,
        "8888",
    );
    let (w, h) = (200, 60);
    let rgba = render_to_rgba(&gpu, &scene, w, h);

    let mut ink = Vec::new();
    for y in 0..h {
        for x in 0..w {
            if pixel(&rgba, w, x, y)[0] > 100 {
                ink.push((x, y));
            }
        }
    }
    assert!(ink.len() > 40, "text has ink: {} bright pixels", ink.len());
    let (min_x, max_x) = (
        ink.iter().map(|p| p.0).min().unwrap(),
        ink.iter().map(|p| p.0).max().unwrap(),
    );
    let (min_y, max_y) = (
        ink.iter().map(|p| p.1).min().unwrap(),
        ink.iter().map(|p| p.1).max().unwrap(),
    );
    // Horizontally centered on 100, vertically centered on 30 (a 20 px digit is ~14 px tall).
    assert!(
        ((min_x + max_x) as i32 / 2 - 100).abs() <= 3,
        "x range {min_x}..{max_x}"
    );
    assert!(((min_y + max_y) as i32 / 2 - 30).abs() <= 2, "y range {min_y}..{max_y}");
    assert!(
        max_y - min_y >= 10 && max_y - min_y <= 18,
        "digit height {}",
        max_y - min_y
    );
    // Nothing outside the text: the corners stay background.
    assert_eq!(pixel(&rgba, w, 0, 0), [0, 0, 0, 255]);
    assert_eq!(pixel(&rgba, w, 199, 59), [0, 0, 0, 255]);
    // Text is antialiased: there are partly covered pixels, not only 0 and 255.
    assert!(ink.iter().any(|&(x, y)| pixel(&rgba, w, x, y)[0] < 250));
}

fn gpu_or_skip() -> Option<Gpu> {
    match Gpu::new_headless() {
        Ok(gpu) => Some(gpu),
        Err(e) => {
            eprintln!("skipped: {e}");
            None
        }
    }
}

#[test]
fn draws_lines_with_the_given_width_and_smooth_diagonals() {
    let Some(gpu) = gpu_or_skip() else { return };
    let mut scene = Scene::new();
    scene.background = Color::rgb(0, 0, 0);
    let white = Color::rgb(255, 255, 255);
    scene.line((10.0, 20.0), (50.0, 20.0), 4.0, white); // horizontal, covers y in 18..22
    scene.line((10.0, 40.0), (50.0, 58.0), 2.0, white); // diagonal
    let (w, h) = (64, 64);
    let rgba = render_to_rgba(&gpu, &scene, w, h);

    // Horizontal: solid inside, background well outside, nothing to the left of the start.
    for y in 18..22 {
        assert_eq!(pixel(&rgba, w, 30, y)[0], 255, "y {y}");
    }
    assert_eq!(pixel(&rgba, w, 30, 15)[0], 0);
    assert_eq!(pixel(&rgba, w, 30, 25)[0], 0);
    assert_eq!(pixel(&rgba, w, 2, 20)[0], 0);
    assert!(
        pixel(&rgba, w, 30, 17)[0] < 30,
        "one pixel outside is at most the faint edge"
    );

    // Diagonal: anti-aliased, so it has partly covered pixels, and it passes through the middle.
    let diagonal: Vec<u8> = (0..w)
        .flat_map(|x| (34..64).map(move |y| (x, y)))
        .map(|(x, y)| pixel(&rgba, w, x, y)[0])
        .collect();
    assert!(diagonal.iter().any(|&v| v > 40 && v < 215), "edge pixels are blended");
    assert!(
        pixel(&rgba, w, 30, 49)[0] > 200,
        "the middle of the segment is at (30, 49)"
    );
}

#[test]
fn later_layers_cover_earlier_ones_including_text() {
    let Some(gpu) = gpu_or_skip() else { return };
    let white = Color::rgb(255, 255, 255);
    let red = Color::rgb(255, 0, 0);
    let (w, h) = (120, 40);
    let draw = |separate_layers: bool| {
        let mut scene = Scene::new();
        scene.background = Color::rgb(0, 0, 0);
        scene.text(60.0, 20.0, 20.0, white, tickerlight_core::Align::Center, "8888");
        if separate_layers {
            scene.next_layer();
        }
        scene.fill(0.0, 0.0, 120.0, 40.0, red); // covers everything
        render_to_rgba(&gpu, &scene, w, h)
    };
    // In separate layers the red rect is on top: no white left anywhere.
    let covered = draw(true);
    assert!((0..w).all(|x| (0..h).all(|y| pixel(&covered, w, x, y) == [255, 0, 0, 255])));
    // In one layer text goes above rects: white text shows over the red.
    let together = draw(false);
    assert!((0..w).any(|x| (0..h).any(|y| pixel(&together, w, x, y)[1] > 150)));
}

#[test]
fn rounded_rects_cut_their_corners_and_keep_straight_edges_crisp() {
    let Some(gpu) = gpu_or_skip() else { return };
    let mut scene = Scene::new();
    scene.background = Color::rgb(0, 0, 0);
    scene.fill_rounded(10.0, 10.0, 40.0, 30.0, 10.0, Color::rgb(255, 255, 255));
    scene.fill(60.0, 10.0, 10.0, 10.0, Color::rgb(255, 255, 255));
    let (w, h) = (80, 50);
    let rgba = render_to_rgba(&gpu, &scene, w, h);

    assert_eq!(pixel(&rgba, w, 30, 25)[0], 255, "the middle is solid");
    assert_eq!(
        pixel(&rgba, w, 30, 10)[0],
        255,
        "a straight edge pixel is fully covered"
    );
    assert_eq!(pixel(&rgba, w, 10, 25)[0], 255);
    assert_eq!(pixel(&rgba, w, 10, 10)[0], 0, "the very corner is cut away");
    let edge = pixel(&rgba, w, 12, 13)[0]; // its center is 9.9 px from the arc center
    assert!(edge > 0 && edge < 255, "the arc is anti-aliased: {edge}");
    assert_eq!(pixel(&rgba, w, 60, 10)[0], 255, "a sharp rect keeps its corner");
    assert_eq!(pixel(&rgba, w, 69, 19)[0], 255);
}
