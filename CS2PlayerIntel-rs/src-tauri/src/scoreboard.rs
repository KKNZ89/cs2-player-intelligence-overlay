//! Teammate colours read from a picture of CS2's scoreboard.
//!
//! CS2 draws each teammate's colour inside their scoreboard avatar (panorama/scripts/scoreboard.vts tints
//! the avatar's `player-color` element). Avatars are 24 px squares at 1080p and scale with the screen
//! height. Nothing here relies on screen positions: each player's Steam avatar is searched for around that
//! size, and the colour is read inside the box where it was found. Pixels that are part of the avatar
//! picture itself are ignored, so a yellow avatar is not mistaken for the yellow teammate.

use crate::capture::{classify, Frame, TeammateColour};

/// A player's Steam avatar, decoded to RGB.
#[derive(Clone)]
pub struct Avatar {
    pub steam_id: String,
    pub width: usize,
    pub height: usize,
    pub rgb: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    pub steam_id: String,
    pub colour: Option<TeammateColour>,
    /// How well the avatar matched (correlation, 1 = identical).
    pub score: f32,
}

/// Blocks per side of the coarse avatar fingerprint compared during the search.
const GRID: usize = 4;
const DIMS: usize = GRID * GRID * 3;
const MIN_SCORE: f32 = 0.86;
/// Two avatars this similar (default avatars, for example) can't be told apart, so neither is read.
const SAME_PICTURE: f32 = 0.97;

type Fingerprint = [f32; DIMS];

struct Image {
    width: usize,
    height: usize,
    rgb: Vec<u8>,
}

/// Sums over rectangles in constant time.
struct Integral {
    width: usize,
    sums: Vec<[u32; 3]>,
}

impl Integral {
    fn new(image: &Image) -> Self {
        let width = image.width + 1;
        let mut sums = vec![[0u32; 3]; width * (image.height + 1)];
        for y in 0..image.height {
            let mut row = [0u32; 3];
            for x in 0..image.width {
                let i = (y * image.width + x) * 3;
                for c in 0..3 {
                    row[c] += image.rgb[i + c] as u32;
                    sums[(y + 1) * width + x + 1][c] = sums[y * width + x + 1][c] + row[c];
                }
            }
        }
        Self { width, sums }
    }

    fn mean(&self, x0: usize, y0: usize, x1: usize, y1: usize) -> [f32; 3] {
        let at = |x: usize, y: usize| self.sums[y * self.width + x];
        let (a, b, c, d) = (at(x0, y0), at(x1, y0), at(x0, y1), at(x1, y1));
        let area = ((x1 - x0) * (y1 - y0)).max(1) as f32;
        [0, 1, 2].map(|i| (d[i] + a[i]).wrapping_sub(b[i]).wrapping_sub(c[i]) as f32 / area)
    }
}

/// Block means over the middle of a square (its outer tenth is left out: CS2 may draw the colour there),
/// or None for a flat area, which can't be an avatar.
fn fingerprint(integral: &Integral, x: usize, y: usize, size: usize) -> Option<Fingerprint> {
    let inset = size / 10;
    let (x, y, size) = (x + inset, y + inset, size - 2 * inset);
    let mut v = [0f32; DIMS];
    for by in 0..GRID {
        for bx in 0..GRID {
            let mean = integral.mean(x + bx * size / GRID, y + by * size / GRID, x + (bx + 1) * size / GRID, y + (by + 1) * size / GRID);
            v[(by * GRID + bx) * 3..][..3].copy_from_slice(&mean);
        }
    }
    let average = v.iter().sum::<f32>() / DIMS as f32;
    let spread = (v.iter().map(|value| (value - average).powi(2)).sum::<f32>() / DIMS as f32).sqrt();
    // Below an average deviation of about 4 levels the square is flat.
    (spread >= 4.0).then_some(v)
}

