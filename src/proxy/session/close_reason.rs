use std::io;
use std::io::ErrorKind;

pub(super) fn is_expected_close_error(e: &io::Error) -> bool {
    if matches!(
        e.kind(),
        ErrorKind::BrokenPipe
            | ErrorKind::ConnectionReset
            | ErrorKind::ConnectionAborted
            | ErrorKind::UnexpectedEof
            | ErrorKind::NotConnected
    ) {
        return true;
    }

    matches!(
        e.to_string().as_str(),
        "Session closed" | "session read half closed" | "session write half closed"
    )
}
