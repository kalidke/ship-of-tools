use super::*;

const FLOOR: Duration = Duration::from_millis(200);
const CAP: Duration = Duration::from_secs(30);

#[test]
fn a_short_connection_keeps_the_doubling() {
    let mut redial = Redial::new(FLOOR, CAP);
    let waits: Vec<_> = (0..10).map(|_| redial.after(STABLE - Duration::from_secs(1)).as_millis()).collect();
    assert_eq!(waits, [200, 400, 800, 1_600, 3_200, 6_400, 12_800, 25_600, 30_000, 30_000]);
}

#[test]
fn a_connection_that_lasted_stable_starts_over() {
    let mut redial = Redial::new(FLOOR, CAP);
    for _ in 0..5 {
        redial.after(Duration::ZERO);
    }
    assert_eq!(redial.after(STABLE), FLOOR);
    assert_eq!(redial.after(Duration::ZERO), FLOOR * 2);
}

#[test]
fn a_reset_starts_over() {
    let mut redial = Redial::new(FLOOR, CAP);
    for _ in 0..5 {
        redial.after(Duration::ZERO);
    }
    redial.reset();
    assert_eq!(redial.after(Duration::ZERO), FLOOR);
}
