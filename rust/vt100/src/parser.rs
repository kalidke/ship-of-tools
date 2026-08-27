/// A parser for terminal output which produces an in-memory representation of
/// the terminal contents.
pub struct Parser<CB: crate::callbacks::Callbacks = ()> {
    parser: crate::vte::Parser,
    screen: crate::perform::WrappedScreen<CB>,
}

impl Parser {
    /// Creates a new terminal parser of the given size and with the given
    /// amount of scrollback.
    #[must_use]
    pub fn new(rows: u16, cols: u16, scrollback_len: usize) -> Self {
        Self {
            parser: crate::vte::Parser::new(),
            screen: crate::perform::WrappedScreen::new(
                rows,
                cols,
                scrollback_len,
            ),
        }
    }
}

impl<CB: crate::callbacks::Callbacks> Parser<CB> {
    /// Creates a new terminal parser of the given size and with the given
    /// amount of scrollback. Terminal events will be reported via method
    /// calls on the provided [`Callbacks`](crate::callbacks::Callbacks)
    /// implementation.
    pub fn new_with_callbacks(
        rows: u16,
        cols: u16,
        scrollback_len: usize,
        callbacks: CB,
    ) -> Self {
        Self {
            parser: crate::vte::Parser::new(),
            screen: crate::perform::WrappedScreen::new_with_callbacks(
                rows,
                cols,
                scrollback_len,
                callbacks,
            ),
        }
    }

    /// Processes the contents of the given byte string, and updates the
    /// in-memory terminal state.
    pub fn process(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.parser.advance(&mut self.screen, *byte);
        }
    }

    /// True iff the escape-sequence state machine is at its ground state and
    /// no partial UTF-8 codepoint is buffered — the only positions where a
    /// checkpoint's post-cut stream may legally begin (see
    /// [`Screen::checkpoint`](crate::Screen::checkpoint)).
    #[must_use]
    pub fn is_ground(&self) -> bool {
        self.parser.is_ground()
    }

    /// Returns a reference to a [`Screen`](crate::Screen) object containing
    /// the terminal state.
    #[must_use]
    pub fn screen(&self) -> &crate::Screen {
        &self.screen.screen
    }

    /// Returns a mutable reference to a [`Screen`](crate::Screen) object
    /// containing the terminal state.
    #[must_use]
    pub fn screen_mut(&mut self) -> &mut crate::Screen {
        &mut self.screen.screen
    }

    /// Returns a reference to the [`Callbacks`](crate::callbacks::Callbacks)
    /// state object passed into the constructor.
    pub fn callbacks(&self) -> &CB {
        &self.screen.callbacks
    }

    /// Returns a mutable reference to the
    /// [`Callbacks`](crate::callbacks::Callbacks) state object passed into
    /// the constructor.
    pub fn callbacks_mut(&mut self) -> &mut CB {
        &mut self.screen.callbacks
    }
}

impl Default for Parser {
    /// Returns a parser with dimensions 80x24 and no scrollback.
    fn default() -> Self {
        Self::new(24, 80, 0)
    }
}

impl std::io::Write for Parser {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.process(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<CB: crate::callbacks::Callbacks> Parser<CB> {
    /// Replaces the terminal state with one restored from
    /// [`Screen::checkpoint`](crate::Screen::checkpoint) bytes.
    ///
    /// The escape-sequence state machine is reset to ground at the same time.
    /// A checkpoint describes a screen, never a half-consumed escape
    /// sequence — the sequence parser's state is private to `vte` and cannot
    /// be captured — so the producer of a checkpoint is responsible for
    /// cutting the byte stream that follows it at a ground-state boundary.
    /// Resetting here is what makes that contract hold on this side.
    ///
    /// The restored screen keeps this parser's own scrollback capacity; see
    /// [`Screen::restore`](crate::Screen::restore).
    ///
    /// # Errors
    ///
    /// Returns [`CheckpointError`](crate::CheckpointError) if the payload
    /// cannot be decoded. The parser is left untouched in that case.
    pub fn restore_screen(
        &mut self,
        bytes: &[u8],
    ) -> Result<(), crate::CheckpointError> {
        let screen = crate::Screen::restore(
            bytes,
            self.screen.screen.scrollback_len(),
        )?;
        self.screen.screen = screen;
        self.parser = crate::vte::Parser::new();
        Ok(())
    }
}