/// Zero-mean, unit-length copy of the chosen blocks' values.
fn normalised(v: &Fingerprint, keep: &[bool; GRID * GRID]) -> Fingerprint {
    let mut out = [0f32; DIMS];
    let kept = keep.iter().filter(|k| **k).count().max(1) as f32 * 3.0;
    let mean = (0..DIMS).filter(|i| keep[i / 3]).map(|i| v[i]).sum::<f32>() / kept;
    for i in (0..DIMS).filter(|i| keep[i / 3]) {
        out[i] = v[i] - mean;
    }
    let norm = out.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-6);
    out.iter_mut().for_each(|x| *x /= norm);
    out
}

/// Correlation between an avatar and a square of the frame (1 = identical up to brightness and contrast).
/// The four blocks that disagree most are left out, so a colour marker drawn over part of the avatar, in
/// whatever shape, does not spoil the match.
fn correlation(a: &Fingerprint, b: &Fingerprint) -> f32 {
    let all = [true; GRID * GRID];
    let (na, nb) = (normalised(a, &all), normalised(b, &all));
    let mut residual: Vec<(f32, usize)> =
        (0..GRID * GRID).map(|block| ((0..3).map(|c| (na[block * 3 + c] - nb[block * 3 + c]).powi(2)).sum(), block)).collect();
    residual.sort_by(|x, y| y.0.total_cmp(&x.0));
    let mut keep = all;
    for (_, block) in residual.iter().take(4) {
        keep[*block] = false;
    }
    let (ka, kb) = (normalised(a, &keep), normalised(b, &keep));
    ka.iter().zip(&kb).map(|(x, y)| x * y).sum()
}

fn avatar_image(avatar: &Avatar) -> Image {
    Image { width: avatar.width, height: avatar.height, rgb: avatar.rgb.clone() }
}

/// The frame as RGB, halved (or more) above about 1600 px high so 4K searches cost the same as 1080p.
fn working_image(frame: &Frame) -> (Image, usize) {
    let factor = ((frame.height as f32 / 1080.0).round() as usize).max(1);
    let (width, height) = (frame.width as usize / factor, frame.height as usize / factor);
    let mut rgb = vec![0u8; width * height * 3];
    for y in 0..height {
        for x in 0..width {
            let mut sum = [0u32; 3];
            for dy in 0..factor {
                for dx in 0..factor {
                    let (r, g, b) = frame.rgb((x * factor + dx) as u32, (y * factor + dy) as u32);
                    sum[0] += r as u32;
                    sum[1] += g as u32;
                    sum[2] += b as u32;
                }
            }
            let n = (factor * factor) as u32;
            rgb[(y * width + x) * 3..][..3].copy_from_slice(&sum.map(|s| (s / n) as u8));
        }
    }
    (Image { width, height, rgb }, factor)
}

#[derive(Clone, Copy, Debug)]
struct Hit {
    score: f32,
    x: usize,
    y: usize,
    size: usize,
}

/// Candidate spots kept per avatar from the fast pass.
const CANDIDATES: usize = 10;
const ALL_BLOCKS: [bool; GRID * GRID] = [true; GRID * GRID];

/// Keeps the best `CANDIDATES` spots, at least half an avatar apart.
fn keep_candidate(list: &mut Vec<Hit>, hit: Hit) {
    if let Some(near) = list.iter_mut().find(|h| h.x.abs_diff(hit.x) < hit.size / 2 && h.y.abs_diff(hit.y) < hit.size / 2) {
        if hit.score > near.score {
            *near = hit;
        }
        return;
    }
    if list.len() < CANDIDATES {
        list.push(hit);
    } else if let Some(weakest) = list.iter_mut().min_by(|a, b| a.score.total_cmp(&b.score)) {
        if hit.score > weakest.score {
            *weakest = hit;
        }
    }
}

