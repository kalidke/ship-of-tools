use super::*;

#[test]
fn the_question_line_parses_like_the_queue() {
    let cases = [
        ("Land it? Options: (a) merge; (b) send back. Recommend (a). Page: /project/decision.html",
         "Land it?", vec![("a", "merge"), ("b", "send back")], "a", "/project/decision.html"),
        ("Just a question?", "Just a question?", vec![], "", ""),
        ("Context? Page: /project/context.html", "Context?", vec![], "", "/project/context.html"),
        ("Pick? Options: (a) first; (b) second. Recommended: b", "Pick?", vec![("a", "first"), ("b", "second")], "b", ""),
        ("Pick? Page: /project/context.html. Options: (a) α; (b) β. Recommend: (b).",
         "Pick?", vec![("a", "α"), ("b", "β")], "b", "/project/context.html"),
        ("Recommend (a) without options", "Recommend (a) without options", vec![], "", ""),
        ("Page: is not a path?", "not a path?", vec![], "", "is"),
        ("Pick? Options: (a) one. (b) two;", "Pick?", vec![("a", "one"), ("b", "two")], "", ""),
        ("Unchanged Page:/project/x", "Unchanged Page:/project/x", vec![], "", ""),
    ];
    for (text, question, options, recommend, page) in cases {
        let got = parts(text);
        assert_eq!(got.question, question, "{text}");
        assert_eq!(got.options.iter().map(|o| (o.key.as_str(), o.text.as_str())).collect::<Vec<_>>(), options, "{text}");
        assert_eq!(got.recommend, recommend, "{text}");
        assert_eq!(got.page, page, "{text}");
    }
}
