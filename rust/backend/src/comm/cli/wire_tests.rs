use super::*;

#[test]
fn a_refused_hello_is_the_daemons_words_and_silence_says_nothing_answered() {
    let ep = "unix:/run/x.sock";
    assert_eq!(said(ep, "unix:/run/x.sock: hello refused: a second OS account"), "hello refused: a second OS account");
    assert_eq!(said(ep, "unix:/run/x.sock: no reply to comm.file within 5s"), "the daemon did not answer at unix:/run/x.sock: no reply to comm.file within 5s");
    assert_eq!(said(ep, "connect: refused"), "the daemon did not answer at unix:/run/x.sock: connect: refused");
}
