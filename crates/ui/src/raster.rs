//! Software painter for egui output (ADR 0006).
//!
//! Draws egui's tessellated triangle meshes into an opaque framebuffer of `0x00RRGGBB`
//! pixels (little-endian bytes B, G, R, X: the 32-bit layout both X11 `ZPixmap` at depth 24
//! and Win32 `BI_RGB` DIBs use). Colours are sRGB with premultiplied alpha and blending is
//! done in that space, as egui's vertex colours and textures are defined. egui feathers its
//! own edges, so triangles are filled by pixel centre without further anti-aliasing.
//!
//! No GPU, no window: the same code renders editor frames for X11, Win32 and tests.

use egui::epaint::{ClippedPrimitive, ImageData, Primitive, TextureId, Vertex, textures::TextureFilter};
use egui::{Color32, TexturesDelta};
use std::collections::HashMap;

struct Texture {
    width: usize,
    height: usize,
    /// Premultiplied sRGBA texels.
    texels: Vec<Color32>,
    linear: bool,
}

impl Texture {
    /// Premultiplied RGBA in 0..=1 at normalized `uv` (clamped to the edge).
    fn sample(&self, u: f32, v: f32) -> [f32; 4] {
        let fx = (u * self.width as f32 - 0.5).clamp(0.0, (self.width - 1) as f32);
        let fy = (v * self.height as f32 - 0.5).clamp(0.0, (self.height - 1) as f32);
        if !self.linear {
            return texel(self.texels[(fy.round() as usize) * self.width + fx.round() as usize]);
        }
        let (x0, y0) = (fx.floor() as usize, fy.floor() as usize);
        let (x1, y1) = ((x0 + 1).min(self.width - 1), (y0 + 1).min(self.height - 1));
        let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
        let at = |x: usize, y: usize| texel(self.texels[y * self.width + x]);
        let (a, b, c, d) = (at(x0, y0), at(x1, y0), at(x0, y1), at(x1, y1));
        let mut out = [0.0; 4];
        for i in 0..4 {
            let top = a[i] + (b[i] - a[i]) * tx;
            let bottom = c[i] + (d[i] - c[i]) * tx;
            out[i] = top + (bottom - top) * ty;
        }
        out
    }
}

fn texel(c: Color32) -> [f32; 4] {
    [f32::from(c.r()) / 255.0, f32::from(c.g()) / 255.0, f32::from(c.b()) / 255.0, f32::from(c.a()) / 255.0]
}

/// Software egui painter with its own texture store and framebuffer.
#[derive(Default)]
pub struct SoftwarePainter {
    textures: HashMap<TextureId, Texture>,
    width: usize,
    height: usize,
    pixels: Vec<u32>,
}

