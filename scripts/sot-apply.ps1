# sot-apply.ps1 -- Windows apply of an armed pending update (ADR 0030 Phase C3).
#
# The Windows half of scripts/sot-apply.sh. The Rust updater has been
# Windows-capable all along -- `identity.rs` emits the `windows-x86_64` target
# and `archive.rs` knows a Windows release carries `sot.exe`/`sotd.exe` -- but
# nothing on this platform ever CONSUMED what it armed: sot-apply.sh bails on
# any `uname` outside Linux-x86_64/Darwin-arm64, and launch-sot.ps1 used to
# look for a literal `updates\pending\sot.exe` that no version of the stager
# has ever written. This script closes that gap.
#
# Consumes <prefix>\updates\pending-windows-x86_64.json (written by the
# stage -> prepare -> arm pipeline in rust/frontend/src/selfupdate.rs):
# re-verifies the staged tree (archive digest, per-file digests, prepared
# worktree commit + cleanliness), swaps binaries keeping .prev, flips the
# repo\current junction, rewrites install.json, records last-good + a
# just-applied health marker, clears the pointer, prunes old versions.
#
# FAIL-OPEN BY CONTRACT (same as the .sh): every problem exits 0 with a
# "sot-apply:" line on stderr. A verification failure BEFORE mutation leaves
# everything untouched (and clears a pointer whose stage is damaged, so the
# updater re-stages instead of looping). A failure AFTER mutation restores the
# previous binaries and junction before exiting. The launcher must never be
# bricked by a broken update path.
#
# Windows differences from the .sh, all deliberate:
#   - JUNCTIONS, not symlinks, for repo\current / julia\current. A symlink
#     needs Developer Mode or an elevated shell; a directory junction needs
#     neither, and the readers only ever traverse it as a directory.
#   - No `install -m 0755` (no POSIX modes) and no xattr/quarantine strip.
#   - The running-exe lock is a non-issue here: launch-sot.ps1 invokes this
#     BEFORE it spawns the frontend, so <prefix>\bin\sot.exe is not in use.
#
# No network, no Julia, no npm -- those all happened at prepare time.

[CmdletBinding()]
param(
    # Restore the last-good transaction after a crash-loop. Invoked by
    # launch-sot.ps1's supervisor, gated there on a FRESH just-applied marker.
    [switch]$Rollback,
    # Install prefix override (tests). Default: the manifest's own prefix,
    # else %LOCALAPPDATA%\sot -- matching updater's legacy_updates_root().
    [string]$Prefix
)

# Never let a throw escape: this runs on the launch path.
$ErrorActionPreference = 'Continue'

# Log on the INFORMATION stream, not stderr and not stdout.
#   - `[Console]::Error.WriteLine` bypasses PowerShell redirection entirely, so
#     `& sot-apply.ps1 2>&1` captures nothing and the launcher's supervisor.log
#     loses every diagnostic -- precisely when an update failed and you need it.
#   - `Write-Output` would be captured, but it merges into the SUCCESS stream,
#     which would corrupt the return value of every function that logs before
#     returning (Set-Junction returns a bool; Read-JsonFile/Get-Sha256 return
#     $null on failure). `if (-not (Set-Junction ...))` would then test an array.
# Write-Host writes to the information stream: captured by the launcher with
# `6>&1`, visible when run by hand, and invisible to function output.
function Write-ApplyLog([string]$msg) { Write-Host "sot-apply: $msg" }

# The release matrix target for this host. Never trust the pointer's word for
# what THIS box is (mirrors the .sh's uname gate).
$TARGET = 'windows-x86_64'

if (-not $Prefix) { $Prefix = Join-Path $env:LOCALAPPDATA 'sot' }
$Updates  = Join-Path $Prefix 'updates'
$BinDir   = Join-Path $Prefix 'bin'
$Pending  = Join-Path $Updates "pending-$TARGET.json"
$LastGood = Join-Path $Updates "last-good-$TARGET.json"
$Marker   = Join-Path $Updates "just-applied-$TARGET"
$InstallJson = Join-Path $Prefix 'install.json'

if (-not (Test-Path -LiteralPath $Updates)) { exit 0 }

