//! Shadings (ISO 32000 §8.7.4.5): axial and radial gradients.
//!
//! The PDF function is sampled into gradient stops. Function-based and mesh
//! shadings (types 1, 4–7) are approximated by a single average color.

use tiny_skia::{Color, GradientStop, LinearGradient, Point, RadialGradient, Shader, SpreadMode, Transform};

use super::color::ColorSpace;
use super::function::Function;
use crate::document::Document;
use crate::object::{Dictionary, Object};

/// Number of samples taken from the shading function.
const STOPS: usize = 32;

/// Builds a shader for `shading`; `transform` maps shading space to device.
pub(crate) fn shader(doc: &Document, shading: &Object, resources: &Dictionary, transform: Transform) -> Option<Shader<'static>> {
    let shading = doc.resolve(shading).ok()?;
    let dict = shading.as_dict()?;
    let nums = |key: &[u8]| -> Vec<f64> {
        doc.get_resolved(dict, key)
            .ok()
            .and_then(|o| o.as_array().map(|a| a.iter().filter_map(|v| doc.resolve(v).ok()?.as_f64()).collect()))
            .unwrap_or_default()
    };
    let space = ColorSpace::parse(doc, dict.get(b"ColorSpace")?, resources)?;
    let func = dict.get(b"Function").and_then(|f| Function::parse(doc, f));
    let domain = nums(b"Domain");
    let (t0, t1) = (domain.first().copied().unwrap_or(0.0), domain.get(1).copied().unwrap_or(1.0));

    let color_at = |t: f64| -> Option<Color> {
        let comps = match &func {
            Some(f) => f.eval(&[t]),
            None => return None,
        };
        let [r, g, b] = space.to_rgb(&comps)?;
        Color::from_rgba(r as f32, g as f32, b as f32, 1.0)
    };
    let stops = || -> Vec<GradientStop> {
        (0..STOPS)
            .filter_map(|i| {
                let pos = i as f64 / (STOPS - 1) as f64;
                color_at(t0 + pos * (t1 - t0)).map(|c| GradientStop::new(pos as f32, c))
            })
            .collect()
    };

    let kind = doc.get_resolved(dict, b"ShadingType").ok()?.as_i64()?;
    let c = nums(b"Coords");
    match kind {
        2 if c.len() >= 4 => LinearGradient::new(
            Point::from_xy(c[0] as f32, c[1] as f32),
            Point::from_xy(c[2] as f32, c[3] as f32),
            stops(),
            SpreadMode::Pad,
            transform,
        ),
        3 if c.len() >= 6 => RadialGradient::new(
            Point::from_xy(c[0] as f32, c[1] as f32),
            c[2].max(0.0) as f32,
            Point::from_xy(c[3] as f32, c[4] as f32),
            c[5].max(0.0) as f32,
            stops(),
            SpreadMode::Pad,
            transform,
        ),
        _ => {
            // Approximation: the color in the middle of the domain, or the
            // /Background color.
            let mid = color_at((t0 + t1) / 2.0).or_else(|| {
                let bg = nums(b"Background");
                let [r, g, b] = space.to_rgb(&bg)?;
                Color::from_rgba(r as f32, g as f32, b as f32, 1.0)
            })?;
            Some(Shader::SolidColor(mid))
        }
    }
}

/// The shading's /BBox, if any (in shading space).
pub(crate) fn bbox(doc: &Document, shading: &Object) -> Option<[f64; 4]> {
    let shading = doc.resolve(shading).ok()?;
    let arr = doc.get_resolved(shading.as_dict()?, b"BBox").ok()?;
    let v: Vec<f64> = arr.as_array()?.iter().filter_map(Object::as_f64).collect();
    (v.len() == 4).then(|| [v[0], v[1], v[2], v[3]])
}
