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