# ---- staging lock (shared with the Rust updater) ----------------------------
# Directory-creation as mutex, exactly like the .sh's `mkdir` -- arm/apply/
# rollback are one critical section against the stager.
$Lock = Join-Path $Updates '.lock'
try {
    New-Item -ItemType Directory -Path $Lock -ErrorAction Stop | Out-Null
} catch {
    Write-ApplyLog 'staging lock held -- skipping this launch'
    exit 0
}
try {
    Set-Content -LiteralPath (Join-Path $Lock 'owner') -Encoding utf8 `
        -Value "$PID@$env:COMPUTERNAME#ps1" -ErrorAction SilentlyContinue
} catch {}

# Everything from here runs under the lock; release it on every exit path.
function Exit-Apply([int]$code) {
    Remove-Item -LiteralPath (Join-Path $Lock 'owner') -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $Lock -Force -Recurse -ErrorAction SilentlyContinue
    exit $code
}

# ---- helpers ----------------------------------------------------------------
function Read-JsonFile([string]$path) {
    if (-not (Test-Path -LiteralPath $path)) { return $null }
    try { return (Get-Content -LiteralPath $path -Raw -ErrorAction Stop | ConvertFrom-Json) }
    catch { Write-ApplyLog "unparseable JSON at ${path}: $($_.Exception.Message)"; return $null }
}

# Write UTF-8 with NO BOM.
#
# `Set-Content -Encoding utf8` on Windows PowerShell 5.1 emits EF BB BF, and
# `serde_json::from_str` -- which is what reads install.json back in
# rust/updater/src/manifest.rs -- REJECTS a leading BOM. Getting this wrong is
# a one-shot, self-concealing failure: the first update applies cleanly, the
# rewritten manifest then fails to parse, `InstallManifest::for_current_exe()`
# returns None, and the frontend silently stops checking for updates forever
# after -- reporting only a debug-level "unreadable install manifest".
# Every JSON this script writes goes through here. (install-manifest.ps1 uses
# the same encoding for the same reason.)
function Write-Utf8NoBom([string]$path, [string]$text) {
    [System.IO.File]::WriteAllText($path, $text, (New-Object System.Text.UTF8Encoding($false)))
}

function Get-Sha256([string]$path) {
    try { return (Get-FileHash -LiteralPath $path -Algorithm SHA256 -ErrorAction Stop).Hash.ToLowerInvariant() }
    catch { return $null }
}

# Surgical field rewrite, NOT a parse+reserialize: install.json's schema is
# additive by design ("a newer installer can extend the schema"), so a
# round-trip through ConvertTo-Json would silently drop fields this script
# doesn't know about, and reorder the rest.
function Set-JsonField([string]$text, [string]$key, [string]$value) {
    $esc = $value -replace '\\', '\\\\' -replace '"', '\"'
    return [regex]::Replace($text, "(`"$key`"\s*:\s*)`"[^`"]*`"", "`${1}`"$esc`"")
}

# Directory junction -- no privilege required, unlike a symlink.
function Set-Junction([string]$link, [string]$target) {
    try {
        if (Test-Path -LiteralPath $link) {
            # Remove the LINK, never its contents: Remove-Item -Recurse on a
            # junction can follow into the target on older PowerShell, so drop
            # the reparse point with the directory API instead.
            [System.IO.Directory]::Delete($link)
        }
        $parent = Split-Path -Parent $link
        if ($parent -and -not (Test-Path -LiteralPath $parent)) {
            New-Item -ItemType Directory -Path $parent -Force | Out-Null
        }
        New-Item -ItemType Junction -Path $link -Target $target -ErrorAction Stop | Out-Null
        return $true
    } catch {
        Write-ApplyLog "junction $link -> ${target} failed: $($_.Exception.Message)"
        return $false
    }
}

