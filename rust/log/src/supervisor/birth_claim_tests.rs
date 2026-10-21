use super::*;

#[test]
fn a_takeover_record_round_trips_and_a_torn_or_foreign_one_is_refused() {
    let record = Takeover {
        pid: 4242,
        created: 987_654_321,
    };
    let mut wire: &[u8] = &record.encode();
    assert_eq!(read_takeover(&mut wire).unwrap(), Some(record));
    assert_eq!(
        read_takeover(&mut &[][..]).unwrap(),
        None,
        "EOF with nothing written is no record"
    );
    let full = record.encode();
    assert!(
        read_takeover(&mut &full[..7]).is_err(),
        "a torn record is an error"
    );
    let mut foreign = full;
    foreign[0] = b'X';
    assert!(
        read_takeover(&mut &foreign[..]).is_err(),
        "a record without the magic is refused"
    );
    assert_eq!(Takeover::decode(&full[..15]), None);
}
