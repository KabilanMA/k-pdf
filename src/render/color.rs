//! Color spaces (ISO 32000 §8.6), converted to sRGB.
//!
//! ICC profiles are not interpreted: an ICCBased space is treated as the
//! device space with the same number of components, which is what most
//! lightweight viewers do and is visually close for typical profiles.

use std::rc::Rc;

use super::function::Function;
use crate::document::Document;
use crate::object::{Dictionary, Object};

#[derive(Debug, Clone)]
pub(crate) enum ColorSpace {
    Gray,
    Rgb,
    Cmyk,
    Lab { white: [f64; 3], range: [f64; 4] },
    Indexed { base: Box<ColorSpace>, hival: usize, lookup: Rc<Vec<u8>> },
    /// Separation and DeviceN: tint components converted through a function.
    Tint { n: usize, alt: Box<ColorSpace>, func: Option<Rc<Function>>, none: bool },
    /// Patterns; uncolored patterns carry an underlying space.
    Pattern(Option<Box<ColorSpace>>),
}

impl ColorSpace {
    /// Parses a color space from a name or array, looking up named
    /// spaces in the `/ColorSpace` resource dictionary.
    pub fn parse(doc: &Document, obj: &Object, resources: &Dictionary) -> Option<ColorSpace> {
        Self::parse_depth(doc, obj, resources, 0)
    }

