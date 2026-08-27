//! The vendored upstream fixture corpus: recorded terminal input under
//! `tests/data/fixtures/<name>/<n>.typescript`, with the screen it must
//! produce alongside it as `<n>.json`.
//!
//! Read-only by design. Upstream carried the serializing half of these types
//! too, for a `regen-fixtures` binary that rewrites the corpus from whatever
//! the parser currently does. That tool is deliberately not vendored: this
//! corpus is the evidence that the fork did not change parser behavior, and a
//! one-command way to overwrite the evidence with the behavior under test is
//! exactly the wrong tool to have within reach. A fixture that legitimately
//! needs to change gets edited deliberately, in a commit that says why.

use serde::de::Deserialize as _;
use std::io::Read as _;

#[derive(Clone, Debug, Default, serde::Deserialize)]
pub struct FixtureCell {
    contents: String,
    #[serde(default)]
    is_wide: bool,
    #[serde(default)]
    is_wide_continuation: bool,
    #[serde(default, deserialize_with = "deserialize_color")]
    fgcolor: vt100_ctt::Color,
    #[serde(default, deserialize_with = "deserialize_color")]
    bgcolor: vt100_ctt::Color,
    #[serde(default)]
    bold: bool,
    #[serde(default)]
    dim: bool,
    #[serde(default)]
    italic: bool,
    #[serde(default)]
    underline: bool,
    #[serde(default)]
    inverse: bool,
}

#[derive(Debug, serde::Deserialize)]
pub struct FixtureScreen {
    contents: String,
    cells: std::collections::BTreeMap<String, FixtureCell>,
    cursor_position: (u16, u16),
    #[serde(default)]
    application_keypad: bool,
    #[serde(default)]
    application_cursor: bool,
    #[serde(default)]
    hide_cursor: bool,
    #[serde(default)]
    bracketed_paste: bool,
    #[serde(default, deserialize_with = "deserialize_mouse_protocol_mode")]
    mouse_protocol_mode: vt100_ctt::MouseProtocolMode,
    #[serde(default, deserialize_with = "deserialize_mouse_protocol_encoding")]
    mouse_protocol_encoding: vt100_ctt::MouseProtocolEncoding,
}

impl FixtureScreen {
    fn load<R: std::io::Read>(r: R) -> Self {
        serde_json::from_reader(r).unwrap()
    }
}

fn hex_char(c: u8) -> Result<u8, String> {
    match c {
        b'0'..=b'9' => Ok(c - b'0'),
        b'a'..=b'f' => Ok(c - b'a' + 10),
        b'A'..=b'F' => Ok(c - b'A' + 10),
        _ => Err("invalid hex char".to_string()),
    }
}

fn hex(upper: u8, lower: u8) -> Result<u8, String> {
    Ok(hex_char(upper)? * 16 + hex_char(lower)?)
}

fn deserialize_color<'a, D>(
    deserializer: D,
) -> std::result::Result<vt100_ctt::Color, D::Error>
where
    D: serde::de::Deserializer<'a>,
{
    let val = <Option<String>>::deserialize(deserializer)?;
    match val {
        None => Ok(vt100_ctt::Color::Default),
        Some(x) if x.starts_with('#') => {
            let x = x.as_bytes();
            if x.len() != 7 {
                return Err(serde::de::Error::custom("invalid rgb color"));
            }
            let r = hex(x[1], x[2]).map_err(serde::de::Error::custom)?;
            let g = hex(x[3], x[4]).map_err(serde::de::Error::custom)?;
            let b = hex(x[5], x[6]).map_err(serde::de::Error::custom)?;
            Ok(vt100_ctt::Color::Rgb(r, g, b))
        }
        Some(x) => Ok(vt100_ctt::Color::Idx(
            x.parse().map_err(serde::de::Error::custom)?,
        )),
    }
}

fn deserialize_mouse_protocol_mode<'a, D>(
    deserializer: D,
) -> std::result::Result<vt100_ctt::MouseProtocolMode, D::Error>
where
    D: serde::de::Deserializer<'a>,
{
    let name = <String>::deserialize(deserializer)?;
    match name.as_ref() {
        "none" => Ok(vt100_ctt::MouseProtocolMode::None),
        "press" => Ok(vt100_ctt::MouseProtocolMode::Press),
        "press_release" => Ok(vt100_ctt::MouseProtocolMode::PressRelease),
        "button_motion" => Ok(vt100_ctt::MouseProtocolMode::ButtonMotion),
        "any_motion" => Ok(vt100_ctt::MouseProtocolMode::AnyMotion),
        _ => Err(serde::de::Error::custom(format!(
            "unknown mouse protocol mode {name}"
        ))),
    }
}

