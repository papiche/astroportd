//! Code QR en SVG, pour les liens magiques à scanner avec un smartphone.

use qrcode::QrCode;
use qrcode::render::svg;

/// Rend `text` en QR (SVG autonome, fond blanc, zone de silence incluse). `None` si le texte est trop long.
pub fn svg(text: &str) -> Option<String> {
    let code = QrCode::new(text.as_bytes()).ok()?;
    Some(code.render::<svg::Color>().min_dimensions(256, 256).quiet_zone(true).dark_color(svg::Color("#000000")).light_color(svg::Color("#ffffff")).build())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_magic_link_renders_to_a_scalable_svg() {
        let link = format!("https://station.example/s/abcdef0123#{}", "ab".repeat(32));
        let s = svg(&link).unwrap();
        assert!(s.starts_with("<?xml") || s.starts_with("<svg"));
        assert!(s.contains("<svg") && s.contains("viewBox"));
        assert!(svg(&"x".repeat(10_000)).is_none(), "trop long pour un QR");
    }
}