impl SoftwarePainter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply texture uploads (call before [`Self::paint`]).
    pub fn set_textures(&mut self, delta: &TexturesDelta) {
        for (id, d) in delta.set.iter().flat_map(|(id, ds)| ds.iter().map(move |d| (id, d))) {
            let ImageData::Color(image) = &d.image;
            let [w, h] = image.size;
            let linear = d.options.magnification == TextureFilter::Linear;
            match d.pos {
                None => {
                    self.textures
                        .insert(*id, Texture { width: w.max(1), height: h.max(1), texels: if w * h == 0 { vec![Color32::TRANSPARENT] } else { image.pixels.clone() }, linear });
                }
                Some([x, y]) => {
                    let Some(t) = self.textures.get_mut(id) else { continue };
                    t.linear = linear;
                    for row in 0..h {
                        let ty = y + row;
                        if ty >= t.height {
                            break;
                        }
                        let n = w.min(t.width.saturating_sub(x));
                        t.texels[ty * t.width + x..ty * t.width + x + n].copy_from_slice(&image.pixels[row * w..row * w + n]);
                    }
                }
            }
        }
    }

    /// Release textures egui no longer uses and mark the delta handled (call after
    /// [`Self::paint`]; egui asserts that every delta is acknowledged).
    pub fn finish_textures(&mut self, mut delta: TexturesDelta) {
        for id in &delta.free {
            self.textures.remove(id);
        }
        delta.clear();
    }

    /// Render `primitives` (in points) into a `width` × `height` pixel framebuffer.
    pub fn paint(&mut self, width: usize, height: usize, clear: Color32, primitives: &[ClippedPrimitive], pixels_per_point: f32) {
        self.width = width;
        self.height = height;
        let bg = pack(f32::from(clear.r()), f32::from(clear.g()), f32::from(clear.b()));
        self.pixels.clear();
        self.pixels.resize(width * height, bg);
        for prim in primitives {
            let Primitive::Mesh(mesh) = &prim.primitive else { continue };
            let Some(tex) = self.textures.get(&mesh.texture_id) else { continue };
            let clip = [
                ((prim.clip_rect.min.x * pixels_per_point).round().max(0.0) as usize).min(width),
                ((prim.clip_rect.min.y * pixels_per_point).round().max(0.0) as usize).min(height),
                ((prim.clip_rect.max.x * pixels_per_point).round().max(0.0) as usize).min(width),
                ((prim.clip_rect.max.y * pixels_per_point).round().max(0.0) as usize).min(height),
            ];
            if clip[0] >= clip[2] || clip[1] >= clip[3] {
                continue;
            }
            for tri in mesh.indices.as_chunks::<3>().0 {
                let (Some(a), Some(b), Some(c)) = (mesh.vertices.get(tri[0] as usize), mesh.vertices.get(tri[1] as usize), mesh.vertices.get(tri[2] as usize)) else { continue };
                fill_triangle(&mut self.pixels, width, clip, [a, b, c], pixels_per_point, tex);
            }
        }
    }

    pub fn size(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    /// The framebuffer, row by row, `0x00RRGGBB` per pixel.
    pub fn pixels(&self) -> &[u32] {
        &self.pixels
    }

    /// One pixel as an opaque colour (tests).
    pub fn pixel(&self, x: usize, y: usize) -> Color32 {
        let p = self.pixels[y * self.width + x];
        Color32::from_rgb((p >> 16) as u8, (p >> 8) as u8, p as u8)
    }
}

fn pack(r: f32, g: f32, b: f32) -> u32 {
    let q = |v: f32| (v.round().clamp(0.0, 255.0)) as u32;
    (q(r) << 16) | (q(g) << 8) | q(b)
}

fn edge(a: (f32, f32), b: (f32, f32), c: (f32, f32)) -> f32 {
    (b.0 - a.0) * (c.1 - a.1) - (b.1 - a.1) * (c.0 - a.0)
}

