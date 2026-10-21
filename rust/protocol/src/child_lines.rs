//! A child's output read line by line.

/// A child's output, line by line: the bytes up to each `\n`, less a `\r` before it, decoded lossily (an invalid
/// sequence becomes U+FFFD). A child may write any byte (a Julia kernel's JSON3 writes a string's bytes as they are,
/// a parse error quotes its source, an ssh banner is the far host's text), and a strict decode fails on the first,
/// which ends the reader: a child's generation, or a drain whose closed pipe then fails the child's next write.
/// rust/clippy.toml's child-output group refuses the strict line readers. `next_line` is cancel safe, as tokio's
/// `Lines::next_line` is: a cut line waits in the reader. `blocking_child_lines` reads the same lines on a thread.
pub struct ChildLines<R>(tokio::io::Split<tokio::io::BufReader<R>>);

impl<R: tokio::io::AsyncRead + Unpin> ChildLines<R> {
    pub fn new(stream: R) -> Self {
        Self(tokio::io::AsyncBufReadExt::split(tokio::io::BufReader::new(stream), b'\n'))
    }

    /// The next line, or `None` at the end of the stream.
    pub async fn next_line(&mut self) -> std::io::Result<Option<String>> {
        Ok(self.0.next_segment().await?.map(line_text))
    }
}

/// `ChildLines` for a child read on a thread of its own.
pub fn blocking_child_lines<R: std::io::Read>(stream: R) -> impl Iterator<Item = std::io::Result<String>> {
    std::io::BufRead::split(std::io::BufReader::new(stream), b'\n').map(|line| line.map(line_text))
}

/// One line's bytes as text: a final `\r` dropped, an invalid sequence U+FFFD.
fn line_text(mut line: Vec<u8>) -> String {
    if line.last() == Some(&b'\r') {
        line.pop();
    }
    String::from_utf8_lossy(&line).into_owned()
}

#[cfg(test)]
#[path = "child_lines_tests.rs"]
mod tests;
