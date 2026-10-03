#!/usr/bin/env bash
# job-breakaway-probe.sh: does a process stay in a Windows job built like the daemon's? git-bash on native Windows.
# Starts only its own jobs and processes, ends them all, never opens a live row's job. Flavours: BA = today's
# KILL_ON_JOB_CLOSE|BREAKAWAY_OK, K = sealed. Chains: jd julia detach; jgd julia run() then detach (the kernel's case);
# jbash julia -> MSYS bash -> ping; bash MSYS bash -> ping; p6 bash -> PATH julia -> detach (the incident).
#   --pid N  prints job membership of N and its live ancestors, with query rights only.
set -u
case "$(uname -s)" in MINGW*|MSYS*) ;; *) echo "SKIP: needs git-bash on native Windows"; exit 0 ;; esac
T=$(mktemp -d); trap 'rm -rf "$T"' EXIT; Tm=$(cygpath -m "$T"); B=$(cygpath -m "$(command -v bash)")
J=$(julia --startup-file=no -e 'print(replace(joinpath(Sys.BINDIR, Base.julia_exename()), "\\" => "/"))' 2>/dev/null | tr -d '\r')
[ "${1:-}" = --pid ] || [ -n "$J" ] || { echo "FAIL: julia is not on PATH; the probe needs it"; exit 1; }
echo 'run(detach(`ping -n $(ARGS[1]) 127.0.0.1`); wait=false)' > "$T/jd.jl"
echo 'run(`cmd /c exit 0`); run(detach(`ping -n $(ARGS[1]) 127.0.0.1`); wait=false)' > "$T/jgd.jl"
echo 's = "ping -n $(ARGS[1]) 127.0.0.1 >/dev/null 2>&1 &"; run(`$(ARGS[2]) -c $s`)' > "$T/jbash.jl"
echo 'ping -n "$1" 127.0.0.1 >/dev/null 2>&1 &' > "$T/bash.sh"
echo 'julia -e "p=run(detach(\`ping -n $1 127.0.0.1\`);wait=false);println(getpid(p))" >/dev/null' > "$T/p6.sh"
cat > "$T/probe.ps1" <<'PS'
param([string]$Mode, [int]$Pid0, [string]$T, [string]$Bash, [string]$Julia)
Add-Type -TypeDefinition @'
using System; using System.Runtime.InteropServices; using System.Text;
public static class W {
 [StructLayout(LayoutKind.Sequential)] public struct L { public long a, b; public uint Flags; public UIntPtr c, d; public uint e; public UIntPtr f; public uint g, h; }
 [StructLayout(LayoutKind.Sequential)] public struct X { public L B; public ulong i1, i2, i3, i4, i5, i6; public UIntPtr m1, m2, m3, m4; }
 [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)] public struct S { public int cb; public string r, d, t; public int x, y, xs, ys, xc, yc, fa, fl; public short sw, c2; public IntPtr r2, i, o, e; }
 [StructLayout(LayoutKind.Sequential)] public struct P { public IntPtr hp, ht; public int pid, tid; }
 [DllImport("kernel32", SetLastError = true)] static extern IntPtr CreateJobObjectW(IntPtr a, IntPtr n);
 [DllImport("kernel32", SetLastError = true)] static extern bool SetInformationJobObject(IntPtr j, int c, ref X i, int n);
 [DllImport("kernel32", SetLastError = true)] static extern bool QueryInformationJobObject(IntPtr j, int c, out X i, int n, IntPtr r);
 [DllImport("kernel32", SetLastError = true)] static extern bool AssignProcessToJobObject(IntPtr j, IntPtr p);
 [DllImport("kernel32", SetLastError = true)] static extern bool IsProcessInJob(IntPtr p, IntPtr j, out bool r);
 [DllImport("kernel32", SetLastError = true, CharSet = CharSet.Unicode)] static extern bool CreateProcessW(string a, StringBuilder c, IntPtr pa, IntPtr ta, bool inh, uint f, IntPtr env, string cwd, ref S si, out P pi);
 [DllImport("kernel32")] static extern uint ResumeThread(IntPtr t);
 [DllImport("kernel32")] public static extern IntPtr OpenProcess(int a, bool i, int pid);
 [DllImport("kernel32")] public static extern bool TerminateJobObject(IntPtr j, uint c);
 [DllImport("kernel32")] public static extern bool TerminateProcess(IntPtr p, uint c);
 [DllImport("kernel32")] public static extern uint WaitForSingleObject(IntPtr h, uint ms);
 [DllImport("kernel32")] public static extern bool CloseHandle(IntPtr h);
 public static IntPtr Job(uint flags) { // as AnonymousJob::create (conpty.rs:248-276), with the flags under test
  IntPtr j = CreateJobObjectW(IntPtr.Zero, IntPtr.Zero); X x = new X(); x.B.Flags = flags;
  if (j == IntPtr.Zero || !SetInformationJobObject(j, 9, ref x, Marshal.SizeOf(x))) throw new Exception("job: " + Marshal.GetLastWin32Error());
  return j; }
 public static uint Flags(IntPtr j) { X x; QueryInformationJobObject(j, 9, out x, Marshal.SizeOf(typeof(X)), IntPtr.Zero); return x.B.Flags; }
 public static void Start(IntPtr j, string cmd) { // suspended, assigned, resumed: the lane's order
  S si = new S(); si.cb = Marshal.SizeOf(si); P pi;
  if (!CreateProcessW(null, new StringBuilder(cmd, 4096), IntPtr.Zero, IntPtr.Zero, false, 0x08000004, IntPtr.Zero, null, ref si, out pi)) throw new Exception("spawn: " + Marshal.GetLastWin32Error());
  bool ok = AssignProcessToJobObject(j, pi.hp); int err = Marshal.GetLastWin32Error();
  if (ok) ResumeThread(pi.ht); else TerminateProcess(pi.hp, 1);
  CloseHandle(pi.ht); CloseHandle(pi.hp); if (!ok) throw new Exception("assign: " + err); }
 public static string In(IntPtr h, IntPtr j) { bool r; return IsProcessInJob(h, j, out r) ? r.ToString() : "err" + Marshal.GetLastWin32Error(); }
}
'@
$me = [W]::OpenProcess(0x1000, $false, $PID); "controller in_any_job=$([W]::In($me, [IntPtr]::Zero))"
if ($Mode -eq '--pid') { $p = $Pid0
  for ($k = 0; $k -lt 12 -and $p; $k++) { $w = Get-CimInstance Win32_Process -Filter "ProcessId=$p"
    if (-not $w) { "pid=$p gone"; break }
    $h = [W]::OpenProcess(0x1000, $false, $p); "pid=$p name=$($w.Name) in_any_job=$([W]::In($h, [IntPtr]::Zero))"
    [void][W]::CloseHandle($h); $p = $w.ParentProcessId }
  exit 0 }