/// Fill one triangle; `clip` is `[x0, y0, x1, y1)` in pixels, already inside the framebuffer.
fn fill_triangle(fb: &mut [u32], width: usize, clip: [usize; 4], v: [&Vertex; 3], ppp: f32, tex: &Texture) {
    let p = v.map(|v| (v.pos.x * ppp, v.pos.y * ppp));
    let area = edge(p[0], p[1], p[2]);
    if !area.is_finite() || area.abs() < 1e-6 {
        return;
    }
    let sign = area.signum();
    let inv_area = 1.0 / area.abs();
    let min_x = p.iter().map(|q| q.0).fold(f32::INFINITY, f32::min).floor().max(clip[0] as f32) as usize;
    let max_x = (p.iter().map(|q| q.0).fold(f32::NEG_INFINITY, f32::max).ceil().max(0.0) as usize).min(clip[2]);
    let min_y = p.iter().map(|q| q.1).fold(f32::INFINITY, f32::min).floor().max(clip[1] as f32) as usize;
    let max_y = (p.iter().map(|q| q.1).fold(f32::NEG_INFINITY, f32::max).ceil().max(0.0) as usize).min(clip[3]);
    if min_x >= max_x || min_y >= max_y {
        return;
    }
    let col = v.map(|v| [f32::from(v.color.r()), f32::from(v.color.g()), f32::from(v.color.b()), f32::from(v.color.a())]);
    // the common case: a flat-coloured shape sampling one texel (rects, lines, feathering)
    let flat = v[0].uv == v[1].uv && v[1].uv == v[2].uv;
    let flat_texel = flat.then(|| tex.sample(v[0].uv.x, v[0].uv.y));
    // fill rule for pixel centres exactly on an edge: an edge shared by two triangles appears
    // in them with opposite orientation, so exactly one of them owns those pixels (otherwise a
    // translucent quad would be blended twice along its diagonal)
    let owns = |a: (f32, f32), b: (f32, f32)| {
        let (ca, cb) = (-(b.1 - a.1) * sign, (b.0 - a.0) * sign);
        ca > 0.0 || (ca == 0.0 && cb > 0.0)
    };
    let own = [owns(p[1], p[2]), owns(p[2], p[0]), owns(p[0], p[1])];
    let same_colour = v[0].color == v[1].color && v[1].color == v[2].color;
    for y in min_y..max_y {
        let py = y as f32 + 0.5;
        let row = y * width;
        for x in min_x..max_x {
            let q = (x as f32 + 0.5, py);
            let w0 = edge(p[1], p[2], q) * sign;
            let w1 = edge(p[2], p[0], q) * sign;
            let w2 = edge(p[0], p[1], q) * sign;
            if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 || (w0 == 0.0 && !own[0]) || (w1 == 0.0 && !own[1]) || (w2 == 0.0 && !own[2]) {
                continue;
            }
            let (l0, l1, l2) = (w0 * inv_area, w1 * inv_area, w2 * inv_area);
            let c = if same_colour { col[0] } else { std::array::from_fn(|i| col[0][i] * l0 + col[1][i] * l1 + col[2][i] * l2) };
            let t = match flat_texel {
                Some(t) => t,
                None => tex.sample(v[0].uv.x * l0 + v[1].uv.x * l1 + v[2].uv.x * l2, v[0].uv.y * l0 + v[1].uv.y * l1 + v[2].uv.y * l2),
            };
            // premultiplied source over opaque destination
            let src = [c[0] * t[0], c[1] * t[1], c[2] * t[2], c[3] * t[3]];
            let a = src[3] / 255.0;
            if a <= 0.0 && src[0] <= 0.0 && src[1] <= 0.0 && src[2] <= 0.0 {
                continue;
            }
            let d = fb[row + x];
            let keep = 1.0 - a.clamp(0.0, 1.0);
            fb[row + x] = pack(src[0] + ((d >> 16) & 0xFF) as f32 * keep, src[1] + ((d >> 8) & 0xFF) as f32 * keep, src[2] + (d & 0xFF) as f32 * keep);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Pos2, Rect, pos2, vec2};

    /// Run one egui frame of `draw` at `ppp` and paint it.
    fn render(size: (f32, f32), ppp: f32, draw: impl FnMut(&mut egui::Ui)) -> SoftwarePainter {
        let ctx = egui::Context::default();
        let mut draw = draw;
        let input = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(size.0, size.1))), ..Default::default() };
        ctx.set_pixels_per_point(ppp);
        let out = ctx.run_ui(input, |ui| draw(ui));
        let prims = ctx.tessellate(out.shapes, out.pixels_per_point);
        let mut p = SoftwarePainter::new();
        p.set_textures(&out.textures_delta);
        p.paint((size.0 * ppp) as usize, (size.1 * ppp) as usize, Color32::BLACK, &prims, out.pixels_per_point);
        p.finish_textures(out.textures_delta);
        p
    }

    #[test]
    fn filled_rect_covers_exactly_its_pixels_at_any_scale() {
        for ppp in [1.0f32, 1.5, 2.0] {
            let p = render((100.0, 60.0), ppp, |ui| {
                ui.painter().rect_filled(Rect::from_min_max(pos2(10.0, 10.0), pos2(50.0, 30.0)), 0.0, Color32::from_rgb(200, 100, 50));
            });
            let at = |x: f32, y: f32| p.pixel((x * ppp) as usize, (y * ppp) as usize);
            assert_eq!(at(30.0, 20.0), Color32::from_rgb(200, 100, 50), "inside at {ppp}");
            assert_eq!(at(5.0, 5.0), Color32::BLACK, "outside at {ppp}");
            assert_eq!(at(55.0, 20.0), Color32::BLACK, "right of the rect at {ppp}");
            assert_eq!(p.size(), ((100.0 * ppp) as usize, (60.0 * ppp) as usize));
        }
    }

    #[test]
    fn translucent_shapes_blend_with_premultiplied_alpha() {
        let p = render((40.0, 40.0), 1.0, |ui| {
            let r = Rect::from_min_max(pos2(0.0, 0.0), pos2(40.0, 40.0));
            ui.painter().rect_filled(r, 0.0, Color32::from_rgb(0, 0, 200));
            ui.painter().rect_filled(r, 0.0, Color32::from_rgba_unmultiplied(255, 0, 0, 128));
        });
        // egui's premultiplied red for alpha 128; blue keeps (1 - 128/255) of 200
        let red = Color32::from_rgba_unmultiplied(255, 0, 0, 128);
        let expect_b = (200.0 * (1.0 - f32::from(red.a()) / 255.0)).round() as i32;
        for (x, y) in [(20, 20), (5, 30), (30, 5), (0, 0), (39, 39)] {
            let c = p.pixel(x, y);
            assert!(
                (i32::from(c.r()) - i32::from(red.r())).abs() <= 1 && (i32::from(c.b()) - expect_b).abs() <= 1 && c.g() == 0,
                "({x},{y}) {c:?}, want r {} b {expect_b}",
                red.r()
            );
        }
    }

    #[test]
    fn text_draws_glyphs_from_the_font_atlas_and_respects_clip_rects() {
        let p = render((200.0, 40.0), 1.0, |ui| {
            ui.painter().text(pos2(4.0, 4.0), egui::Align2::LEFT_TOP, "rawDAWg", egui::FontId::proportional(20.0), Color32::WHITE);
            // clipped: nothing of this may land outside x < 100
            let clipped = ui.painter().with_clip_rect(Rect::from_min_max(pos2(150.0, 0.0), pos2(200.0, 40.0)));
            clipped.rect_filled(Rect::from_min_max(pos2(100.0, 0.0), pos2(200.0, 40.0)), 0.0, Color32::from_rgb(0, 255, 0));
        });
        let lit = (0..100).flat_map(|x| (0..40).map(move |y| (x, y))).filter(|&(x, y)| p.pixel(x, y).r() > 128).count();
        assert!(lit > 40, "the word drew {lit} bright pixels");
        assert_eq!(p.pixel(120, 20), Color32::BLACK, "outside the clip rect");
        assert_eq!(p.pixel(170, 20), Color32::from_rgb(0, 255, 0), "inside the clip rect");
    }

    #[test]
    fn degenerate_and_offscreen_geometry_is_ignored() {
        let mut mesh = egui::Mesh::default();
        let w = Color32::WHITE;
        mesh.vertices = vec![
            Vertex { pos: pos2(5.0, 5.0), uv: egui::epaint::WHITE_UV, color: w },
            Vertex { pos: pos2(5.0, 5.0), uv: egui::epaint::WHITE_UV, color: w },
            Vertex { pos: pos2(5.0, 5.0), uv: egui::epaint::WHITE_UV, color: w },
            Vertex { pos: pos2(-500.0, -500.0), uv: egui::epaint::WHITE_UV, color: w },
            Vertex { pos: pos2(f32::NAN, 0.0), uv: egui::epaint::WHITE_UV, color: w },
        ];
        mesh.indices = vec![0, 1, 2, 3, 3, 4, 0, 1, 99];
        let ctx = egui::Context::default();
        let out = ctx.run_ui(egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(20.0, 20.0))), ..Default::default() }, |_| {});
        let mut p = SoftwarePainter::new();
        p.set_textures(&out.textures_delta);
        // straight to the painter, bypassing egui's tessellator (which would drop some of this)
        let prims = vec![ClippedPrimitive { clip_rect: Rect::EVERYTHING, primitive: Primitive::Mesh(mesh) }];
        p.paint(20, 20, Color32::BLACK, &prims, 1.0);
        p.finish_textures(out.textures_delta);
        assert!(p.pixels().iter().all(|&px| px == 0), "nothing drawn");
    }
}
