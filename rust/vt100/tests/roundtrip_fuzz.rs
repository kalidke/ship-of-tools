//! Upstream's `quickcheck.rs`, rewritten around a deterministic generator.
//!
//! Upstream fuzzed one property — that a parsed screen could be re-emitted as
//! escape sequences and re-parsed without loss — over both random bytes and
//! structured terminal-ish input. The property is gone with the stack it
//! tested; its replacement is the checkpoint round trip. The input generator
//! is the part worth keeping, and it is reproduced here.
//!
//! Two changes from upstream. The `quickcheck` dependency is gone: shrinking
//! bought a smaller counterexample, and a fixed-seed generator buys something
//! this project needs more — a CI failure that reproduces exactly, with the
//! offending bytes printed. And the property is weaker in one honest respect:
//! it compares serialized state rather than observed behavior, so a field the
//! encoder forgets is forgotten symmetrically and passes. That gap is closed
//! by `checkpoint.rs`, whose tests observe the restored state through
//! behavior; what this file adds is breadth of *input*, which is exactly what
//! is at risk when a later step changes the parser.

/// xorshift64*, so a failure reproduces from the printed seed alone.
struct Rng(u64);

impl Rng {
    fn next_u32(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        (x.wrapping_mul(0x2545_f491_4f6c_dd1d) >> 32) as u32
    }

    fn below(&mut self, n: u32) -> u32 {
        self.next_u32() % n
    }

    fn in_range(&mut self, range: std::ops::Range<u32>) -> u32 {
        range.start + self.below(range.end - range.start)
    }
}

/// One fragment of plausible terminal traffic, with upstream's weighting:
/// mostly text, then control characters, escapes, and CSI sequences.
fn fragment(rng: &mut Rng, out: &mut Vec<u8>) {
    match rng.below(256) {
        0..=231 => {
            let mut u = rng.in_range(32..(2u32.pow(20) - 2048));
            // surrogates aren't valid codepoints on their own
            if u >= 0xD800 {
                u += 2048;
            }
            let c = char::try_from(u).unwrap_or_else(|e| {
                panic!("failed to create char from {u}: {e}")
            });
            let mut b = [0; 4];
            out.extend_from_slice(c.encode_utf8(&mut b).as_bytes());
        }
        232..=239 => out.push(rng.in_range(7..14) as u8),
        240..=247 => {
            out.push(0x1b);
            out.push(rng.in_range(u32::from(b'0')..u32::from(b'~')) as u8);
        }
        _ => {
            out.push(0x1b);
            out.push(b'[');
            out.push(rng.in_range(u32::from(b'@')..u32::from(b'~')) as u8);
        }
    }
}

fn structured_input(rng: &mut Rng) -> Vec<u8> {
    let fragments = rng.below(100);
    let mut input = vec![];
    for _ in 0..fragments {
        fragment(rng, &mut input);
    }
    input
}

fn random_input(rng: &mut Rng) -> Vec<u8> {
    let len = rng.below(400);
    (0..len).map(|_| rng.below(256) as u8).collect()
}

/// Any screen the parser can produce must checkpoint, restore into an
/// identical screen, and re-serialize to the same bytes.
fn roundtrips(rows: u16, cols: u16, input: &[u8]) -> bool {
    let mut original = vt100_ctt::Parser::new(rows, cols, 0);
    original.process(input);

    let Ok(bytes) = original.screen().checkpoint() else {
        return false;
    };
    let mut restored = vt100_ctt::Parser::new(1, 1, 0);
    if restored.restore_screen(&bytes).is_err() {
        return false;
    }
    if original.screen().contents() != restored.screen().contents() {
        return false;
    }
    restored.screen().checkpoint().map_or(false, |again| again == bytes)
}

/// Geometries small enough to hit the wrap and wide-glyph edges, plus one
/// ordinary size. The narrow ones are where the parser's two inherited panics
/// lived, one row and one column further down — see `geometry.rs`.
const SIZES: &[(u16, u16)] = &[(2, 2), (3, 5), (4, 7), (24, 80)];

fn run(cases: usize, seed: u64, generate: fn(&mut Rng) -> Vec<u8>) {
    let mut rng = Rng(seed);
    for case in 0..cases {
        let input = generate(&mut rng);
        let (rows, cols) = SIZES[case % SIZES.len()];
        // A parser panic would otherwise abort with no way back to the input
        // that caused it, which would defeat the point of a fixed seed.
        let ok = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
            || roundtrips(rows, cols, &input),
        ));
        let verdict = match ok {
            Ok(true) => continue,
            Ok(false) => "did not round trip",
            Err(_) => "PANICKED",
        };
        panic!("seed {seed}, case {case} at {cols}x{rows}: {verdict}: {input:?}");
    }
}

#[test]
fn structured_input_roundtrips() {
    run(2_000, 0x5017_0041_0003_0001, structured_input);
}

#[test]
fn random_input_roundtrips() {
    run(2_000, 0x5017_0041_0003_0002, random_input);
}

#[test]
#[ignore = "long soak; run explicitly when the parser changes"]
fn structured_input_roundtrips_long() {
    run(1_000_000, 0x5017_0041_0003_0003, structured_input);
}

#[test]
#[ignore = "long soak; run explicitly when the parser changes"]
fn random_input_roundtrips_long() {
    run(1_000_000, 0x5017_0041_0003_0004, random_input);
}
