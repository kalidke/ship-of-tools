use super::LOGO_WORDMARK_PNG;

/// The startup splash is these compiled-in bytes, decoded the way `decode_and_fit` does.
#[test]
fn wordmark_png_decodes() {
    let decoded = image::ImageReader::new(std::io::Cursor::new(LOGO_WORDMARK_PNG))
        .with_guessed_format()
        .unwrap()
        .decode();
    assert!(decoded.is_ok());
}