fn deserialize_mouse_protocol_encoding<'a, D>(
    deserializer: D,
) -> std::result::Result<vt100_ctt::MouseProtocolEncoding, D::Error>
where
    D: serde::de::Deserializer<'a>,
{
    let name = <String>::deserialize(deserializer)?;
    match name.as_ref() {
        "default" => Ok(vt100_ctt::MouseProtocolEncoding::Default),
        "utf8" => Ok(vt100_ctt::MouseProtocolEncoding::Utf8),
        "sgr" => Ok(vt100_ctt::MouseProtocolEncoding::Sgr),
        _ => Err(serde::de::Error::custom(format!(
            "unknown mouse protocol encoding {name}"
        ))),
    }
}

fn load_input(name: &str, i: usize) -> Option<Vec<u8>> {
    let mut file = std::fs::File::open(format!(
        "tests/data/fixtures/{name}/{i}.typescript"
    ))
    .ok()?;
    let mut input = vec![];
    file.read_to_end(&mut input).unwrap();
    Some(input)
}

fn load_screen(name: &str, i: usize) -> Option<FixtureScreen> {
    let mut file =
        std::fs::File::open(format!("tests/data/fixtures/{name}/{i}.json"))
            .ok()?;
    Some(FixtureScreen::load(&mut file))
}

fn assert_produces(input: &[u8], expected: &FixtureScreen) {
    let mut parser = vt100_ctt::Parser::default();
    parser.process(input);

    assert_eq!(parser.screen().contents(), expected.contents);
    assert_eq!(parser.screen().cursor_position(), expected.cursor_position);
    assert_eq!(
        parser.screen().application_keypad(),
        expected.application_keypad
    );
    assert_eq!(
        parser.screen().application_cursor(),
        expected.application_cursor
    );
    assert_eq!(parser.screen().hide_cursor(), expected.hide_cursor);
    assert_eq!(parser.screen().bracketed_paste(), expected.bracketed_paste);
    assert_eq!(
        parser.screen().mouse_protocol_mode(),
        expected.mouse_protocol_mode
    );
    assert_eq!(
        parser.screen().mouse_protocol_encoding(),
        expected.mouse_protocol_encoding
    );

    let (rows, cols) = parser.screen().size();
    for row in 0..rows {
        for col in 0..cols {
            let expected_cell = expected
                .cells
                .get(&format!("{row},{col}"))
                .cloned()
                .unwrap_or_default();
            let got_cell = parser.screen().cell(row, col).unwrap();
            assert_eq!(got_cell.contents(), expected_cell.contents);
            assert_eq!(got_cell.is_wide(), expected_cell.is_wide);
            assert_eq!(
                got_cell.is_wide_continuation(),
                expected_cell.is_wide_continuation
            );
            assert_eq!(got_cell.fgcolor(), expected_cell.fgcolor);
            assert_eq!(got_cell.bgcolor(), expected_cell.bgcolor);
            assert_eq!(got_cell.bold(), expected_cell.bold);
            assert_eq!(got_cell.dim(), expected_cell.dim);
            assert_eq!(got_cell.italic(), expected_cell.italic);
            assert_eq!(got_cell.underline(), expected_cell.underline);
            assert_eq!(got_cell.inverse(), expected_cell.inverse);
        }
    }
}

/// Replays one named fixture chunk by chunk. Each chunk must leave the screen
/// the recorded JSON describes, and every prefix must survive the checkpoint
/// round trip.
pub fn fixture(name: &str) {
    let mut i = 1;
    let mut prev_input = vec![];
    while let Some(input) = load_input(name, i) {
        prev_input.extend(input);
        super::assert_screen_roundtrips(&prev_input);

        let expected = load_screen(name, i).unwrap();
        assert_produces(&prev_input, &expected);

        i += 1;
    }
    assert!(i > 1, "couldn't find fixtures to test");
}
