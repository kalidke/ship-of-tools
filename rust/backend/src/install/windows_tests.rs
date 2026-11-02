use super::*;

#[test]
#[ignore = "requires elevated Windows PowerShell 5.1 to assign BUILTIN\\Administrators as a fixture owner"]
fn an_elevated_stage_is_owned_by_the_running_account_before_it_stays_armed() {
    let dir = tempfile::tempdir().unwrap();
    let prefix = dir.path().join("prefix");
    let checkout = prefix.join("repo/versions/v9.9.9");
    let config = dir.path().join("config");
    let outside = dir.path().join("outside");
    let junction = prefix.join("junction");
    let nested = prefix.join("updates/stage/nested");
    std::fs::create_dir_all(&checkout).unwrap();
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::create_dir_all(&config).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    let file = nested.join("sot.exe");
    std::fs::write(&file, "staged").unwrap();
    let settings = config.join("settings.toml");
    std::fs::write(&settings, "").unwrap();
    let pointer = prefix.join("updates/pending-windows-x86_64.json");
    std::fs::write(&pointer, "{}").unwrap();
    let mut c = powershell(&["-Command", r"
$ErrorActionPreference = 'Stop'
$admin = New-Object System.Security.Principal.SecurityIdentifier('S-1-5-32-544')
foreach ($path in @($env:SOT_CHECKOUT, $env:SOT_FILE, $env:SOT_SETTINGS, $env:SOT_OUTSIDE)) {
    $acl = Get-Acl -LiteralPath $path
    $acl.SetOwner($admin)
    Set-Acl -LiteralPath $path -AclObject $acl
}
New-Item -ItemType Junction -Path $env:SOT_JUNCTION -Target $env:SOT_OUTSIDE | Out-Null
"]);
    c.env("SOT_CHECKOUT", &checkout).env("SOT_FILE", &file).env("SOT_SETTINGS", &settings)
        .env("SOT_OUTSIDE", &outside).env("SOT_JUNCTION", &junction);
    let seeded = cmd::output_within(&mut c, Duration::from_secs(30));
    assert!(seeded.ok(), "{}", seeded.last_said());
    owned_stage(&prefix, &checkout, &config, &pointer).unwrap();
    assert!(pointer.is_file());
    let mut c = powershell(&["-Command", r"
$ErrorActionPreference = 'Stop'
$me = [System.Security.Principal.WindowsIdentity]::GetCurrent().User
foreach ($path in @($env:SOT_CHECKOUT, $env:SOT_FILE, $env:SOT_SETTINGS)) {
    $owner = (Get-Acl -LiteralPath $path).GetOwner([System.Security.Principal.SecurityIdentifier])
    if ($owner -ne $me) { throw ('owner still ' + $owner.Value) }
}
$outside = (Get-Acl -LiteralPath $env:SOT_OUTSIDE).GetOwner([System.Security.Principal.SecurityIdentifier])
if ($outside.Value -ne 'S-1-5-32-544') { throw 'ownership correction followed a junction' }
"]);
    c.env("SOT_CHECKOUT", &checkout).env("SOT_FILE", &file).env("SOT_SETTINGS", &settings).env("SOT_OUTSIDE", &outside);
    let checked = cmd::output_within(&mut c, Duration::from_secs(30));
    assert!(checked.ok(), "{}", checked.last_said());
    let again = owned_checkout(&prefix, &checkout, &config);
    assert!(again.is_ok(), "{again:?}");
    let stop = owned_stage(&prefix, &prefix.join("absent"), &config, &pointer).unwrap_err();
    let (line, code) = verdict::stop_line(&stop, "");
    assert!(code == 1 && line.starts_with("FAIL: ownership:") && line.contains("owner is unreadable") && line.contains("again unelevated"), "{line}");
    assert!(!pointer.exists() && !line.contains("stays armed"), "{line}");
    assert_eq!(std::fs::read_to_string(file).unwrap(), "staged");
}

/// A listing that gave no answer presumes a daemon of the install runs, with its reason, so it is asked about and never
/// read as "none runs"; after a stage it stops nothing and the update stays armed, the run ending ONE STEP LEFT naming
/// why.
#[test]
fn a_listing_with_no_answer_is_presumed_running_and_leaves_the_update_armed() {
    let why = "`powershell.exe` gave no answer: powershell.exe did not finish within 60000 ms";
    assert_eq!(runs_in(Err(why.into()), Path::new(r"C:\p")), (true, Some(why.to_string())));
    let step = armed_listing(Err(why.into()), "v0.7.0").unwrap_err();
    let mut found = Found::default();
    found.step(step);
    let (line, code) = verdict::line(&found, "");
    assert!(code == 0 && line.starts_with("ONE STEP LEFT: ") && line.contains(why) && line.contains("stopped nothing"), "exit {code}: {line}");
}
