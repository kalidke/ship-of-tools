use super::*;

fn reply_line(run_len: usize) -> Vec<u8> {
    let res = HelloRes { pid: 1, created: 2, phase: "ready".into(), run: "r".repeat(run_len), row: ROW_PROTOCOL };
    let mut line = serde_json::to_vec(&Frame::res(1, OP_HELLO, serde_json::to_value(res).unwrap())).unwrap();
    line.push(b'\n');
    line
}

#[test]
fn a_hello_reply_over_the_cap_is_refused_even_when_its_newline_arrives_with_it() {
    let mut exchange = RowHelloExchange::new("t");
    let line = reply_line(sot_protocol::codec::MAX_ENVELOPE_BYTES + 16);
    assert!(matches!(exchange.feed(&line), ExchangeDecode::Foreign), "an envelope over the cap is never admitted");
}

#[test]
fn a_hello_reply_within_the_cap_is_admitted() {
    let mut exchange = RowHelloExchange::new("t");
    assert!(matches!(exchange.feed(&reply_line(10)), ExchangeDecode::Identity { pid: 1, created: 2 }));
}

#[test]
fn a_hello_request_stays_within_the_writers_cap() {
    let exchange = RowHelloExchange::new("b".repeat(2 * sot_protocol::codec::MAX_ENVELOPE_BYTES));
    assert!(exchange.encode_request().len() <= sot_protocol::codec::MAX_ENVELOPE_BYTES + 1);
}

// The hello exchange keeps what the lanes' exchanges proved: one clean reply is an identity, and anything else (a
// second reply, trailing bytes, a reply after the answer, a wrong kind or a malformed line) is a foreign peer. They run
// on the raw bytes, not through a server.

#[test]
fn a_partial_reply_is_incomplete() {
    let mut exchange = RowHelloExchange::new("t");
    let line = reply_line(10);
    assert!(matches!(exchange.feed(&line[..line.len() - 1]), ExchangeDecode::Incomplete));
    assert!(matches!(exchange.feed(&line[line.len() - 1..]), ExchangeDecode::Identity { pid: 1, created: 2 }), "the rest completes it");
}

#[test]
fn two_complete_replies_in_one_feed_are_foreign() {
    let mut exchange = RowHelloExchange::new("t");
    let mut bytes = reply_line(10);
    bytes.extend(reply_line(10));
    assert!(matches!(exchange.feed(&bytes), ExchangeDecode::Foreign));
}

#[test]
fn a_reply_followed_by_malformed_bytes_is_foreign_not_proven() {
    // The good reply must not mask the trailing corruption in the same feed.
    let mut exchange = RowHelloExchange::new("t");
    let mut bytes = reply_line(10);
    bytes.extend([0xffu8; 16]);
    assert!(matches!(exchange.feed(&bytes), ExchangeDecode::Foreign));
    let mut exchange = RowHelloExchange::new("t");
    let mut bytes = reply_line(10);
    bytes.push(0xAB); // one byte: not even a line
    assert!(matches!(exchange.feed(&bytes), ExchangeDecode::Foreign));
}

#[test]
fn a_malformed_first_line_is_foreign() {
    let mut exchange = RowHelloExchange::new("t");
    assert!(matches!(exchange.feed(b"this is no frame\n"), ExchangeDecode::Foreign));
    let mut exchange = RowHelloExchange::new("t");
    assert!(matches!(exchange.feed(&[0xffu8; 16]), ExchangeDecode::Incomplete), "no newline yet is not an answer");
}

#[test]
fn an_unterminated_reply_over_the_cap_is_foreign() {
    let mut exchange = RowHelloExchange::new("t");
    let bytes = vec![b'x'; sot_protocol::codec::MAX_ENVELOPE_BYTES + 1];
    assert!(matches!(exchange.feed(&bytes), ExchangeDecode::Foreign));
}

#[test]
fn a_reply_of_the_wrong_kind_or_op_is_foreign() {
    let res = HelloRes { pid: 1, created: 2, phase: "ready".into(), run: "r".into(), row: ROW_PROTOCOL };
    let payload = serde_json::to_value(res).unwrap();
    for frame in [
        Frame::req(1, OP_HELLO, payload.clone()),
        Frame::evt(OP_HELLO, payload.clone()),
        Frame::res(1, "row.status", payload.clone()),
    ] {
        let mut line = serde_json::to_vec(&frame).unwrap();
        line.push(b'\n');
        let mut exchange = RowHelloExchange::new("t");
        assert!(matches!(exchange.feed(&line), ExchangeDecode::Foreign), "{frame:?}");
    }
}

#[test]
fn a_second_reply_after_the_answer_is_foreign_on_the_same_instance() {
    let mut exchange = RowHelloExchange::new("t");
    assert!(matches!(exchange.feed(&reply_line(10)), ExchangeDecode::Identity { pid: 1, created: 2 }));
    assert!(matches!(exchange.feed(&reply_line(10)), ExchangeDecode::Foreign), "a later complete reply, same instance");
    assert!(matches!(exchange.feed(&[0xAB]), ExchangeDecode::Foreign), "any byte at all after a conclusive answer");
}

#[test]
fn another_row_number_is_foreign_and_named() {
    let res = HelloRes { pid: 1, created: 2, phase: "ready".into(), run: "r".into(), row: ROW_PROTOCOL + 1 };
    let mut line = serde_json::to_vec(&Frame::res(1, OP_HELLO, serde_json::to_value(res).unwrap())).unwrap();
    line.push(b'\n');
    let mut exchange = RowHelloExchange::new("t");
    assert!(matches!(exchange.feed(&line), ExchangeDecode::Foreign));
    assert_eq!(exchange.mismatch(), Some(ROW_PROTOCOL + 1));
}

#[test]
fn the_request_is_the_hello_with_this_row_number() {
    let exchange = RowHelloExchange::new("t");
    let request = exchange.encode_request();
    assert_eq!(request.last(), Some(&b'\n'));
    let frame: Frame = serde_json::from_slice(&request[..request.len() - 1]).unwrap();
    assert_eq!((frame.kind, frame.op.as_str()), (Kind::Req, OP_HELLO));
    let req: HelloReq = serde_json::from_value(frame.payload).unwrap();
    assert_eq!((req.build.as_str(), req.row), ("t", ROW_PROTOCOL));
}

#[test]
fn a_build_cut_at_the_bound_keeps_whole_characters() {
    // One byte, then two-byte characters: the bound falls inside the character that straddles it.
    let build = format!("a{}", "é".repeat(MAX_BUILD_BYTES));
    assert!(!build.is_char_boundary(MAX_BUILD_BYTES));
    let exchange = RowHelloExchange::new(build);
    let request = exchange.encode_request();
    let frame: Frame = serde_json::from_slice(&request[..request.len() - 1]).expect("the request is valid UTF-8 JSON");
    let req: HelloReq = serde_json::from_value(frame.payload).unwrap();
    assert_eq!(req.build.len(), MAX_BUILD_BYTES - 1, "cut back to the boundary, never past the bound");
    assert!(req.build.chars().skip(1).all(|c| c == 'é'));
}
