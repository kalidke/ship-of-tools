# launch-splash.ps1 — launch progress splash for launch-sot.ps1 (maintainer note, 2026-07-06:
# "say what it's doing ... or Error: xxx"). The Windows launcher runs hidden, so
# the dev-freshness pull+rebuild (up to ~1-3 min after a big merge) used to be
# invisible and read as a dead taskbar click. This is the visible surface.
#
# It runs as its OWN process, spawned first-thing by launch-sot.ps1, for two
# reasons: (1) it can paint within ~1s of the click, before any slow work; and
# (2) it has its own Windows message pump, so it keeps animating during the
# launcher's BLOCKING `cargo build` — a splash on the launcher's own thread would
# freeze and gray out to "(Not Responding)" for the whole build.
#
# Contract with the launcher (a one-line status file it overwrites, no BOM):
#   "<phase text>"   -> shown as the current status (e.g. "Rebuilding frontend...")
#   "ERROR: <msg>"   -> sticky red; stops auto-close; shows a Close button
#   "DONE"           -> close the splash (unless a sticky ERROR is showing)
#
# FAIL-OPEN by construction: the launcher Start-Process'd us and moved on, so
# anything that throws in here dies with this process and never touches the
# launch. $ErrorActionPreference stays 'Continue'/SilentlyContinue for that.
param(
    [Parameter(Mandatory)][string]$StatusFile
)
$ErrorActionPreference = 'SilentlyContinue'
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing

$bg     = [System.Drawing.Color]::FromArgb(24, 26, 32)
$fg     = [System.Drawing.Color]::FromArgb(235, 238, 245)
$accent = [System.Drawing.Color]::FromArgb(120, 190, 255)
$errCol = [System.Drawing.Color]::FromArgb(255, 120, 120)

$form = New-Object System.Windows.Forms.Form
$form.FormBorderStyle = 'None'
$form.StartPosition   = 'CenterScreen'
$form.Size            = New-Object System.Drawing.Size(380, 132)
$form.BackColor       = $bg
$form.TopMost         = $true
$form.ShowInTaskbar   = $false

# Thin brand accent along the top edge.
$stripe = New-Object System.Windows.Forms.Panel
$stripe.BackColor = $accent
$stripe.Height    = 3
$stripe.Dock      = 'Top'
$form.Controls.Add($stripe)

$title = New-Object System.Windows.Forms.Label
$title.Text      = 'Ship of Tools'
$title.ForeColor = $fg
$title.Font      = New-Object System.Drawing.Font('Segoe UI', 13, [System.Drawing.FontStyle]::Bold)
$title.AutoSize  = $true
$title.Location  = New-Object System.Drawing.Point(20, 18)
$form.Controls.Add($title)

$status = New-Object System.Windows.Forms.Label
$status.Text      = 'Starting...'
$status.ForeColor = $accent
$status.Font      = New-Object System.Drawing.Font('Segoe UI', 10)
$status.AutoSize  = $false
$status.Size      = New-Object System.Drawing.Size(340, 22)
$status.Location  = New-Object System.Drawing.Point(20, 52)
$form.Controls.Add($status)

$bar = New-Object System.Windows.Forms.ProgressBar
$bar.Style                 = 'Marquee'
$bar.MarqueeAnimationSpeed = 30
$bar.Size                  = New-Object System.Drawing.Size(340, 12)
$bar.Location              = New-Object System.Drawing.Point(20, 84)
$form.Controls.Add($bar)

$closeBtn = New-Object System.Windows.Forms.Button
$closeBtn.Text                    = 'Close'
$closeBtn.Size                    = New-Object System.Drawing.Size(84, 26)
$closeBtn.Location                = New-Object System.Drawing.Point(276, 82)
$closeBtn.FlatStyle               = 'Flat'
$closeBtn.ForeColor               = $fg
$closeBtn.Visible                 = $false
$closeBtn.Add_Click({ $form.Close() })
$form.Controls.Add($closeBtn)

$script:hadError = $false

$timer = New-Object System.Windows.Forms.Timer
$timer.Interval = 150
$timer.Add_Tick({
    $line = ''
    try { $line = ([System.IO.File]::ReadAllText($StatusFile)).Trim() } catch { }
    if (-not $line) { return }

    if ($line -eq 'DONE') {
        # A sticky error keeps the splash up so the user actually sees it; a
        # clean launch closes silently the instant the FE window is up.
        if (-not $script:hadError) { $timer.Stop(); $form.Close() }
        return
    }
    if ($line -like 'ERROR:*') {
        $script:hadError  = $true
        $status.Text      = $line.Substring(6).Trim()
        $status.ForeColor = $errCol
        $bar.Visible      = $false
        $closeBtn.Visible = $true
        return
    }
    if (-not $script:hadError) { $status.Text = $line }
})
$timer.Start()

$form.Add_Shown({ $form.Activate() })
[System.Windows.Forms.Application]::Run($form)
