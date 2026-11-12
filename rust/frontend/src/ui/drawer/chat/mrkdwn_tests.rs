use super::mrkdwn::*;
use sot_protocol::chat::text_segments;

/// The shown pieces of Slack's text as `(text, style)`; a token not read is marked with `<>` around its text in the table, a
/// link with `[]` and a mention with `{}`.
fn shown(text: &str) -> Vec<(String, u8)> {
    styled(text_segments(text)).into_iter().map(|p| (match p.role {
        Role::Text => p.text,
        Role::Markup => format!("<{}>", p.text),
        Role::Link => format!("[{}]", p.text),
        Role::Mention => format!("{{{}}}", p.text),
    }, p.style)).collect()
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
    assert_eq!(shown("*hi <@U1>*"), [s("hi ", BOLD), s("{@U1}", BOLD)]);
    assert_eq!(shown("*hi <!foo>*"), [s("hi ", BOLD), s("<<!foo>>", BOLD)]);
    assert_eq!(shown("`<@U1> x`"), [s("<@U1> x", CODE)]);
    assert_eq!(shown("<@U1>*b*"), [s("{@U1}", 0), s("*b*", 0)]);
    // A token is its shown text, whatever the styles around it: a link, a mention, and a date's fallback joining its words.
    assert_eq!(shown("_<https://x.test|a *b*> <!here>_"), [s("[a *b*]", ITALIC), s(" ", ITALIC), s("{@here}", ITALIC)]);
    assert_eq!(shown("a <!date^1^x|b> c"), plain("a b c"));
    // A marker followed by the same marker never opens, so in `**x**` the inner pair styles `x` between literal outer marks;
    // two pairs that meet merge into one run, and a run of backticks closing one span and opening the next is one box.
    for (t, m) in [("**x**", '*'), ("~~x~~", '~'), ("__init__", '_')] {
        let bit = match m { '*' => BOLD, '~' => STRIKE, _ => ITALIC };
        let inner = t.trim_matches(m);
        assert_eq!(shown(t), [s(&m.to_string(), 0), s(inner, bit), s(&m.to_string(), 0)], "{t}");
    }
    assert_eq!(shown("*a**b*"), [s("ab", BOLD)]);
    assert_eq!(shown("`a``b`"), [s("ab", CODE)]);
    // A failed closer scan is remembered per marker and per line: it never suppresses another marker or the next line.
    assert_eq!(shown("*a _b_"), [s("*a ", 0), s("b", ITALIC)]);
    assert_eq!(shown("*a\n*b*"), [s("*a\n", 0), s("b", BOLD)]);
    // Only ASCII punctuation is a boundary.
    for t in ["*a*\u{201d}", "\u{201c}*a*", "*a*\u{2014}x", "x*\u{65e5}\u{672c}*y"] {
        assert_eq!(shown(t), plain(t), "{t}");
    }
    // A fence with no closing one is literal.
    assert_eq!(shown("```code"), plain("```code"));
}

/// The scans a parse makes, counted by `close` and `fence`: a line of openers that never close costs one scan, not one a marker,
/// and a line of fences, of quote markers or of tokens costs a pass.
#[test]
fn the_parse_is_linear_on_a_line_of_openers() {
    for unit in [
        "*a ", "_a ", "~a ", "`a ", "*a_", "*a* ", "*<@U1> ", "<!here>*a ", "`<https://x.test|a> ", "<#C1|d>_a_ ",
        "``` ", "```a ", "```\n> ", "`` ", "````", "> ", "> *a ", ">>> ", ">\n", "\n> `a ", "```*a ``` *a ", "a``` ```b *c ",
    ] {
        let text = unit.repeat(8192 / unit.len());
        scanned_reset();
        let pieces = shown(&text);
        let steps = scanned();
        assert!(!pieces.is_empty());
        assert!(steps <= 4 * text.len(), "{unit:?}: {steps} atoms stepped over for {} of text", text.len());
    }
}

/// Slack's blocks: a fence opens a code block and the next closes it, across lines or not, one line end dropped after the opening
/// and before the closing, nothing inside read; a quote is a line of `>` (`&gt;`) and an optional space, or `>>>` and the rest;
/// an empty line in either is one space that is flagged, and an unclosed fence is literal.
#[test]
fn code_blocks_and_quotes_are_found_as_slack_writes_them() {
    let s = |t: &str, st: u8| (t.to_string(), st);
    let (b, q, f) = (BLOCK, QUOTE, FILL);
    // Code blocks: inline, across lines with the two line ends dropped, with text round them, one after another, empty.
    assert_eq!(shown("```code```"), [s("code", b)]);
    assert_eq!(shown("```\nx\ny\n```"), [s("x\ny", b)]);
    assert_eq!(shown("a ```b``` c"), [s("a \n", 0), s("b", b), s("\n c", 0)]);
    assert_eq!(shown("```x```\nrest"), [s("x", b), s("\nrest", 0)]);
    assert_eq!(shown("```x```\n"), [s("x", b)]);
    assert_eq!(shown("```a```\n```b```"), [s("a", b), s("\n", 0), s("b", b | ALT)]);
    assert_eq!(shown("``````"), [s(" ", b | f)]);
    assert_eq!(shown("```\na\n\nb\n```"), [s("a\n", b), s(" ", b | f), s("\nb", b)]);
    // Nothing inside is read: no style, no token, no quote; a token shows as its text.
    assert_eq!(shown("```*a* <@U1> `x` _y_\n```"), [s("*a* <@U1> `x` _y_", b)]);
    assert_eq!(shown("```\n> a\n```"), [s("> a", b)]);
    // An unclosed fence stays as typed.
    for t in ["```a", "x ``` y", "``a``"] {
        assert_eq!(shown(t), plain(t), "{t}");
    }
    // Quotes: one line, as `&gt;` decodes, with and without the space, one a line, an empty line, and styles and tokens inside.
    assert_eq!(shown("> a"), [s("a", q)]);
    assert_eq!(shown("&gt;a"), [s("a", q)]);
    assert_eq!(shown("> a\n> b\nc"), [s("a", q), s("\n", 0), s("b", q), s("\nc", 0)]);
    assert_eq!(shown(">\n> b"), [s(" ", q | f), s("\n", 0), s("b", q)]);
    assert_eq!(shown("> *b* <@U1> `c`"), [s("b", q | BOLD), s(" ", q), s("{@U1}", q), s(" ", q), s("c", q | CODE)]);
    assert_eq!(shown("a > b\n x"), plain("a > b\n x"));
    // `>>>` quotes the rest, empty lines included, and a block still ends it as a block.
    assert_eq!(shown(">>> a\nb\n\nc"), [s("a", q), s("\n", 0), s("b", q), s("\n", 0), s(" ", q | f), s("\n", 0), s("c", q)]);
    assert_eq!(shown("x\n>>> a\n> b"), [s("x\n", 0), s("a", q), s("\n", 0), s("> b", q)]);
    assert_eq!(shown("```x```> y"), [s("x", b), s("\n", 0), s("y", q)]);
}
