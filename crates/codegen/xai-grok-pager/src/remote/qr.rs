//! Terminal QR rendering for the remote control URL.

/// Render `data` as a compact Unicode QR block for the terminal.
///
/// Returns `None` if the payload is too large / invalid for a QR code.
pub fn render_qr_unicode(data: &str) -> Option<String> {
    let code = qrcode::QrCode::new(data.as_bytes()).ok()?;
    // Dense1x2 packs two rows per character — readable on most terminals.
    let image = code
        .render::<qrcode::render::unicode::Dense1x2>()
        .dark_color(qrcode::render::unicode::Dense1x2::Light)
        .light_color(qrcode::render::unicode::Dense1x2::Dark)
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
}
