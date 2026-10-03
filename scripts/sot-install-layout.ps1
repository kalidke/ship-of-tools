# sot-install-layout.ps1 -- shared "is this launch pinned to the installed
# checkout" predicate + shortcut-target rule, and the home of the launcher code
# id (Get-SotLauncherCodeId), which launch-sot.ps1 uses on a converge. Dot-sourced by launch-sot.ps1
# (which gates its self-update prelude on Test-SotPinnedCheckout) and
# install-shortcut.ps1 (which uses Get-SotLauncherTarget to decide what the
# shortcut/pin should point at). Same shape as sot-hosts.ps1: one
# dot-sourceable file, no privileged access either caller doesn't already
# have.
#
# See docs/adr/0030-versioning-release-and-auto-update.md's 2026-09-17
# amendment for why this predicate exists and what it replaced.
#
# ASCII ONLY in string literals (see the same note in launch-sot.ps1 /
# sot-hosts.ps1): this file has no BOM, and Windows PowerShell 5.1 decodes
# a non-ASCII byte inside a string literal into a phantom quote that fails
# the whole parse.

# Pinned iff this script is running FROM repo\current -- a path identity,
# not a git-state one: it is exactly the invariant Linux's `sot-launch`
# wrapper enforces by construction (execs repo/current/scripts/
# launch-sot.sh, never the base clone), and it cannot disagree with which
# file is actually executing the way a git-state predicate could.
function Test-SotPinnedCheckout {
    param(
        [Parameter(Mandatory)][string]$RepoPath,
        [Parameter(Mandatory)][string]$Prefix
    )
    return ($RepoPath -eq (Join-Path $Prefix 'repo\current'))
}

# The shortcut/pin target, in priority order: the pinned launcher once
# sot-apply.ps1 (or Initialize-InstallLayout's own first run) has created
# repo\current, else the clone's own launcher (bootstrap only, before any
# layout exists). Idempotent and safe to call on every install-shortcut.ps1
# run: a box that migrates gets repointed once and every later run finds
# the same target and rewrites nothing that changed.
function Get-SotLauncherTarget {
    param(
        [Parameter(Mandatory)][string]$Prefix,
        [Parameter(Mandatory)][string]$ClonePath
    )
    $pinned = Join-Path $Prefix 'repo\current\scripts\launch-sot.ps1'
    if (Test-Path -LiteralPath $pinned) { return $pinned }
    return (Join-Path $ClonePath 'scripts\launch-sot.ps1')
}

# The identity of the launcher code in a scripts directory: the SHA-256 of
# launch-sot.ps1 and of the two files it dot-sources, joined with '-', or ''
# when any of them cannot be read. launch-sot.ps1 logs its own at start (the
# "supervisor start" line) and, on a converge, re-invokes the launcher when the
# files on disk have another id (ADR 0017's 0.6.6 amendment). Read through
# repo\current, a flip by sot-apply.ps1 shows here with the path unchanged.
function Get-SotLauncherCodeId {
    param([Parameter(Mandatory)][string]$ScriptsDir)
    try {
        $ids = foreach ($f in @('launch-sot.ps1', 'sot-hosts.ps1', 'sot-install-layout.ps1')) {
            (Get-FileHash -Algorithm SHA256 -LiteralPath (Join-Path $ScriptsDir $f) -ErrorAction Stop).Hash
        }
        return ($ids -join '-')
    } catch {
        return ''
    }
}

# Folder trust, the Windows twin of install.sh's [trust] step. The daemon
# pre-answers the agent's folder-trust dialog for every session root under ONE
# declared absolute prefix read from settings.toml; nothing else on Windows
# writes that declaration. Written once: an existing [trust] table (matched the
# way install.sh and the daemon's parser do -- trimmed line, brackets stripped,
# name trimmed) is the owner's own answer and is never rewritten. The prefix is
# the home folder with / as the separator, the spelling Claude Code keys its
# own projects by. UTF-8 without BOM, appended, creating folder and file.
function Set-SotFolderTrust {
    param(
        [Parameter(Mandatory)][string]$ConfigDir,
        [Parameter(Mandatory)][string]$HomeDir
    )
    $file = Join-Path $ConfigDir 'settings.toml'
    $existing = ''
    if (Test-Path -LiteralPath $file) {
        $existing = [System.IO.File]::ReadAllText($file)
        foreach ($line in ($existing -split "`n")) {
            $t = $line.Trim()
            if ($t.StartsWith('[') -and $t.EndsWith(']') -and $t.Trim('[', ']').Trim() -ceq 'trust') {
                return $false
            }
        }
    } elseif (-not (Test-Path -LiteralPath $ConfigDir)) {
        New-Item -ItemType Directory -Path $ConfigDir -Force | Out-Null
    }
    $prefix = $HomeDir.Replace('\', '/')
    $block = "`n[trust]`n" +
        "# Every session root under this absolute prefix counts as already`n" +
        "# trusted, so an agent the daemon spawns there never stops at its`n" +
        "# folder-trust dialog. Narrow it to the parent your repos live under,`n" +
        "# or comment it out to answer that dialog by hand. Roots outside it`n" +
        "# are left untouched.`n" +
        "root_prefix = `"$prefix`"`n"
    $utf8 = New-Object System.Text.UTF8Encoding($false)
    [System.IO.File]::WriteAllText($file, $existing + $block, $utf8)
    return $true
}
