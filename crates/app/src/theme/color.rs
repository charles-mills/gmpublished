//! Colour-space maths for the theme tokens: sRGB bytes <-> HSL.
//!
//! No backend contact — it lived under `bridge` only because the theme
//! preset resolution next to it did.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Rgb {
    pub(crate) r: u8,
    pub(crate) g: u8,
    pub(crate) b: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DerivedColor {
    pub(crate) base: Rgb,
    pub(crate) dark: Rgb,
}

pub fn derive(rgb: u32) -> DerivedColor {
    let base = Rgb {
        r: ((rgb & 0xFF0000) >> 16) as u8,
        g: ((rgb & 0x00FF00) >> 8) as u8,
        b: (rgb & 0x0000FF) as u8,
    };
    let (h, s, l) = rgb_to_hsl(base);
    let dark = hsl_to_rgb(h, s, l * 0.85);
    DerivedColor { base, dark }
}

fn rgb_to_hsl(rgb: Rgb) -> (f64, f64, f64) {
    let r = f64::from(rgb.r) / 255.0;
    let g = f64::from(rgb.g) / 255.0;
    let b = f64::from(rgb.b) / 255.0;

    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;

    if (max - min).abs() < f64::EPSILON {
        return (0.0, 0.0, l);
    }

    let d = max - min;
    let s = if l > 0.5 {
        d / (2.0 - max - min)
    } else {
        d / (max + min)
    };

    let mut h = if (max - r).abs() < f64::EPSILON {
        (g - b) / d + if g < b { 6.0 } else { 0.0 }
    } else if (max - g).abs() < f64::EPSILON {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    h /= 6.0;

    (h, s, l)
}

fn hsl_to_rgb(h: f64, s: f64, l: f64) -> Rgb {
    if s.abs() < f64::EPSILON {
        let channel = float_channel(l);
        return Rgb {
            r: channel,
            g: channel,
            b: channel,
        };
    }

    let q = if l < 0.5 {
        l * (1.0 + s)
    } else {
        l + s - l * s
    };
    let p = 2.0 * l - q;

    Rgb {
        r: float_channel(hue_to_rgb(p, q, h + (1.0 / 3.0))),
        g: float_channel(hue_to_rgb(p, q, h)),
        b: float_channel(hue_to_rgb(p, q, h - (1.0 / 3.0))),
    }
}

fn hue_to_rgb(p: f64, q: f64, mut t: f64) -> f64 {
    if t < 0.0 {
        t += 1.0;
    }
    if t > 1.0 {
        t -= 1.0;
    }
    if t < 1.0 / 6.0 {
        return p + (q - p) * 6.0 * t;
    }
    if t < 1.0 / 2.0 {
        return q;
    }
    if t < 2.0 / 3.0 {
        return p + (q - p) * ((2.0 / 3.0) - t) * 6.0;
    }
    p
}

fn float_channel(value: f64) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}