    fn parse_depth(doc: &Document, obj: &Object, resources: &Dictionary, depth: usize) -> Option<ColorSpace> {
        if depth > 8 {
            return None;
        }
        let obj = doc.resolve(obj).ok()?;
        match &obj {
            Object::Name(n) => match n.as_slice() {
                b"DeviceGray" | b"G" | b"CalGray" => Some(ColorSpace::Gray),
                b"DeviceRGB" | b"RGB" | b"CalRGB" => Some(ColorSpace::Rgb),
                b"DeviceCMYK" | b"CMYK" => Some(ColorSpace::Cmyk),
                b"Pattern" => Some(ColorSpace::Pattern(None)),
                other => {
                    // A named resource.
                    let spaces = doc.get_dict(resources, b"ColorSpace")?;
                    let entry = spaces.get(other)?.clone();
                    Self::parse_depth(doc, &entry, resources, depth + 1)
                }
            },
            Object::Array(a) => {
                let family = a.first()?.as_name()?;
                let arg = |i: usize| a.get(i).and_then(|o| doc.resolve(o).ok());
                match family {
                    b"DeviceGray" | b"G" | b"CalGray" => Some(ColorSpace::Gray),
                    b"DeviceRGB" | b"RGB" | b"CalRGB" => Some(ColorSpace::Rgb),
                    b"DeviceCMYK" | b"CMYK" => Some(ColorSpace::Cmyk),
                    b"ICCBased" => {
                        let stream = arg(1)?;
                        let dict = stream.as_dict()?;
                        if let Some(alt) = dict.get(b"Alternate")
                            && let Some(cs) = Self::parse_depth(doc, alt, resources, depth + 1)
                        {
                            return Some(cs);
                        }
                        match doc.get_resolved(dict, b"N").ok()?.as_i64()? {
                            1 => Some(ColorSpace::Gray),
                            4 => Some(ColorSpace::Cmyk),
                            _ => Some(ColorSpace::Rgb),
                        }
                    }
                    b"Lab" => {
                        let d = arg(1).and_then(|o| o.as_dict().cloned()).unwrap_or_default();
                        let nums = |k: &[u8]| -> Vec<f64> {
                            doc.get_resolved(&d, k)
                                .ok()
                                .and_then(|o| o.as_array().map(|a| a.iter().filter_map(Object::as_f64).collect()))
                                .unwrap_or_default()
                        };
                        let w = nums(b"WhitePoint");
                        let r = nums(b"Range");
                        Some(ColorSpace::Lab {
                            white: [w.first().copied().unwrap_or(0.9505), w.get(1).copied().unwrap_or(1.0), w.get(2).copied().unwrap_or(1.089)],
                            range: if r.len() == 4 { [r[0], r[1], r[2], r[3]] } else { [-100.0, 100.0, -100.0, 100.0] },
                        })
                    }
                    b"Indexed" | b"I" => {
                        let base = Self::parse_depth(doc, a.get(1)?, resources, depth + 1)?;
                        let hival = arg(2)?.as_i64()?.clamp(0, 255) as usize;
                        let lookup = match arg(3)? {
                            Object::String(s) => s,
                            Object::Stream(s) => doc.decode_stream(&s).ok()?,
                            _ => return None,
                        };
                        Some(ColorSpace::Indexed { base: Box::new(base), hival, lookup: Rc::new(lookup) })
                    }
                    b"Separation" | b"DeviceN" => {
                        let (n, none) = if family == b"Separation" {
                            (1, arg(1).and_then(|o| o.as_name().map(|n| n == b"None")).unwrap_or(false))
                        } else {
                            (arg(1).and_then(|o| o.as_array().map(<[_]>::len)).unwrap_or(1), false)
                        };
                        let alt = Self::parse_depth(doc, a.get(2)?, resources, depth + 1).unwrap_or(ColorSpace::Gray);
                        let func = a.get(3).and_then(|f| Function::parse(doc, f)).map(Rc::new);
                        Some(ColorSpace::Tint { n, alt: Box::new(alt), func, none })
                    }
                    b"Pattern" => {
                        let under = a.get(1).and_then(|o| Self::parse_depth(doc, o, resources, depth + 1));
                        Some(ColorSpace::Pattern(under.map(Box::new)))
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// For a space given by component count (images without /ColorSpace).
    pub fn from_components(n: usize) -> ColorSpace {
        match n {
            1 => ColorSpace::Gray,
            4 => ColorSpace::Cmyk,
            _ => ColorSpace::Rgb,
        }
    }

    pub fn components(&self) -> usize {
        match self {
            ColorSpace::Gray | ColorSpace::Indexed { .. } => 1,
            ColorSpace::Rgb | ColorSpace::Lab { .. } => 3,
            ColorSpace::Cmyk => 4,
            ColorSpace::Tint { n, .. } => *n,
            ColorSpace::Pattern(under) => under.as_ref().map_or(0, |u| u.components()),
        }
    }

    /// The initial color when this space is selected (§8.6.8).
    pub fn initial_color(&self) -> Vec<f64> {
        match self {
            ColorSpace::Cmyk => vec![0.0, 0.0, 0.0, 1.0],
            ColorSpace::Tint { n, .. } => vec![1.0; *n],
            ColorSpace::Lab { .. } => vec![0.0, 0.0, 0.0],
            other => vec![0.0; other.components()],
        }
    }

    /// Default image `/Decode` range for component `i`.
    pub fn default_decode(&self, i: usize, bpc: u32) -> (f64, f64) {
        match self {
            ColorSpace::Indexed { .. } => (0.0, ((1u32 << bpc.min(16)) - 1) as f64),
            ColorSpace::Lab { range, .. } => match i {
                0 => (0.0, 100.0),
                1 => (range[0], range[1]),
                _ => (range[2], range[3]),
            },
            _ => (0.0, 1.0),
        }
    }

    /// Converts components to sRGB in 0..1. `None` for Separation /None
    /// (which must not paint anything).
    pub fn to_rgb(&self, c: &[f64]) -> Option<[f64; 3]> {
        let get = |i: usize| c.get(i).copied().unwrap_or(0.0).clamp(0.0, 1.0);
        Some(match self {
            ColorSpace::Gray => [get(0); 3],
            ColorSpace::Rgb => [get(0), get(1), get(2)],
            ColorSpace::Cmyk => cmyk_to_rgb(get(0), get(1), get(2), get(3)),
            ColorSpace::Lab { white, range } => {
                let l = c.first().copied().unwrap_or(0.0).clamp(0.0, 100.0);
                let a = c.get(1).copied().unwrap_or(0.0).clamp(range[0], range[1]);
                let b = c.get(2).copied().unwrap_or(0.0).clamp(range[2], range[3]);
                lab_to_rgb(l, a, b, white)
            }
            ColorSpace::Indexed { base, hival, lookup } => {
                let idx = (c.first().copied().unwrap_or(0.0).round().max(0.0) as usize).min(*hival);
                let n = base.components();
                let comps: Vec<f64> =
                    (0..n).map(|i| lookup.get(idx * n + i).copied().unwrap_or(0) as f64 / 255.0).collect();
                return base.to_rgb(&comps);
            }
            ColorSpace::Tint { alt, func, none, .. } => {
                if *none {
                    return None;
                }
                match func {
                    Some(f) => return alt.to_rgb(&f.eval(c)),
                    // No usable tint transform: show the tint as gray.
                    None => [1.0 - get(0); 3],
                }
            }
            ColorSpace::Pattern(under) => return under.as_ref().and_then(|u| u.to_rgb(c)),
        })
    }
}

pub(crate) fn cmyk_to_rgb(c: f64, m: f64, y: f64, k: f64) -> [f64; 3] {
    [(1.0 - c) * (1.0 - k), (1.0 - m) * (1.0 - k), (1.0 - y) * (1.0 - k)]
}

fn lab_to_rgb(l: f64, a: f64, b: f64, white: &[f64; 3]) -> [f64; 3] {
    let fy = (l + 16.0) / 116.0;
    let fx = fy + a / 500.0;
    let fz = fy - b / 200.0;
    let g = |t: f64| if t > 6.0 / 29.0 { t * t * t } else { 108.0 / 841.0 * (t - 4.0 / 29.0) };
    let (x, y, z) = (white[0] * g(fx), white[1] * g(fy), white[2] * g(fz));
    let r = 3.2406 * x - 1.5372 * y - 0.4986 * z;
    let gg = -0.9689 * x + 1.8758 * y + 0.0415 * z;
    let bb = 0.0557 * x - 0.2040 * y + 1.0570 * z;
    let gamma = |v: f64| {
        let v = v.clamp(0.0, 1.0);
        if v <= 0.0031308 { 12.92 * v } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 }
    };
    [gamma(r), gamma(gg), gamma(bb)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_spaces() {
        assert_eq!(ColorSpace::Gray.to_rgb(&[0.5]), Some([0.5; 3]));
        assert_eq!(ColorSpace::Cmyk.to_rgb(&[0.0, 0.0, 0.0, 1.0]), Some([0.0; 3]));
        assert_eq!(ColorSpace::Cmyk.to_rgb(&[1.0, 0.0, 0.0, 0.0]), Some([0.0, 1.0, 1.0]));
        let idx = ColorSpace::Indexed { base: Box::new(ColorSpace::Rgb), hival: 1, lookup: Rc::new(vec![255, 0, 0, 0, 0, 255]) };
        assert_eq!(idx.to_rgb(&[1.0]), Some([0.0, 0.0, 1.0]));
        let none = ColorSpace::Tint { n: 1, alt: Box::new(ColorSpace::Gray), func: None, none: true };
        assert_eq!(none.to_rgb(&[1.0]), None);
    }

    #[test]
    fn lab_white_is_white() {
        let lab = ColorSpace::Lab { white: [0.9505, 1.0, 1.089], range: [-100.0, 100.0, -100.0, 100.0] };
        let [r, g, b] = lab.to_rgb(&[100.0, 0.0, 0.0]).unwrap();
        assert!(r > 0.98 && g > 0.98 && b > 0.98);
    }
}
