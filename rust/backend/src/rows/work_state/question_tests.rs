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
        // REAL (a card on 2026-10-27, stamped by hand): "recommend" in lower case, opened by "I". It set no
        // recommendation and made a third button, a blank (a) between a and b.
        ("Order? Options: (a) you point me to the order list and measure the axis; (b) you point me to the order list and I keep the axis from the CAD until it is measured. I recommend (a).",
         "Order?", vec![("a", "you point me to the order list and measure the axis"),
             ("b", "you point me to the order list and I keep the axis from the CAD until it is measured")], "a", ""),
        ("Pick? Options: (a) first; (b) unlike (a), second. We recommend (b)", "Pick?", vec![("a", "first"), ("b", "unlike (a), second")], "b", ""),
        ("Pick? Options: (a); (b) only this one. RECOMMENDED: (b)", "Pick?", vec![("b", "only this one")], "b", ""),
    ];
    for (text, question, options, recommend, page) in cases {
        let got = parts(text);
        assert_eq!(got.question, question, "{text}");
        assert_eq!(got.options.iter().map(|o| (o.key.as_str(), o.text.as_str())).collect::<Vec<_>>(), options, "{text}");
        assert_eq!(got.recommend, recommend, "{text}");
        assert_eq!(got.page, page, "{text}");
    }
}