$cases = [ordered]@{ jd = "`"$Julia`" --startup-file=no `"$T/jd.jl`" {0}"; jgd = "`"$Julia`" --startup-file=no `"$T/jgd.jl`" {0}"
  jbash = "`"$Julia`" --startup-file=no `"$T/jbash.jl`" {0} `"$Bash`""; bash = "`"$Bash`" `"$T/bash.sh`" {0}"; p6 = "`"$Bash`" `"$T/p6.sh`" {0}" }
$base = 40000 + 10 * (Get-Random -Maximum 900); $i = 0; $res = @{}
try {
 foreach ($fl in @(@('BA', 0x2800), @('K', 0x2000))) { foreach ($c in $cases.Keys) {
  $n = $base + $i; $i++; $job = [W]::Job($fl[1]); $flags = '0x{0:x}' -f [W]::Flags($job)
  [W]::Start($job, ($cases[$c] -f $n)); $ping = $null
  for ($k = 0; $k -lt 60 -and -not $ping; $k++) { Start-Sleep -Milliseconds 500
    $ping = Get-CimInstance Win32_Process -Filter "Name='PING.EXE'" | Where-Object { $_.CommandLine -match "-n $n 127" } | Select-Object -First 1 }
  if (-not $ping) { [void][W]::TerminateJobObject($job, 1); [void][W]::CloseHandle($job); $res["$c/$($fl[0])"] = 'none'; "$c $($fl[0]) flags=$flags ping=none"; continue }
  $h = [W]::OpenProcess(0x101001, $false, [int]$ping.ProcessId); $in = [W]::In($h, $job); $any = [W]::In($h, [IntPtr]::Zero)
  [void][W]::TerminateJobObject($job, 1); [void][W]::CloseHandle($job)
  $end = if ([W]::WaitForSingleObject($h, 3000) -eq 0) { 'died' } else { [void][W]::TerminateProcess($h, 1); 'SURVIVED' }
  [void][W]::CloseHandle($h); $res["$c/$($fl[0])"] = "$in/$end"
  "$c $($fl[0]) flags=$flags ping=$($ping.ProcessId) in_job=$in any_job=$any after_end=$end" } }
} finally {
 Get-CimInstance Win32_Process -Filter "Name='PING.EXE'" | Where-Object { $_.CommandLine -match "-n (\d+) 127" -and [int]$Matches[1] -ge $base -and [int]$Matches[1] -lt $base + 10 } | ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }
}
$yn = { param($b) if ($b) { 'YES' } else { 'NO' } }
"C2 julia's detach leaves a job:               " + (& $yn ($res['jd/BA'] -like 'False/*'))
"C1 MSYS adds an explicit breakaway:            " + (& $yn (($res['bash/BA'] -like 'False/*') -and ($res['bash/K'] -like 'True/*')))
"C3 silent breakaway climbs a BREAKAWAY_OK job: " + (& $yn ($res['jgd/BA'] -like 'False/*'))
$bad = @($res.Keys | Where-Object { $_ -like '*/K' -and $res[$_] -ne 'True/died' })
"SEALED job keeps every chain:                  " + $(if ($bad.Count -eq 0) { 'YES' } else { 'NO: ' + ($bad -join ' ') })
PS
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "$(cygpath -w "$T/probe.ps1")" "${1:-run}" "${2:-0}" "$Tm" "$B" "$J" | tr -d '\r'
