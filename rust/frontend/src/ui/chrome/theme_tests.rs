use super::*;

#[test]
fn contrast_bright_pops_active_coloured_row() {
    // Bright lever (contrast_dim=false): the active coloured row is
    // scaled up past its base tone and bold; a non-active coloured row
    // keeps its full base tone, not bold.
    let base = AgentTone::Working.rgb().unwrap();
    let (act_rgb, act_bold, act_dim) =
        contrast_tone_rgb(AgentTone::Working, false, true, false, 0.0);
    assert!(act_bold && !act_dim);
    let act = act_rgb.unwrap();
    assert!(
        act.0 >= base.0 && act.1 >= base.1 && act.2 >= base.2 && act != base,
        "active bright row must be >= base tone and brighter overall"
    );
    // Non-active keeps base, no bold.
    let (na_rgb, na_bold, _) = contrast_tone_rgb(AgentTone::Working, false, false, false, 0.0);
    assert_eq!(na_rgb, Some(base));
    assert!(!na_bold);
}

#[test]
fn contrast_dim_fades_non_active_coloured_row() {
    // Dim lever (contrast_dim=true): the active coloured row keeps its
    // base tone + bold; a non-active coloured row is scaled down.
    let base = AgentTone::Blocked.rgb().unwrap();
    let (act_rgb, act_bold, _) = contrast_tone_rgb(AgentTone::Blocked, false, true, true, 0.0);
    assert_eq!(act_rgb, Some(base));
    assert!(act_bold);
    let (na_rgb, na_bold, _) = contrast_tone_rgb(AgentTone::Blocked, false, false, true, 0.0);
    let na = na_rgb.unwrap();
    assert!(
        na.0 < base.0 && na.1 <= base.1 && na.2 <= base.2,
        "non-active dim row must be darker than base"
    );
    assert!(!na_bold);
}

#[test]
fn flash_factor_ramps_down_over_window() {
    // Full brightness at the instant of the change.
    assert!((flash_factor(0.0) - 1.0).abs() < 1e-6);
    // Half-faded at half the window.
    assert!((flash_factor(FLASH_SECS / 2.0) - 0.5).abs() < 1e-6);
    // Zero at (and past) the window end — clamped, never negative.
    assert_eq!(flash_factor(FLASH_SECS), 0.0);
    assert_eq!(flash_factor(FLASH_SECS * 2.0), 0.0);
    assert_eq!(flash_factor(-1.0), 1.0);
}
