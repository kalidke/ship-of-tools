use super::mrkdwn::*;
use sot_protocol::chat::text_segments;

/// The shown pieces of Slack's text as `(text, style)`; a token is marked with `<>` around its text in the table.
fn shown(text: &str) -> Vec<(String, u8)> {
    styled(text_segments(text)).into_iter().map(|p| (if p.markup { format!("<{}>", p.text) } else { p.text }, p.style)).collect()
}

fn plain(t: &str) -> Vec<(String, u8)> {
    vec![(t.to_string(), 0)]
}

/// Slack's boundary rules: a marker opens at the start or after whitespace or ASCII punctuation (never after an
/// alphanumeric) and before a non-space, and closes at the nearest same marker on its line that follows a non-space and
/// precedes the end, whitespace or punctuation; anything else is literal.
#[test]
fn slacks_inline_styles_follow_its_boundary_rules() {
    let s = |t: &str, st: u8| (t.to_string(), st);
    assert_eq!(shown("*b*"), [s("b", BOLD)]);
    assert_eq!(shown("_i_"), [s("i", ITALIC)]);
    assert_eq!(shown("~s~"), [s("s", STRIKE)]);
    assert_eq!(shown("`c`"), [s("c", CODE)]);
    assert_eq!(shown("a *b* c, (_d_)."), [s("a ", 0), s("b", BOLD), s(" c, (", 0), s("d", ITALIC), s(").", 0)]);
    // After an alphanumeric, or before a space, a marker is literal; so is one whose closer is before a letter.
    for t in ["2*3*4", "* x *", "snake_case_name", "*a*b", "x*y* z", "~ no~"] {
        assert_eq!(shown(t), plain(t), "{t}");
    }
    // Unclosed, and a closer on another line.
    for t in ["*b", "say _it", "*a\nb*", "`a\nb`", "**", "~~"] {
        assert_eq!(shown(t), plain(t), "{t:?}");
    }
    // Nested styles keep every bit; a marker inside code is shown; code takes no inner style.
    assert_eq!(shown("*_x_*"), [s("x", BOLD | ITALIC)]);
    assert_eq!(shown("*a ~b~ c*"), [s("a ", BOLD), s("b", BOLD | STRIKE), s(" c", BOLD)]);
    assert_eq!(shown("`*a* _b_`"), [s("*a* _b_", CODE)]);
    // A token is one atom: it styles with its run, and inside code it is its decoded text.
    assert_eq!(shown("*hi <@U1>*"), [s("hi ", BOLD), s("<<@U1>>", BOLD)]);
    assert_eq!(shown("`<@U1> x`"), [s("<@U1> x", CODE)]);
    assert_eq!(shown("<@U1>*b*"), [s("<<@U1>>", 0), s("*b*", 0)]);
    // A fenced run stays literal until code blocks are drawn.
    assert_eq!(shown("```code```"), plain("```code```"));
    assert_eq!(shown("```\nx\n```"), plain("```\nx\n```"));
}
