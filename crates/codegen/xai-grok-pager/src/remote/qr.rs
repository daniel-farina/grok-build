//! QR rendering for remote control URLs (terminal unicode + browser SVG).

/// Render `data` as a compact Unicode QR block for the terminal.
///
/// Returns `None` if the payload is too large / invalid for a QR code.
///
/// Quiet zone is reduced so the QR fits more easily in modal panes without
/// mid-row wrapping (wrapping destroys scannability).
pub fn render_qr_unicode(data: &str) -> Option<String> {
    let code = qrcode::QrCode::new(data.as_bytes()).ok()?;
    // Dense1x2 packs two module rows per character — readable on most terminals.
    // Dark modules = full block on dark terminals so contrast matches scrollback.
    let image = code
        .render::<qrcode::render::unicode::Dense1x2>()
        .quiet_zone(false)
        .dark_color(qrcode::render::unicode::Dense1x2::Light)
        .light_color(qrcode::render::unicode::Dense1x2::Dark)
        .build();
    // Add a 1-module manual quiet zone of spaces so scanners still work,
    // without the library's larger default padding.
    let padded = pad_quiet_zone(&image, 1);
    Some(padded)
}

fn pad_quiet_zone(qr: &str, pad: usize) -> String {
    let lines: Vec<&str> = qr.lines().filter(|l| !l.is_empty()).collect();
    if lines.is_empty() {
        return qr.to_string();
    }
    let width = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0);
    let blank = " ".repeat(width + pad * 2);
    let mut out = String::new();
    for _ in 0..pad {
        out.push_str(&blank);
        out.push('\n');
    }
    for line in lines {
        out.push_str(&" ".repeat(pad));
        out.push_str(line);
        // Pad short lines so every row has equal width.
        let extra = width.saturating_sub(line.chars().count());
        out.push_str(&" ".repeat(extra + pad));
        out.push('\n');
    }
    for _ in 0..pad {
        out.push_str(&blank);
        out.push('\n');
    }
    out
}

/// Render `data` as an SVG QR code for the browser QR page.
pub fn render_qr_svg(data: &str) -> Option<String> {
    let code = qrcode::QrCode::new(data.as_bytes()).ok()?;
    let image = code
        .render::<qrcode::render::svg::Color>()
        .min_dimensions(280, 280)
        .dark_color(qrcode::render::svg::Color("#0b0d10"))
        .light_color(qrcode::render::svg::Color("#ffffff"))
        .build();
    Some(image)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_http_url() {
        let qr = render_qr_unicode("http://100.64.0.1:7788/s/abc/").expect("qr");
        assert!(qr.lines().count() > 5);
        assert!(qr.contains('█') || qr.contains('▀') || qr.contains('▄') || qr.contains(' '));
    }

    #[test]
    fn renders_svg() {
        let svg = render_qr_svg("http://100.64.0.1:7788/s/abc/").expect("svg");
        assert!(svg.contains("<svg") || svg.contains("svg"));
    }
}
