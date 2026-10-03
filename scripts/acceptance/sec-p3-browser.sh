#!/usr/bin/env bash
# Security premise P3 (git-bash on a Windows box): unelevated, the owner check finds each browser's loopback
# connection and its owning account. PASS: every line reads same-account=True and every lookup= is under 5 ms.
# Opens browser tabs; read-only apart from its own temp file. Do not paste SIDs anywhere public.
P="$(cygpath -u "$TEMP")/sot-owner-probe.ps1"
cat > "$P" <<'EOF'
param([switch]$Other)
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Security.Principal;
public static class SotOwner {
  [DllImport("iphlpapi.dll")] static extern uint GetExtendedTcpTable(IntPtr t, ref int size, bool order, int af, int cls, uint reserved);
  [DllImport("kernel32.dll", SetLastError=true)] static extern IntPtr OpenProcess(uint access, bool inherit, uint pid);
  [DllImport("kernel32.dll")] static extern bool CloseHandle(IntPtr h);
  [DllImport("advapi32.dll", SetLastError=true)] static extern bool OpenProcessToken(IntPtr p, uint access, out IntPtr tok);
  [DllImport("advapi32.dll", SetLastError=true)] static extern bool GetTokenInformation(IntPtr tok, int cls, IntPtr info, int len, out int ret);
  static int Port(int v) { return ((v & 0xFF) << 8) | ((v >> 8) & 0xFF); }
  public static uint OwningPid(int lport, int rport) {
    int size = 0; GetExtendedTcpTable(IntPtr.Zero, ref size, false, 2, 4, 0); size += 4096;
    IntPtr buf = Marshal.AllocHGlobal(size);
    try {
      uint rc = GetExtendedTcpTable(buf, ref size, false, 2, 4, 0);
      if (rc != 0) throw new Exception("GetExtendedTcpTable rc=" + rc);
      int n = Marshal.ReadInt32(buf);
      for (int i = 0; i < n; i++) {
        IntPtr row = IntPtr.Add(buf, 4 + i * 24);
        if (Marshal.ReadInt32(row, 4) == 0x0100007F && Port(Marshal.ReadInt32(row, 8)) == lport &&
            Marshal.ReadInt32(row, 12) == 0x0100007F && Port(Marshal.ReadInt32(row, 16)) == rport)
          return (uint)Marshal.ReadInt32(row, 20);
      }
      return 0;
    } finally { Marshal.FreeHGlobal(buf); }
  }
  public static string SidOf(uint pid) {
    IntPtr h = OpenProcess(0x1000, false, pid);
    if (h == IntPtr.Zero) return "OpenProcess error " + Marshal.GetLastWin32Error();
    try {
      IntPtr tok;
      if (!OpenProcessToken(h, 0x0008, out tok)) return "OpenProcessToken error " + Marshal.GetLastWin32Error();
      try {
        int len; GetTokenInformation(tok, 1, IntPtr.Zero, 0, out len);
        IntPtr info = Marshal.AllocHGlobal(len);
        try {
          if (!GetTokenInformation(tok, 1, info, len, out len)) return "GetTokenInformation error " + Marshal.GetLastWin32Error();
          return new SecurityIdentifier(Marshal.ReadIntPtr(info)).Value;
        } finally { Marshal.FreeHGlobal(info); }
      } finally { CloseHandle(tok); }
    } finally { CloseHandle(h); }
  }
}
'@
$me = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
function Probe($label, $waitMs, [scriptblock]$open) {
  $l = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0); $l.Start(); $port = $l.LocalEndpoint.Port
  try {
    & $open "http://127.0.0.1:$port/sot-owner-probe"
    $t = $l.AcceptTcpClientAsync()
    if (-not $t.Wait($waitMs)) { "${label}: no connection"; return }
    $c = $t.Result; $rp = $c.Client.RemoteEndPoint.Port
    $sw = [Diagnostics.Stopwatch]::StartNew()
    $owner = [SotOwner]::OwningPid($rp, $port)
    $sid = if ($owner) { [SotOwner]::SidOf($owner) } else { 'no table row' }
    $ms = [math]::Round($sw.Elapsed.TotalMilliseconds, 2)
    $name = if ($owner) { (Get-Process -Id $owner -ErrorAction SilentlyContinue).ProcessName } else { '-' }
    "${label}: pid=$owner ($name) same-account=$($sid -eq $me) lookup=${ms}ms"
    if ($sid -ne $me -and $sid -notmatch '^S-1-') { "  answer: $sid" }
    $s = $c.GetStream(); $b = [Text.Encoding]::ASCII.GetBytes("HTTP/1.1 200 OK`r`nContent-Length: 2`r`nConnection: close`r`n`r`nok")
    $s.Write($b, 0, $b.Length); $c.Close()
  } finally { $l.Stop() }
}
if ($Other) {
  Probe 'other-account' 120000 { param($u) "Within 2 minutes, as the OTHER account run:  curl.exe -s -m 5 $u" | Out-Host }
} else {
  Probe 'curl' 30000 { param($u) Start-Process curl.exe -ArgumentList '-s','-m','5',$u -WindowStyle Hidden }
  Probe 'default-browser' 30000 { param($u) Start-Process $u }
  foreach ($b in 'msedge','chrome','firefox') {
    $reg = "SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths\$b.exe"
    if ((Test-Path "HKLM:\$reg") -or (Test-Path "HKCU:\$reg")) { Probe $b 30000 { param($u) Start-Process "$b.exe" $u } }
  }
}
EOF
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "$(cygpath -w "$P")" "$@"
