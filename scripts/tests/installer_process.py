"""Process containment and bounded timeout evidence for installer-publish-guard.py.

Snapshot PIDs are diagnostic data only. Termination uses our spawn's process group or our Windows job handle.
Unix cleanup after an abnormal installer exit is not guaranteed: installer steps take their own groups. Rely on an
enclosing lane container where available; abnormal-exit containment on macOS remains unproven.
"""

import ctypes
import json
import locale
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import uuid


RUNNER_ENV = os.environ.copy()


if os.name == "nt":
    from ctypes import wintypes

    class BasicLimits(ctypes.Structure):
        _fields_ = [("process_time", ctypes.c_longlong), ("job_time", ctypes.c_longlong),
                    ("flags", wintypes.DWORD), ("min_working_set", ctypes.c_size_t),
                    ("max_working_set", ctypes.c_size_t), ("active_processes", wintypes.DWORD),
                    ("affinity", ctypes.c_size_t), ("priority", wintypes.DWORD), ("scheduling", wintypes.DWORD)]

    class ExtendedLimits(ctypes.Structure):
        _fields_ = [("basic", BasicLimits), ("io_counters", ctypes.c_ulonglong * 6),
                    ("process_memory", ctypes.c_size_t), ("job_memory", ctypes.c_size_t),
                    ("peak_process_memory", ctypes.c_size_t), ("peak_job_memory", ctypes.c_size_t)]

    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel.CreateJobObjectW.argtypes = [ctypes.c_void_p, wintypes.LPCWSTR]
    kernel.CreateJobObjectW.restype = wintypes.HANDLE
    kernel.OpenJobObjectW.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.LPCWSTR]
    kernel.OpenJobObjectW.restype = wintypes.HANDLE
    kernel.SetInformationJobObject.argtypes = [wintypes.HANDLE, ctypes.c_int, ctypes.c_void_p, wintypes.DWORD]
    kernel.SetInformationJobObject.restype = wintypes.BOOL
    kernel.AssignProcessToJobObject.argtypes = [wintypes.HANDLE, wintypes.HANDLE]
    kernel.AssignProcessToJobObject.restype = wintypes.BOOL
    kernel.GetCurrentProcess.restype = wintypes.HANDLE
    kernel.CloseHandle.argtypes = [wintypes.HANDLE]
    kernel.CloseHandle.restype = wintypes.BOOL

    def checked(value):
        if not value:
            raise ctypes.WinError(ctypes.get_last_error())
        return value

    class WindowsJob:
        def __init__(self):
            self.name = "Local\\sot-installer-guard-" + uuid.uuid4().hex
            self.handle = checked(kernel.CreateJobObjectW(None, self.name))
            limits = ExtendedLimits()
            limits.basic.flags = 0x2000  # JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE; no breakaway.
            try:
                checked(kernel.SetInformationJobObject(self.handle, 9, ctypes.byref(limits), ctypes.sizeof(limits)))
            except BaseException:
                self.close()
                raise

        def close(self):
            if self.handle:
                checked(kernel.CloseHandle(self.handle))
                self.handle = None

    def job_child(name, argv):
        # Join before starting the real command: it cannot escape in a spawn/assignment race. The parent keeps the
        # only lasting job handle; neither this trampoline nor the command inherits it. Nested jobs require Windows 8+.
        handle = checked(kernel.OpenJobObjectW(1, False, name))  # JOB_OBJECT_ASSIGN_PROCESS
        try:
            checked(kernel.AssignProcessToJobObject(handle, kernel.GetCurrentProcess()))
        finally:
            checked(kernel.CloseHandle(handle))
        return subprocess.call(argv)


def read_output(path):
    # A separate open has its own file position: reading progress must not move the child's writer or chase a stream
    # that is still growing. A file also avoids Windows communicate() hiding partial output until after termination.
    with open(path, "rb") as source:
        data = source.read(os.fstat(source.fileno()).st_size)
    return data.decode(locale.getpreferredencoding(False), errors="replace").replace("\r\n", "\n").replace("\r", "\n")


