use super::*;

pub(super) fn expiry_result(error: Option<&sot_log::lane::transport::TransportError>) {
    use sot_log::lane::transport::TransportError::*;
    let public = |text: &str, words: &[&'static str], fallback| words.iter().copied().find(|word| *word == text).unwrap_or(fallback);
    let sanitized = |text: &str| {
        let words = ["lane.connect: handshake timed out", "lane read", "lane write", "failed to fill whole buffer", "Broken pipe", "Connection reset by peer", "operation cancelled", "unexpected reply", "the daemon's lane.connect reply carried no recognizable result"];
        match words.iter().find(|word| text.starts_with(**word)) { Some(word) if text == *word => word.to_string(), Some(word) => format!("{word} <detail>"), None => "<detail>".into() }
    };
    let (variant, io, op, via, code, detail) = match error {
        None => ("Ok", None, "none", "none", "none", "none".into()),
        Some(Io { op, source }) => ("Io", Some(source), public(op, &["lane.connect", "lane read", "lane write"], "<operation>"), "none", "none", sanitized(&source.to_string())),
        Some(RuntimeDir(source)) => ("RuntimeDir", Some(source), "runtime dir", "none", "none", sanitized(&source.to_string())),
        Some(Unreachable(source)) => ("Unreachable", Some(source), "none", "none", "none", sanitized(&source.to_string())),
        Some(Undetermined { via, detail }) => ("Undetermined", None, "none", public(via, &["direct", "bridge"], "<via>"), "none", sanitized(detail)),
        Some(Refused { code, detail }) => ("Refused", None, "none", "none", public(code, &["no_bridge", "hello_refused", "unknown_workspace", "not_capsule", "bad_lane", "unauthenticated", "foreign", "voyage_mismatch", "protocol_mismatch", "bad_request"], "<code>"), sanitized(detail)),
        Some(error) => (match error { InvalidVoyageId(_) => "InvalidVoyageId", InvalidMaxConnections => "InvalidMaxConnections", PathTooLong(_) => "PathTooLong", UnknownConnection(_) => "UnknownConnection", QueueFull(_) => "QueueFull", EmptyPayload => "EmptyPayload", PayloadTooLarge(_) => "PayloadTooLarge", Cancelled => "Cancelled", ConcurrentSubmit => "ConcurrentSubmit", Foreign => "Foreign", LinkDown => "LinkDown", _ => unreachable!() }, None, "none", "none", "none", "<detail>".into()),
    };
    let kind = io.map(|error| format!("{:?}", error.kind())).unwrap_or_else(|| "none".into());
    let os = io.and_then(|error| error.raw_os_error()).map(|number| number.to_string()).unwrap_or_else(|| "none".into());
    println!("T2 expiry first-call: variant={variant} kind={kind} os={os} op={op} via={via} code={code} detail={detail:?}");
}
