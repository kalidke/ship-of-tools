# vt100

This crate parses a terminal byte stream and provides an in-memory
representation of the rendered contents.

## Overview

This is essentially the terminal parser component of a graphical terminal
emulator pulled out into a separate crate. Although you can use this crate
to build a graphical terminal emulator, it also contains functionality
necessary for implementing terminal applications that want to run other
terminal applications - programs like `screen` or `tmux` for example.

## Synopsis

```rust
let mut parser = vt100_ctt::Parser::new(24, 80, 0);

parser.process(b"this text is \x1b[31mRED\x1b[m");
assert_eq!(
    parser.screen().cell(0, 13).unwrap().fgcolor(),
    vt100_ctt::Color::Idx(1),
);

// Hand the parsed state to another process exactly, including the grid
// that is not currently on screen.
let bytes = parser.screen().checkpoint().unwrap();
let mut elsewhere = vt100_ctt::Parser::default();
elsewhere.restore_screen(&bytes).unwrap();
```