/// The fast pass: plain correlation of every `step`-th square of one size against each avatar.
fn candidates(integral: &Integral, templates: &[Option<Fingerprint>], size: usize, area: (usize, usize, usize, usize), step: usize) -> Vec<Vec<Hit>> {
    let (x0, y0, x1, y1) = area;
    let mut lists: Vec<Vec<Hit>> = vec![Vec::new(); templates.len()];
    let mut y = y0;
    while y + size <= y1 {
        let mut x = x0;
        while x + size <= x1 {
            if let Some(print) = fingerprint(integral, x, y, size) {
                let print = normalised(&print, &ALL_BLOCKS);
                for (i, template) in templates.iter().enumerate() {
                    let Some(template) = template else { continue };
                    let score: f32 = template.iter().zip(&print).map(|(a, b)| a * b).sum();
                    let list = &mut lists[i];
                    if list.len() < CANDIDATES || list.iter().any(|h| score > h.score) {
                        keep_candidate(list, Hit { score, x, y, size });
                    }
                }
            }
            x += step;
        }
        y += step;
    }
    lists
}

/// The best robust match near a candidate spot: a pixel or two either way, and one size up or down.
fn refine(integral: &Integral, image: &Image, template: &Fingerprint, around: Hit) -> Option<Hit> {
    let mut best: Option<Hit> = None;
    for size in around.size.saturating_sub(1).max(GRID * 2)..=around.size + 1 {
        for y in around.y.saturating_sub(2)..=around.y + 2 {
            for x in around.x.saturating_sub(2)..=around.x + 2 {
                if x + size > image.width || y + size > image.height {
                    continue;
                }
                let Some(print) = fingerprint(integral, x, y, size) else { continue };
                let score = correlation(template, &print);
                if best.is_none_or(|b| score > b.score) {
                    best = Some(Hit { score, x, y, size });
                }
            }
        }
    }
    best
}

/// Whether the avatar picture has this colour at a box position, allowing for the box being a pixel off.
fn in_picture(avatar: &Avatar, (x, y): (usize, usize), size: usize, colour: TeammateColour) -> bool {
    let (sx, sy) = (avatar.width / size, avatar.height / size);
    let (cx, cy) = (x * avatar.width / size, y * avatar.height / size);
    (cy.saturating_sub(sy)..=(cy + sy).min(avatar.height - 1)).any(|ay| {
        (cx.saturating_sub(sx)..=(cx + sx).min(avatar.width - 1)).any(|ax| {
            let i = (ay * avatar.width + ax) * 3;
            classify(avatar.rgb[i], avatar.rgb[i + 1], avatar.rgb[i + 2]) == Some(colour)
        })
    })
}

/// The teammate colour drawn in and just around an avatar's box (frame coordinates), ignoring pixels that
/// have the same colour in the avatar picture itself.
fn colour_in_box(frame: &Frame, avatar: &Avatar, bx: usize, by: usize, size: usize) -> Option<TeammateColour> {
    let margin = (size / 5).max(1);
    // A colour covering more than 3 % of the avatar picture can't be told apart from the marker, so it is
    // not counted for this player (worst case: no colour, never a wrong one).
    let mut in_avatar: Vec<(TeammateColour, usize)> = Vec::new();
    for px in avatar.rgb.chunks_exact(3) {
        if let Some(colour) = classify(px[0], px[1], px[2]) {
            match in_avatar.iter_mut().find(|(c, _)| *c == colour) {
                Some((_, n)) => *n += 1,
                None => in_avatar.push((colour, 1)),
            }
        }
    }
    let pixels = (avatar.width * avatar.height).max(1);
    let ambiguous: Vec<TeammateColour> = in_avatar.iter().filter(|(_, n)| n * 100 > pixels * 3).map(|(c, _)| *c).collect();
    let mut counts: Vec<(TeammateColour, usize)> = Vec::new();
    for y in by.saturating_sub(margin)..by + size + margin {
        for x in bx.saturating_sub(margin)..bx + size + margin {
            let (r, g, b) = frame.rgb(x as u32, y as u32);
            let Some(colour) = classify(r, g, b).filter(|c| !ambiguous.contains(c)) else { continue };
            if (bx..bx + size).contains(&x) && (by..by + size).contains(&y) && in_picture(avatar, (x - bx, y - by), size, colour) {
                continue;
            }
            match counts.iter_mut().find(|(c, _)| *c == colour) {
                Some((_, n)) => *n += 1,
                None => counts.push((colour, 1)),
            }
        }
    }
    counts.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    let needed = (size * size / 40).max(3);
    match counts.as_slice() {
        [(colour, n), rest @ ..] if *n >= needed && rest.first().is_none_or(|(_, second)| *n >= second * 2) => Some(*colour),
        _ => None,
    }
}

