    use super::*;

    #[test]
    fn wide_preview_hands_llm_width_to_preview() {
        let p = LayoutPreset::default_ultrawide().wide_preview();
        assert_eq!(p.columns, vec![Slot::Nav, Slot::Preview]);
        // Nav keeps its width; Preview absorbs the Llm share.
        assert!((p.widths[0] - 0.167).abs() < 1e-3);
        assert!((p.widths[1] - 0.833).abs() < 1e-3);
        let sum: f32 = p.widths.iter().sum();
        assert!((sum - 1.0).abs() < 1e-3, "widths must still sum to ~1.0");
        // Drawer config rides along untouched.
        assert_eq!(p.drawer, Some(Slot::Repl));
    }

    #[test]
    fn wide_preview_is_identity_without_llm_column() {
        let p = LayoutPreset::default_portrait().wide_preview();
        assert_eq!(p.columns, vec![Slot::Nav, Slot::Preview]);
        assert_eq!(p.widths, LayoutPreset::default_portrait().widths);
    }

    #[test]
    fn defaults_are_auto_with_three_columns_each_preset() {
        let s = Settings::default();
        assert_eq!(s.preset, PresetMode::Auto);
        assert_eq!(
            s.ultrawide.columns,
            vec![Slot::Nav, Slot::Preview, Slot::Llm]
        );
        assert_eq!(s.ultrawide.widths.len(), 3);
        let sum: f32 = s.ultrawide.widths.iter().sum();
        assert!((sum - 1.0).abs() < 1e-3, "widths must sum to ~1.0");
    }

    #[test]
    fn auto_resolves_by_aspect() {
        let s = Settings::default();
        // 32:9 / 21:9 → ultrawide.
        assert_eq!(s.resolve_preset(2.4).columns, s.ultrawide.columns);
        // 16:10 → laptop.
        assert_eq!(s.resolve_preset(1.6).columns, s.laptop.columns);
        // 4:3 → portrait.
        assert_eq!(s.resolve_preset(1.33).columns, s.portrait.columns);
    }

    #[test]
    fn explicit_preset_ignores_aspect() {
        let mut s = Settings::default();
        s.preset = PresetMode::Laptop;
        assert_eq!(s.resolve_preset(3.0).columns, s.laptop.columns);
    }

    #[test]
    fn parse_columns_accepts_named_slots() {
        assert_eq!(
            parse_columns("nav,preview,llm"),
            Some(vec![Slot::Nav, Slot::Preview, Slot::Llm])
        );
        assert_eq!(parse_columns("NAV, PREVIEW"), Some(vec![Slot::Nav, Slot::Preview]));
    }

    #[test]
    fn parse_columns_rejects_duplicates_and_unknown() {
        assert!(parse_columns("nav,nav").is_none());
        assert!(parse_columns("nav,unknown,llm").is_none());
        assert!(parse_columns("").is_none());
    }

    #[test]
    fn parse_widths_renormalises_to_one() {
        let ws = parse_widths("0.167,0.333,0.5").unwrap();
        let sum: f32 = ws.iter().sum();
        assert!((sum - 1.0).abs() < 1e-4);
        // Even if the user writes integers, fractions are derived.
        let ws = parse_widths("1,2,3").unwrap();
        let sum: f32 = ws.iter().sum();
        assert!((sum - 1.0).abs() < 1e-4);
        assert!((ws[2] - 0.5).abs() < 1e-3);
    }

    #[test]
    fn parse_widths_rejects_zeros_and_negatives() {
        assert!(parse_widths("0,1").is_none());
        assert!(parse_widths("-0.5,0.5").is_none());
    }

    #[test]
    fn merge_overrides_preset_and_subsection() {
        let mut s = Settings::default();
        s.merge_text(
            "[layout]\npreset = \"laptop\"\n\n[layout.laptop]\ncolumns = \"nav,llm\"\nwidths = \"0.25,0.75\"\ndrawer = \"none\"\n",
        );
        assert_eq!(s.preset, PresetMode::Laptop);
        assert_eq!(s.laptop.columns, vec![Slot::Nav, Slot::Llm]);
        assert!((s.laptop.widths[0] - 0.25).abs() < 1e-3);
        assert!(s.laptop.drawer.is_none());
    }

    #[test]
    fn repl_auto_open_drawer_defaults_true_and_parses() {
        assert!(Settings::default().repl_auto_open_drawer_on_run);
        let mut s = Settings::default();
        s.merge_text("[repl]\nauto_open_drawer_on_run = false\n");
        assert!(!s.repl_auto_open_drawer_on_run);
        s.merge_text("[repl]\nauto_open_drawer_on_run = \"on\"\n");
        assert!(s.repl_auto_open_drawer_on_run);
        // Garbage value leaves the prior setting untouched.
        s.merge_text("[repl]\nauto_open_drawer_on_run = banana\n");
        assert!(s.repl_auto_open_drawer_on_run);
    }

    #[test]
    fn gpu_power_preference_defaults_low_and_parses() {
        // The default is what keeps the dGPU asleep on hybrid laptops —
        // regressing it silently costs battery, so pin it.
        assert_eq!(
            Settings::default().gpu_power_preference,
            GpuPowerPreference::Low
        );
        let mut s = Settings::default();
        s.merge_text("[gpu]\npower_preference = \"high\"\n");
        assert_eq!(s.gpu_power_preference, GpuPowerPreference::High);
        s.merge_text("[gpu]\npower_preference = \"low\"\n");
        assert_eq!(s.gpu_power_preference, GpuPowerPreference::Low);
        // wgpu's own spellings work, and case is ignored.
        s.merge_text("[gpu]\npower_preference = \"HighPerformance\"\n");
        assert_eq!(s.gpu_power_preference, GpuPowerPreference::High);
        s.merge_text("[gpu]\npower_preference = \"low_power\"\n");
        assert_eq!(s.gpu_power_preference, GpuPowerPreference::Low);
        // Garbage leaves the prior value untouched.
        s.merge_text("[gpu]\npower_preference = banana\n");
        assert_eq!(s.gpu_power_preference, GpuPowerPreference::Low);
    }

    #[test]
    fn fullscreen_vsync_pin_defaults_false_and_parses() {
        // Default false — most panels are fixed-refresh; #15's
        // steady-redraw guard stays off until someone opts in on a
        // VRR/OLED panel that pumps brightness in borderless fullscreen.
        assert!(!Settings::default().fullscreen_vsync_pin);
        let mut s = Settings::default();
        s.merge_text("[display]\nfullscreen_vsync_pin = true\n");
        assert!(s.fullscreen_vsync_pin);
        s.merge_text("[display]\nfullscreen_vsync_pin = false\n");
        assert!(!s.fullscreen_vsync_pin);
    }

    #[test]
    fn merge_keeps_defaults_on_garbage() {
        let mut s = Settings::default();
        let before = s.ultrawide.widths.clone();
        s.merge_text("[layout.ultrawide]\nwidths = \"banana,1\"\n");
        assert_eq!(s.ultrawide.widths, before);
    }

    #[test]
    fn merge_clamps_drawer_height() {
        let mut s = Settings::default();
        s.merge_text("[layout.laptop]\ndrawer_height = \"0.99\"\n");
        assert!((s.laptop.drawer_height - DRAWER_MAX).abs() < 1e-3);
        s.merge_text("[layout.laptop]\ndrawer_height = \"0.01\"\n");
        assert!((s.laptop.drawer_height - DRAWER_MIN).abs() < 1e-3);
    }

    #[test]
    fn comments_and_blank_lines_skipped() {
        let mut s = Settings::default();
        s.merge_text(
            "# header comment\n\n[layout]\n# inline note\npreset = \"ultrawide\"  # trailing\n",
        );
        assert_eq!(s.preset, PresetMode::Ultrawide);
    }

    #[test]
    fn terminal_shell_defaults_none_and_parses() {
        // Default is None (auto-resolve).
        assert!(Settings::default().terminal_shell.is_none());

        // Explicit shell is stored.
        let mut s = Settings::default();
        s.merge_text("[terminal]\nshell = \"/usr/bin/fish\"\n");
        assert_eq!(s.terminal_shell.as_deref(), Some("/usr/bin/fish"));

        // Quoted value with double-quotes.
        let mut s = Settings::default();
        s.merge_text("[terminal]\nshell = \"pwsh.exe\"\n");
        assert_eq!(s.terminal_shell.as_deref(), Some("pwsh.exe"));

        // Empty string clears to None.
        let mut s = Settings::default();
        s.merge_text("[terminal]\nshell = \"\"\n");
        assert!(s.terminal_shell.is_none());
    }

    #[test]
    fn downloads_dir_defaults_none_and_parses() {
        // Default is None (resolve via the OS download dir at use time).
        assert!(Settings::default().downloads_dir.is_none());

        // Explicit dir is stored verbatim.
        let mut s = Settings::default();
        s.merge_text("[downloads]\ndir = \"/home/me/dl\"\n");
        assert_eq!(s.downloads_dir, Some(PathBuf::from("/home/me/dl")));

        // Empty string clears to None.
        let mut s = Settings::default();
        s.merge_text("[downloads]\ndir = \"\"\n");
        assert!(s.downloads_dir.is_none());
    }

    #[test]
    fn download_dir_prefers_configured_then_falls_back() {
        // Configured dir wins outright.
        let mut s = Settings::default();
        s.merge_text("[downloads]\ndir = \"/tmp/sot-dl\"\n");
        assert_eq!(s.download_dir(), PathBuf::from("/tmp/sot-dl"));

        // Unconfigured falls back to *some* absolute dir (OS download dir or
        // $HOME/Downloads); on a headless CI box without either it lands on
        // cwd ("."). Just assert it never panics and returns non-empty.
        let s = Settings::default();
        let resolved = s.download_dir();
        assert!(!resolved.as_os_str().is_empty());
    }

    #[test]
    fn load_layered_reads_the_file_sot_settings_names() {
        struct Restore(Option<std::ffi::OsString>);
        impl Drop for Restore {
            fn drop(&mut self) {
                match self.0.take() {
                    Some(v) => std::env::set_var("SOT_SETTINGS", v),
                    None => std::env::remove_var("SOT_SETTINGS"),
                }
            }
        }
        let _restore = Restore(std::env::var_os("SOT_SETTINGS"));
        let dir = std::env::temp_dir().join(format!("sot-settings-pin-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("settings.toml");
        std::fs::write(&file, "[font]\nscale = 2.37\n").unwrap();

        std::env::set_var("SOT_SETTINGS", &file);
        assert_eq!(Settings::load_layered().font_scale, Some(2.37));

        // A variable naming a missing file falls through to the later sources.
        std::env::set_var("SOT_SETTINGS", dir.join("missing.toml"));
        assert_ne!(Settings::load_layered().font_scale, Some(2.37));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Input class -> result of `strip_quotes`; the keybindings reader pins the same table.
    #[test]
    fn strip_quotes_table() {
        let table: &[(&str, &str)] = &[
            ("a", "a"),
            ("\"a\"", "a"),
            ("'a'", "a"),
            (" \"a\" ", "a"),
            (" a ", "a"),
            ("\"a'", "\"a'"),
            ("'a\"", "'a\""),
            ("\"", "\""),
            ("'", "'"),
            ("\"a\"b\"", "a\"b"),
            ("''x''", "'x'"),
            ("", ""),
            ("  ", ""),
            ("\"\"", ""),
            ("''", ""),
        ];
        for (input, want) in table {
            assert_eq!(strip_quotes(input), *want, "strip_quotes({input:?})");
            // The reader's path: `[sessions] new_session_root = <input>`.
            let mut s = Settings::default();
            s.merge_text(&format!("[sessions]\nnew_session_root = {input}\n"));
            let want_root = if want.trim().is_empty() { None } else { Some(want.trim().to_string()) };
            assert_eq!(s.new_session_root, want_root, "merge_text({input:?})");
        }
    }