def capture(argv, cwd, env, input_text=None, *, timeout, on_timeout=None):
    """Bound the wait and cleanup attempt; Unix's outer group cannot contain abnormal installer-exit leftovers."""
    with tempfile.TemporaryDirectory(prefix="installer-process-", dir=cwd) as scratch:
        output_path = Path(scratch) / "output"
        with open(output_path, "wb", buffering=0) as output:
            command = [str(a) for a in argv]
            job = WindowsJob() if os.name == "nt" else None
            child = None
            try:
                if job:
                    command = [sys.executable, "-I", "-S", "-B", str(Path(__file__).resolve()), "--job-child", job.name, *command]
                child = subprocess.Popen(command, cwd=cwd, env=env, text=True,
                                         stdin=subprocess.DEVNULL if input_text is None else subprocess.PIPE,
                                         stdout=output, stderr=subprocess.STDOUT, start_new_session=job is None)
                try:
                    child.communicate(input=input_text, timeout=timeout)
                    return subprocess.CompletedProcess(argv, child.returncode, read_output(output_path))
                except subprocess.TimeoutExpired as error:
                    error.output = read_output(output_path)
                    if on_timeout:
                        on_timeout(child.pid, error)
                    raise
            finally:
                if job:
                    job.close()
                if child is not None:
                    try:
                        if child.returncode is None:
                            if job is None:
                                # Cooperative TERM lets the installer end its recorded step groups. If it died
                                # abnormally, those groups can outlive this outer group; no PID-based repair here.
                                try:
                                    os.killpg(child.pid, signal.SIGTERM)
                                except ProcessLookupError:
                                    pass
                            try:
                                child.communicate(timeout=15 if job is None else 5)
                            except subprocess.TimeoutExpired:
                                if job is None:
                                    try:
                                        os.killpg(child.pid, signal.SIGKILL)
                                    except ProcessLookupError:
                                        pass
                                else:
                                    # Only our Popen handle: startup may have stalled before joining the job.
                                    child.kill()
                                try:
                                    child.wait(timeout=5)
                                except subprocess.TimeoutExpired as error:
                                    raise RuntimeError(f"owned process root {child.pid} did not reap within 5 s after container termination") from error
                    finally:
                        for stream in (child.stdin, child.stdout):
                            if stream is not None:
                                stream.close()


def descendants(root_pid, cwd):
    """CIM/ps use the runner's full environment. Walk downward for display; these PIDs never feed termination."""
    try:
        if os.name == "nt":
            powershell = Path(RUNNER_ENV["SYSTEMROOT"]) / "System32/WindowsPowerShell/v1.0/powershell.exe"
            script = r'''
$ErrorActionPreference = 'Stop'
$all = @(Get-CimInstance Win32_Process -Property ProcessId,ParentProcessId,CreationDate,CommandLine -OperationTimeoutSec 5)
$seen = @{ ROOT_PID = $true }
do {
    $changed = $false
    foreach ($process in $all) {
        if ($seen.ContainsKey([int]$process.ParentProcessId) -and -not $seen.ContainsKey([int]$process.ProcessId)) {
            $seen[[int]$process.ProcessId] = $true
            $changed = $true
        }
    }
} while ($changed)
$now = [DateTime]::UtcNow
@($all | Where-Object { $seen.ContainsKey([int]$_.ProcessId) } | ForEach-Object {
    [pscustomobject]@{
        pid = $_.ProcessId; parent = $_.ParentProcessId
        elapsed_seconds = $(if ($null -ne $_.CreationDate) { [Math]::Round(($now - $_.CreationDate.ToUniversalTime()).TotalSeconds, 3) } else { $null })
        command = $_.CommandLine
    }
}) | ConvertTo-Json -Compress
'''.replace("ROOT_PID", str(root_pid))
            result = capture([powershell, "-NoLogo", "-NoProfile", "-NonInteractive", "-Command", script],
                             cwd, RUNNER_ENV, timeout=10)
            if result.returncode:
                return f"CIM descendant listing failed (exit {result.returncode}):\n{result.stdout}"
            return result.stdout.strip() or "CIM returned no remaining root or descendants"
        result = capture(["ps", "-ww", "-axo", "pid=,ppid=,etime=,command="], cwd, RUNNER_ENV, timeout=10)
        if result.returncode:
            return f"ps descendant listing failed (exit {result.returncode})"
        rows = []
        for line in result.stdout.splitlines():
            parts = line.split(None, 3)
            if len(parts) == 4:
                rows.append(dict(pid=int(parts[0]), parent=int(parts[1]), elapsed=parts[2], command=parts[3]))
        seen = {root_pid}
        while True:
            found = {row["pid"] for row in rows if row["parent"] in seen}
            if found <= seen:
                break
            seen.update(found)
        return "\n".join(json.dumps(row) for row in rows if row["pid"] in seen) or "ps returned no remaining root or descendants"
    except Exception as error:
        return f"descendant listing unavailable within its bound: {type(error).__name__}: {error}"


if __name__ == "__main__":
    if os.name != "nt" or len(sys.argv) < 4 or sys.argv[1] != "--job-child":
        raise SystemExit("only the Windows job trampoline is executable")
    raise SystemExit(job_child(sys.argv[2], sys.argv[3:]))
