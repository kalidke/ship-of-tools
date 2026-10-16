# in-job.ps1 CMD [ARG...] -- runs CMD with its ARGs as the one child of this script inside a Windows job object, the
# test job (Windows): every process CMD starts, at any depth, is in the job and ends with it, and the job ends when this
# script does: when CMD exits, when this script is ended from outside (Stop-Process, taskkill), or when the ssh session
# it runs under ends (its nearest sshd ancestor exits: Windows OpenSSH ends no process of a session whose client is
# gone). A parent-id walk (taskkill /T, Stop-SotTree) cannot do this on Git Bash, whose exec leaves the new process a
# creator that has exited. Exit status: CMD's; 129 when the ssh session ended first; 2 on a usage error; 8 when CMD
# exits 0 with processes of the job still running (a CMD that exits non-zero keeps its own). Then one stderr line names
# them (image and pid, at most 16) and the job ends them before the script exits.
$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"
if ($args.Count -lt 1) { [Console]::Error.WriteLine("usage: in-job.ps1 CMD [ARG...]"); exit 2 }
Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
public static class SotTestJob {
    [StructLayout(LayoutKind.Sequential)] struct Basic {
        public long PerProcessUserTimeLimit, PerJobUserTimeLimit; public uint LimitFlags;
        public UIntPtr MinimumWorkingSetSize, MaximumWorkingSetSize; public uint ActiveProcessLimit;
        public UIntPtr Affinity; public uint PriorityClass, SchedulingClass;
    }
    [StructLayout(LayoutKind.Sequential)] struct Io { public ulong R, W, O, RB, WB, OB; }
    [StructLayout(LayoutKind.Sequential)] struct Extended {
        public Basic BasicLimitInformation; public Io IoInfo;
        public UIntPtr ProcessMemoryLimit, JobMemoryLimit, PeakProcessMemoryUsed, PeakJobMemoryUsed;
    }
    [DllImport("kernel32.dll", SetLastError = true)] static extern IntPtr CreateJobObjectW(IntPtr attributes, IntPtr name);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool SetInformationJobObject(IntPtr job, int infoClass, ref Extended info, int length);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool QueryInformationJobObject(IntPtr job, int infoClass, byte[] info, int length, IntPtr returned);
    [DllImport("kernel32.dll", SetLastError = true)] static extern bool AssignProcessToJobObject(IntPtr job, IntPtr process);
    [DllImport("kernel32.dll", SetLastError = true)] static extern bool TerminateJobObject(IntPtr job, uint exitCode);
    [DllImport("kernel32.dll")] static extern IntPtr GetCurrentProcess();
    // A new job holding this process: JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE (0x2000, through
    // JobObjectExtendedLimitInformation, 9) and no breakaway, so every process created under it stays in it, and all
    // end when its one handle, this process's and not inheritable, closes.
    public static IntPtr EnterOwn() {
        IntPtr job = CreateJobObjectW(IntPtr.Zero, IntPtr.Zero);
        if (job == IntPtr.Zero) throw new Win32Exception();
        Extended info = new Extended();
        info.BasicLimitInformation.LimitFlags = 0x2000;
        if (!SetInformationJobObject(job, 9, ref info, Marshal.SizeOf(typeof(Extended)))) throw new Win32Exception();
        if (!AssignProcessToJobObject(job, GetCurrentProcess())) throw new Win32Exception();
        return job;
    }
    // The job's running processes, this one included (JobObjectBasicAccountingInformation, 1: ActiveProcesses at byte 40).
    public static uint Active(IntPtr job) {
        byte[] info = new byte[48];
        if (!QueryInformationJobObject(job, 1, info, info.Length, IntPtr.Zero)) throw new Win32Exception();
        return BitConverter.ToUInt32(info, 40);
    }
    // The ids in the job's process list, up to max of them (JobObjectBasicProcessIdList, 3: two DWORDs, then one ULONG_PTR
    // per id from byte 8). The list can hold a process that has already exited, so a caller names only the ids it still
    // finds. ERROR_MORE_DATA (234) only says the list is longer than the buffer.
    public static uint[] Pids(IntPtr job, int max) {
        byte[] info = new byte[8 + IntPtr.Size * max];
        if (!QueryInformationJobObject(job, 3, info, info.Length, IntPtr.Zero)
            && Marshal.GetLastWin32Error() != 234) throw new Win32Exception();
        uint[] ids = new uint[Math.Min(BitConverter.ToInt32(info, 4), max)];
        for (int i = 0; i < ids.Length; i++)
            ids[i] = (uint)(IntPtr.Size == 8 ? BitConverter.ToInt64(info, 8 + 8 * i) : BitConverter.ToInt32(info, 8 + 4 * i));
        return ids;
    }
    // Ends every process of the job, this one included, with exitCode (TerminateJobObject).
    public static void End(IntPtr job, int exitCode) {
        if (!TerminateJobObject(job, (uint)exitCode)) throw new Win32Exception();
    }
}
'@
# The ssh session this script runs under: its nearest sshd ancestor, each step created before the one below it (a
# recorded parent created later is a reused id). None when it runs under no ssh session.
function Get-SotSession {
    $id = $PID; $born = (Get-Process -Id $PID).StartTime
    for ($i = 0; $i -lt 32; $i++) {
        $me = Get-CimInstance Win32_Process -Filter "ProcessId=$id"
        $up = if ($me) { Get-CimInstance Win32_Process -Filter "ProcessId=$($me.ParentProcessId)" }
        if (-not $up -or $up.CreationDate -gt $born) { return $null }
        if ($up.Name -in @("sshd.exe", "sshd-session.exe")) {
            $p = Get-Process -Id $up.ProcessId -ErrorAction SilentlyContinue
            if ($p -and [math]::Abs(($p.StartTime - $up.CreationDate).TotalSeconds) -lt 1) { return $p } else { return $null }
        }
        $id = $up.ProcessId; $born = $up.CreationDate
    }
    return $null
}
# One Windows command-line word per argument, every word quoted: CommandLineToArgvW and the MSYS runtime read it back
# alike, and the MSYS runtime globs no quoted word.
function ConvertTo-SotArg([string]$Word) {
    return '"' + (($Word -replace '(\\*)"', '$1$1\"') -replace '(\\+)$', '$1$1') + '"'
}
$job = [SotTestJob]::EnterOwn()
$session = Get-SotSession
if (-not $session -and $env:SSH_CONNECTION) { [Console]::Error.WriteLine("in-job.ps1: SSH_CONNECTION is set and no sshd ancestor was found; the ssh session's end is not watched") }
# A bare command is looked up on PATH, as the calling shell would: CreateProcess alone looks in System32 first, where
# `bash` is WSL's.
$exe = $args[0]
if ($exe -notmatch '[\\/:]') { $exe = (Get-Command $exe -CommandType Application -ErrorAction Stop)[0].Source }
$psi = New-Object System.Diagnostics.ProcessStartInfo($exe, ((@($args | Select-Object -Skip 1) | ForEach-Object { ConvertTo-SotArg "$_" }) -join ' '))
$psi.UseShellExecute = $false
$cmd = [System.Diagnostics.Process]::Start($psi)
while (-not $cmd.WaitForExit(500)) {
    if ($session -and $session.HasExited) { exit 129 }
}
$code = $cmd.ExitCode
$left = [SotTestJob]::Active($job) - 1
if ($left -gt 0) {
    $named = @()
    foreach ($p in [SotTestJob]::Pids($job, 32)) {
        $w = if ($p -ne $PID) { Get-CimInstance Win32_Process -Filter "ProcessId=$p" }
        if ($w -and $named.Count -lt 16) { $named += "$($w.Name) $p" }
    }
    $more = if ($left -gt $named.Count) { " and $($left - $named.Count) more" } else { "" }
    if ($code -eq 0) { $code = 8 }
    [Console]::Error.WriteLine("in-job.ps1: $left processes $($args[0]) started were still running when it exited ($($named -join ', '))$more; the job ends them and in-job.ps1 exits $code")
    [SotTestJob]::End($job, $code)
}
exit $code