/// Finds each avatar on the scoreboard and reads its teammate colour. Players whose avatar isn't found
/// (or can't be told apart from another player's) are left out.
pub fn read(frame: &Frame, avatars: &[Avatar]) -> Vec<Found> {
    if avatars.is_empty() || frame.width < 64 || frame.height < 64 {
        return Vec::new();
    }
    let (image, factor) = working_image(frame);
    let integral = Integral::new(&image);
    let mut templates: Vec<Option<Fingerprint>> = avatars
        .iter()
        .map(|avatar| {
            let picture = avatar_image(avatar);
            fingerprint(&Integral::new(&picture), 0, 0, picture.width.min(picture.height))
        })
        .collect();
    let snapshot = templates.clone();
    for (i, template) in templates.iter_mut().enumerate() {
        let duplicate =
            snapshot.iter().enumerate().any(|(j, other)| j != i && matches!((&snapshot[i], other), (Some(a), Some(b)) if correlation(a, b) > SAME_PICTURE));
        if duplicate {
            *template = None;
        }
    }

    let base = 24.0 * image.height as f32 / 1080.0;
    let mut sizes: Vec<usize> = [0.9f32, 1.0, 1.12].iter().map(|k| (base * k).round() as usize).filter(|s| *s >= GRID * 2).collect();
    sizes.dedup();
    let area = (image.width / 20, image.height / 50, image.width - image.width / 20, image.height - image.height / 50);

    // Fast pass at every other pixel, one thread per size; then a robust check around each candidate.
    let normalised_templates: Vec<Option<Fingerprint>> = templates.iter().map(|t| t.map(|t| normalised(&t, &ALL_BLOCKS))).collect();
    let per_size: Vec<Vec<Vec<Hit>>> = std::thread::scope(|scope| {
        let (integral, templates) = (&integral, &normalised_templates);
        let jobs: Vec<_> = sizes.iter().map(|&size| scope.spawn(move || candidates(integral, templates, size, area, 2))).collect();
        jobs.into_iter().filter_map(|job| job.join().ok()).collect()
    });
    let mut scored: Vec<(usize, Hit)> = Vec::new();
    for (i, template) in templates.iter().enumerate() {
        let Some(template) = template else { continue };
        for lists in &per_size {
            for candidate in &lists[i] {
                if let Some(hit) = refine(&integral, &image, template, *candidate).filter(|h| h.score >= MIN_SCORE) {
                    scored.push((i, hit));
                }
            }
        }
    }

    // Best matches first; each avatar gets one box, and two avatars can't share a box.
    scored.sort_by(|a, b| b.1.score.total_cmp(&a.1.score));
    let mut placed: Vec<(usize, Hit)> = Vec::new();
    for (i, hit) in scored {
        let clash = placed.iter().any(|(j, t)| *j == i || (t.x.abs_diff(hit.x) < hit.size / 2 && t.y.abs_diff(hit.y) < hit.size / 2));
        if !clash {
            placed.push((i, hit));
        }
    }
    placed
        .into_iter()
        .map(|(i, hit)| Found {
            steam_id: avatars[i].steam_id.clone(),
            colour: colour_in_box(frame, &avatars[i], hit.x * factor, hit.y * factor, hit.size * factor),
            score: hit.score,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A distinct, deterministic 64x64 avatar picture.
    fn picture(seed: u32, tint: [u8; 3]) -> Avatar {
        let mut state = seed.wrapping_mul(2_654_435_761).wrapping_add(1);
        let mut rgb = Vec::with_capacity(64 * 64 * 3);
        let blocks: Vec<[u8; 3]> = (0..64)
            .map(|_| {
                state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                let v = (state >> 8) as u8;
                [v.wrapping_add(tint[0]) / 2 + 40, v.wrapping_mul(3).wrapping_add(tint[1]) / 2 + 30, v.wrapping_mul(7).wrapping_add(tint[2]) / 2 + 20]
            })
            .collect();
        for y in 0..64 {
            for x in 0..64 {
                rgb.extend_from_slice(&blocks[(y / 8) * 8 + x / 8]);
            }
        }
        Avatar { steam_id: format!("7656119800000000{seed}"), width: 64, height: 64, rgb }
    }

    #[derive(Clone, Copy)]
    enum Marker {
        Bar,
        Ring,
        Corner,
    }

    struct Canvas(Frame);

    impl Canvas {
        fn new(width: u32, height: u32) -> Self {
            let mut bgra = Vec::with_capacity((width * height * 4) as usize);
            for i in 0..width * height {
                // A dark, slightly noisy background, like the blurred world behind the scoreboard.
                let n = (i.wrapping_mul(2_654_435_761) >> 28) as u8;
                bgra.extend_from_slice(&[12 + n, 14 + n, 16 + n, 255]);
            }
            Self(Frame { width, height, bgra })
        }

        fn set(&mut self, x: usize, y: usize, rgb: [u8; 3]) {
            let i = (y * self.0.width as usize + x) * 4;
            self.0.bgra[i..i + 3].copy_from_slice(&[rgb[2], rgb[1], rgb[0]]);
        }

        fn avatar(&mut self, avatar: &Avatar, x: usize, y: usize, size: usize, wash: f32, colour: Option<[u8; 3]>) {
            self.avatar_with(avatar, (x, y, size), wash, colour, Marker::Bar);
        }

        /// Draws an avatar scaled to `size`, dimmed by `wash` (dead players), with an optional colour marker
        /// standing in for CS2's `player-color` element, whose exact shape isn't in the game's style files.
        fn avatar_with(&mut self, avatar: &Avatar, (x, y, size): (usize, usize, usize), wash: f32, colour: Option<[u8; 3]>, marker: Marker) {
            for dy in 0..size {
                for dx in 0..size {
                    let i = ((dy * 64 / size) * 64 + dx * 64 / size) * 3;
                    let px = [0, 1, 2].map(|c| (avatar.rgb[i + c] as f32 * wash) as u8);
                    self.set(x + dx, y + dy, px);
                }
            }
            let Some(colour) = colour else { return };
            let shade = colour.map(|c| (c as f32 * wash) as u8);
            let bar = (size / 6).max(2);
            for dy in 0..size {
                for dx in 0..size {
                    let drawn = match marker {
                        Marker::Bar => dy >= size - bar,
                        Marker::Ring => dx < 2 || dy < 2 || dx >= size - 2 || dy >= size - 2,
                        Marker::Corner => dx >= size - size * 2 / 5 && dy >= size - size * 2 / 5,
                    };
                    if drawn {
                        self.set(x + dx, y + dy, shade);
                    }
                }
            }
        }
    }

    const YELLOW: [u8; 3] = [248, 246, 45];
    const PURPLE: [u8; 3] = [192, 54, 153];
    const GREEN: [u8; 3] = [29, 162, 132];
    const BLUE: [u8; 3] = [136, 206, 245];
    const ORANGE: [u8; 3] = [255, 155, 37];

    #[test]
    fn reads_each_teammate_colour_and_none_for_opponents() {
        // 1280x720: avatars are 16 px, as 24 px at 1080p scaled to this height.
        let mut canvas = Canvas::new(1280, 720);
        let mates: Vec<Avatar> = (1..=5).map(|i| picture(i, [0, 0, 0])).collect();
        // One avatar picture full of teammate yellow; its colour marker is blue.
        let mut yellowish = picture(6, [0, 0, 0]);
        for px in yellowish.rgb.chunks_exact_mut(3).take(64 * 40) {
            px.copy_from_slice(&YELLOW);
        }
        let enemies: Vec<Avatar> = (7..=10).map(|i| picture(i, [60, 0, 90])).collect();
        let colours = [YELLOW, PURPLE, GREEN, ORANGE];
        for (row, avatar) in mates.iter().take(4).enumerate() {
            canvas.avatar(avatar, 400, 160 + row * 22, 16, if row == 2 { 0.6 } else { 1.0 }, Some(colours[row]));
        }
        canvas.avatar(&yellowish, 400, 160 + 4 * 22, 16, 1.0, Some(BLUE));
        for (row, avatar) in enemies.iter().enumerate() {
            canvas.avatar(avatar, 400, 360 + row * 22, 16, 1.0, None);
        }
        let mut all: Vec<Avatar> = mates.iter().take(4).cloned().collect();
        all.push(yellowish);
        all.extend(enemies.iter().cloned());
        // A player who isn't on the scoreboard.
        all.push(picture(5, [90, 90, 0]));

        let found = read(&canvas.0, &all);
        let colour_of = |avatar: &Avatar| found.iter().find(|f| f.steam_id == avatar.steam_id).map(|f| f.colour);
        assert_eq!(colour_of(&all[0]), Some(Some(TeammateColour::Yellow)));
        assert_eq!(colour_of(&all[1]), Some(Some(TeammateColour::Purple)));
        assert_eq!(colour_of(&all[2]), Some(Some(TeammateColour::Green)), "a dead (greyed) teammate");
        assert_eq!(colour_of(&all[3]), Some(Some(TeammateColour::Orange)));
        assert_eq!(colour_of(&all[4]), Some(Some(TeammateColour::Blue)), "yellow in the picture is not the marker");
        for enemy in &enemies {
            assert_eq!(colour_of(enemy), Some(None), "opponents have no colour");
        }
        assert!(found.iter().all(|f| f.score >= MIN_SCORE));
    }

    #[test]
    fn marker_shape_does_not_matter() {
        for marker in [Marker::Ring, Marker::Corner] {
            let mut canvas = Canvas::new(1280, 720);
            let players: Vec<Avatar> = (11..=15).map(|i| picture(i, [0, 0, 0])).collect();
            let colours = [YELLOW, PURPLE, GREEN, BLUE, ORANGE];
            for (row, avatar) in players.iter().enumerate() {
                canvas.avatar_with(avatar, (520, 200 + row * 22, 16), 1.0, Some(colours[row]), marker);
            }
            let found = read(&canvas.0, &players);
            let expected = [TeammateColour::Yellow, TeammateColour::Purple, TeammateColour::Green, TeammateColour::Blue, TeammateColour::Orange];
            for (avatar, colour) in players.iter().zip(expected) {
                assert_eq!(found.iter().find(|f| f.steam_id == avatar.steam_id).map(|f| f.colour), Some(Some(colour)));
            }
        }
    }

    /// `cargo test --release -- --ignored --nocapture scoreboard`: time for a 1080p frame and 10 players.
    #[test]
    #[ignore]
    fn speed_at_1080p() {
        let mut canvas = Canvas::new(1920, 1080);
        let players: Vec<Avatar> = (21..=30).map(|i| picture(i, [0, 0, 0])).collect();
        for (row, avatar) in players.iter().enumerate() {
            canvas.avatar(avatar, 640, 300 + row * 34, 24, 1.0, (row < 5).then_some(YELLOW));
        }
        let started = std::time::Instant::now();
        let found = read(&canvas.0, &players);
        eprintln!("1080p, 10 players: {:?}, {} found", started.elapsed(), found.len());
        assert_eq!(found.len(), 10);
    }

    #[test]
    fn identical_avatars_and_empty_frames_give_nothing() {
        let mut canvas = Canvas::new(1280, 720);
        let a = picture(1, [0, 0, 0]);
        let mut b = a.clone();
        b.steam_id = "other".into();
        canvas.avatar(&a, 400, 300, 16, 1.0, Some(YELLOW));
        assert!(read(&canvas.0, &[a.clone(), b]).is_empty(), "default avatars can't be told apart");
        assert!(read(&Canvas::new(1280, 720).0, &[a]).is_empty(), "nothing on screen, nothing found");
    }
}