# Canonicalise a path for COMPARISON. Windows hands back the same directory
# under different spellings, and a raw string compare then reports a correct
# junction as "did not flip" -- which sends the applier down Restore-Previous
# and means the machine NEVER accepts an update. Caught on the CI Windows
# runner, whose TEMP is the 8.3 form `C:\Users\RUNNER~1\...` while the
# junction's own Target reports the expanded `C:\Users\runneradmin\...`.
# Any install path containing a short-name component hits this.
#
# Normalise BOTH sides to the 8.3 SHORT form rather than trying to expand to
# the long one. Scripting.FileSystemObject's .Path just echoes back whichever
# spelling it was handed (so it cannot resolve the disagreement), but
# .ShortPath maps the long and short spellings of a directory onto the same
# string -- verified both directions. .NET's GetFullPath and Resolve-Path
# leave 8.3 untouched, so neither helps here.
# If 8.3 creation is disabled on the volume, .ShortPath returns the long form
# for both sides, which is still consistent. Case needs no handling:
# PowerShell's -eq/-ne on strings is already case-insensitive.
# Falls back to the trimmed input if the path is gone or COM is unavailable --
# no worse than the previous behaviour.
function Get-ComparablePath([string]$p) {
    if (-not $p) { return '' }
    $t = $p.TrimEnd('\')
    try {
        $fso = New-Object -ComObject Scripting.FileSystemObject
        if ($fso.FolderExists($t)) { return ([string]$fso.GetFolder($t).ShortPath).TrimEnd('\') }
    } catch {}
    return $t
}

function Get-JunctionTarget([string]$link) {
    try {
        $i = Get-Item -LiteralPath $link -Force -ErrorAction Stop
        if ($i.Target) { return @($i.Target)[0] }
    } catch {}
    return ''
}

$curTag = ''
$installText = ''
if (Test-Path -LiteralPath $InstallJson) {
    $installText = Get-Content -LiteralPath $InstallJson -Raw -ErrorAction SilentlyContinue
    $m = Read-JsonFile $InstallJson
    if ($m -and $m.tag) { $curTag = [string]$m.tag }
}

# ---- rollback mode ----------------------------------------------------------
if ($Rollback) {
    $lg = Read-JsonFile $LastGood
    if (-not $lg) { Write-ApplyLog 'rollback requested but no last-good state -- nothing to do'; Exit-Apply 0 }
    $lgTag = [string]$lg.tag
    $lgCheckout = [string]$lg.checkout
    if (-not $lgCheckout -or -not (Test-Path -LiteralPath $lgCheckout)) {
        Write-ApplyLog "last-good checkout '$lgCheckout' missing -- cannot roll back"; Exit-Apply 0
    }
    if ($curTag -eq $lgTag) {
        Write-ApplyLog "install already at last-good $lgTag -- nothing to roll back"; Exit-Apply 0
    }
    Write-ApplyLog "ROLLING BACK to $lgTag (marking $curTag bad for $TARGET)"
    foreach ($b in @('sot.exe', 'sotd.exe')) {
        $prev = Join-Path $BinDir "$b.prev"
        if (Test-Path -LiteralPath $prev) {
            Copy-Item -LiteralPath $prev -Destination (Join-Path $BinDir $b) -Force -ErrorAction SilentlyContinue
        }
    }
    [void](Set-Junction (Join-Path $Prefix 'repo\current') $lgCheckout)
    [void](Set-Junction (Join-Path $Prefix 'julia\current') $lgCheckout)
    if ($curTag -match '^v[0-9]') {
        New-Item -ItemType File -Path (Join-Path $Updates "bad-$curTag-$TARGET") -Force | Out-Null
    }
    if ($installText -and $lgTag) {
        $t = Set-JsonField $installText 'version' ($lgTag -replace '^v', '')
        $t = Set-JsonField $t 'tag' $lgTag
        try { Write-Utf8NoBom $InstallJson $t } catch {
            Write-ApplyLog "could not rewrite install.json during rollback: $($_.Exception.Message)"
        }
    }
    Remove-Item -LiteralPath $Pending, $Marker -Force -ErrorAction SilentlyContinue
    Write-ApplyLog "rollback complete -- running $lgTag"
    Exit-Apply 0
}

# ---- apply ------------------------------------------------------------------
if (-not (Test-Path -LiteralPath $Pending)) { Exit-Apply 0 }

$ptr = Read-JsonFile $Pending
function Remove-Pending { Remove-Item -LiteralPath $Pending -Force -ErrorAction SilentlyContinue }
if (-not $ptr) { Write-ApplyLog 'pending pointer unreadable -- dropping'; Remove-Pending; Exit-Apply 0 }

$tag       = [string]$ptr.tag
$ptrTarget = [string]$ptr.target
$checkout  = [string]$ptr.checkout
$commit    = [string]$ptr.commit
$asset     = [string]$ptr.asset
$assetSha  = [string]$ptr.asset_sha256

if ($tag -notmatch '^v[0-9]') {
    Write-ApplyLog "pending tag '$tag' fails validation -- dropping pointer"; Remove-Pending; Exit-Apply 0
}
# Reject path material in the tag -- it is used to build directory names below.
if ($tag -match '[\\/]' -or $tag -match '\.\.') {
    Write-ApplyLog "pending tag '$tag' contains path material -- dropping pointer"; Remove-Pending; Exit-Apply 0
}
if ($ptrTarget -ne $TARGET) {
    Write-ApplyLog "pending pointer is for target '$ptrTarget', this host is $TARGET -- dropping"; Remove-Pending; Exit-Apply 0
}
if (-not $checkout -or -not $commit -or -not $asset -or -not $assetSha) {
    Write-ApplyLog 'pending pointer is missing fields -- dropping'; Remove-Pending; Exit-Apply 0
}

# Stage dirs are keyed <tag>-<target> (a shared root serves several platforms).
$ready = Join-Path $Updates "$tag-$TARGET"
$top = $asset -replace '\.zip$', '' -replace '\.tar\.gz$', ''
$staged = Join-Path $ready $top

if ($curTag -eq $tag) {
    Write-ApplyLog "install is already at $tag -- clearing stale pending pointer"; Remove-Pending; Exit-Apply 0
}

# ---- verify the whole transaction BEFORE touching anything ------------------
# A damaged stage clears the pointer AND the stage dir so the next cycle
# re-stages fresh instead of auto-exiting into the same failure forever.
function Stop-Damaged([string]$why) {
    Write-ApplyLog "$why -- dropping pointer and damaged stage so the updater re-stages"
    Remove-Pending
    Remove-Item -LiteralPath $ready -Recurse -Force -ErrorAction SilentlyContinue
    Exit-Apply 0
}

if (-not (Test-Path -LiteralPath (Join-Path $ready 'manifest.json'))) { Stop-Damaged "no ready manifest for $tag" }
if (-not (Test-Path -LiteralPath (Join-Path $staged 'sot.exe')))      { Stop-Damaged 'staged sot.exe missing' }

$got = Get-Sha256 (Join-Path $ready $asset)
if (-not $got -or $got -ne $assetSha.ToLowerInvariant()) { Stop-Damaged 'staged archive digest mismatch' }

# Per-file digests of what we are actually about to install -- the extracted
# tree is mutable independently of the archive it came from.
$filesSha = Join-Path $ready 'files.sha256'
if (Test-Path -LiteralPath $filesSha) {
    foreach ($line in (Get-Content -LiteralPath $filesSha -ErrorAction SilentlyContinue)) {
        if ($line -notmatch '^\s*([0-9a-fA-F]{64})\s+\*?(.+?)\s*$') { continue }
        $want = $Matches[1].ToLowerInvariant()
        $rel  = $Matches[2] -replace '/', '\'
        $have = Get-Sha256 (Join-Path $ready $rel)
        if (-not $have -or $have -ne $want) { Stop-Damaged "staged file digest mismatch: $rel" }
    }
} else {
    Write-ApplyLog 'note: stage has no files.sha256 (pre-C4 stage) -- archive digest only'
}

# Prepared worktree: exact commit, and no modified tracked files.
if (-not (Test-Path -LiteralPath $checkout)) {
    Write-ApplyLog "prepared checkout $checkout missing -- dropping pointer"; Remove-Pending; Exit-Apply 0
}
$head = (git -C $checkout rev-parse HEAD 2>$null | Select-Object -First 1)
if ("$head".Trim() -ne $commit) {
    Write-ApplyLog "prepared checkout HEAD ($head) != pinned commit ($commit) -- dropping pointer"; Remove-Pending; Exit-Apply 0
}
$dirty = (git -C $checkout status --porcelain -uno 2>$null)
if ($dirty) {
    Write-ApplyLog 'prepared checkout has modified tracked files -- dropping pointer'; Remove-Pending; Exit-Apply 0
}

# ---- record last-good (the pre-apply state) for rollback --------------------
$prevCheckout = Get-JunctionTarget (Join-Path $Prefix 'repo\current')
$lgTmp = "$LastGood.tmp"
$lgBody = @"
{
  "tag": "$curTag",
  "checkout": "$($prevCheckout -replace '\\', '\\\\')"
}
"@
try { Write-Utf8NoBom $lgTmp $lgBody } catch {
    Write-ApplyLog "could not write last-good state: $($_.Exception.Message)"
}
Move-Item -LiteralPath $lgTmp -Destination $LastGood -Force -ErrorAction SilentlyContinue

# ---- the flip: binaries, then pointers -- all-or-restore ---------------------
function Restore-Previous([string]$why) {
    Write-ApplyLog "$why -- restoring previous binaries and pointers"
    foreach ($r in @('sot.exe', 'sotd.exe')) {
        $prev = Join-Path $BinDir "$r.prev"
        if (Test-Path -LiteralPath $prev) {
            Copy-Item -LiteralPath $prev -Destination (Join-Path $BinDir $r) -Force -ErrorAction SilentlyContinue
        }
    }
    if ($prevCheckout) {
        [void](Set-Junction (Join-Path $Prefix 'repo\current') $prevCheckout)
        [void](Set-Junction (Join-Path $Prefix 'julia\current') $prevCheckout)
    }
    # Pending stays: the stage verified clean, so the failure is local
    # (permissions, disk); retrying at the next launch is safe and fail-open.
    Exit-Apply 0
}

if (-not (Test-Path -LiteralPath $BinDir)) { New-Item -ItemType Directory -Path $BinDir -Force | Out-Null }
foreach ($b in @('sot.exe', 'sotd.exe')) {
    $src = Join-Path $staged $b
    if (-not (Test-Path -LiteralPath $src)) { continue }
    $dst = Join-Path $BinDir $b
    if (Test-Path -LiteralPath $dst) {
        Copy-Item -LiteralPath $dst -Destination "$dst.prev" -Force -ErrorAction SilentlyContinue
    }
    try {
        Copy-Item -LiteralPath $src -Destination "$dst.new" -Force -ErrorAction Stop
        Move-Item -LiteralPath "$dst.new" -Destination $dst -Force -ErrorAction Stop
    } catch {
        Restore-Previous "installing $b failed: $($_.Exception.Message)"
    }
}

if (-not (Set-Junction (Join-Path $Prefix 'repo\current') $checkout)) { Restore-Previous 'flipping repo\current failed' }
$flipped = Get-ComparablePath (Get-JunctionTarget (Join-Path $Prefix 'repo\current'))
$wanted  = Get-ComparablePath $checkout
if ($flipped -ne $wanted) {
    Restore-Previous "repo\current did not flip (points at '$flipped', wanted '$wanted')"
}
if (-not (Set-Junction (Join-Path $Prefix 'julia\current') $checkout)) { Restore-Previous 'flipping julia\current failed' }

# ---- rewrite install.json (preserve role/prefix/config/service) -------------
if ($installText) {
    try {
        $t = Set-JsonField $installText 'version' ($tag -replace '^v', '')
        $t = Set-JsonField $t 'tag' $tag
        $t = Set-JsonField $t 'commit' $commit
        Write-Utf8NoBom $InstallJson $t
    } catch {
        Restore-Previous "rewriting install.json failed: $($_.Exception.Message)"
    }
}

# Success: arm the crash-loop health window, clear the pointer.
New-Item -ItemType File -Path $Marker -Force | Out-Null
Remove-Pending
Write-ApplyLog "APPLIED $tag (previous kept as .prev; rollback marker armed)"

# ---- prune: keep the new and previous version dirs --------------------------
$keep = @($tag)
if ($prevCheckout) { $keep += (Split-Path -Leaf $prevCheckout) }
$versions = Join-Path $Prefix 'repo\versions'
if (Test-Path -LiteralPath $versions) {
    foreach ($v in (Get-ChildItem -LiteralPath $versions -Directory -ErrorAction SilentlyContinue)) {
        if ($keep -contains $v.Name) { continue }
        $base = Join-Path $Prefix 'repo\base'
        if (Test-Path -LiteralPath $base) { git -C $base worktree remove --force $v.FullName 2>$null | Out-Null }
        if (Test-Path -LiteralPath $v.FullName) {
            Remove-Item -LiteralPath $v.FullName -Recurse -Force -ErrorAction SilentlyContinue
        }
    }
}
# Old stage dirs for THIS target (the archive + extracted tree are large).
foreach ($d in (Get-ChildItem -LiteralPath $Updates -Directory -Filter "v*-$TARGET" -ErrorAction SilentlyContinue)) {
    if ($d.Name -eq "$tag-$TARGET") { continue }
    Remove-Item -LiteralPath $d.FullName -Recurse -Force -ErrorAction SilentlyContinue
}

Exit-Apply 0
