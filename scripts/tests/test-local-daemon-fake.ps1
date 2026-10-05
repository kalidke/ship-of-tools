# test-local-daemon-fake.ps1 -- compiles the fake sotd.exe and defines Clear-FakeEnv, New-FakePrefix, Stop-FakeOn. Dot-sourced inside the outer try.

    # ---- 7-8: a fake daemon (C#, compiled once) that can bind late and exit by itself ----
    # Add-Type -OutputAssembly is Windows PowerShell 5.1 only, which is what
    # this file runs under (CI step `shell: powershell`). C# 5 syntax only.
    Write-Host "`n=== 7-8 setup. compile the fake daemon ===" -ForegroundColor Cyan
    if ($null -eq $envSaved) { $envSaved = @{} }
    foreach ($k in @('LOCALAPPDATA', 'FAKE_SOTD_EXIT_AFTER_MS', 'FAKE_SOTD_BIND_DELAY_MS', 'FAKE_SOTD_LEASE_OUTCOME', 'FAKE_SOTD_HELLO_REFUSAL', 'FAKE_SOTD_LOG')) {
        if (-not $envSaved.ContainsKey($k)) { $envSaved[$k] = [Environment]::GetEnvironmentVariable($k) }
    }
    $fakeLocalAppData = Join-Path $root 'fakelocal'
    New-Item -ItemType Directory -Force -Path (Join-Path $fakeLocalAppData 'sot') | Out-Null
    $env:LOCALAPPDATA = $fakeLocalAppData
    $fakeSrc = @'
using System;
using System.IO;
using System.IO.Pipes;
using System.Text;
using System.Threading;

public static class FakeSotd
{
    static string logPath;
    static object logLock = new object();
    static string outcome = "granted";
    static string helloRefusal = "";

    static void Log(string s)
    {
        if (string.IsNullOrEmpty(logPath)) { return; }
        lock (logLock) { File.AppendAllText(logPath, s + "\n"); }
    }

    static int EnvInt(string name, int dflt)
    {
        string v = Environment.GetEnvironmentVariable(name);
        int n;
        if (!string.IsNullOrEmpty(v) && int.TryParse(v, out n)) { return n; }
        return dflt;
    }

    static void Serve(object o)
    {
        NamedPipeServerStream srv = (NamedPipeServerStream)o;
        bool sent = false;
        try
        {
            StreamReader r = new StreamReader(srv, new UTF8Encoding(false));
            string line;
            while ((line = r.ReadLine()) != null)
            {
                sent = true;
                Log(line);
                if (line.Contains("\"op\":\"hello\""))
                {
                    // The daemon's admission (ADR 0049): a hello is answered accepted, or refused with FAKE_SOTD_HELLO_REFUSAL as its code.
                    string reply = helloRefusal.Length == 0
                        ? "{\"session_id\":\"s\",\"revision\":0,\"snapshot_pending\":false}"
                        : "{\"error\":\"refused\",\"code\":\"" + helloRefusal + "\"}";
                    byte[] h = new UTF8Encoding(false).GetBytes("{\"v\":3,\"id\":0,\"kind\":\"res\",\"op\":\"hello\",\"payload\":" + reply + "}\n");
                    srv.Write(h, 0, h.Length);
                    srv.Flush();
                }
                if (line.Contains("\"op\":\"fe.lease\""))
                {
                    byte[] b = new UTF8Encoding(false).GetBytes(
                        "{\"v\":3,\"id\":1,\"kind\":\"res\",\"op\":\"fe.lease\",\"payload\":{\"outcome\":\"" + outcome + "\"}}\n");
                    srv.Write(b, 0, b.Length);
                    srv.Flush();
                }
            }
        }
        catch (Exception) { }
        if (sent) { Log("eof"); }
        try { srv.Dispose(); } catch (Exception) { }
    }

    public static int Main(string[] a)
    {
        string name = null;
        for (int i = 0; i + 1 < a.Length; i++)
        {
            if (a[i] == "--socket") { name = a[i + 1]; }
        }
        if (name == null) { return 2; }
        const string prefix = "\\\\.\\pipe\\";
        if (name.StartsWith(prefix)) { name = name.Substring(prefix.Length); }
        logPath = Environment.GetEnvironmentVariable("FAKE_SOTD_LOG");
        string oc = Environment.GetEnvironmentVariable("FAKE_SOTD_LEASE_OUTCOME");
        if (!string.IsNullOrEmpty(oc)) { outcome = oc; }
        string hr = Environment.GetEnvironmentVariable("FAKE_SOTD_HELLO_REFUSAL");
        if (!string.IsNullOrEmpty(hr)) { helloRefusal = hr; }
        int exitAfter = EnvInt("FAKE_SOTD_EXIT_AFTER_MS", -1);
        int bindDelay = EnvInt("FAKE_SOTD_BIND_DELAY_MS", 0);
        if (exitAfter >= 0)
        {
            Thread t = new Thread(delegate () { Thread.Sleep(exitAfter); Environment.Exit(0); });
            t.IsBackground = true;
            t.Start();
        }
        if (bindDelay > 0) { Thread.Sleep(bindDelay); }
        while (true)
        {
            NamedPipeServerStream srv = new NamedPipeServerStream(
                name, PipeDirection.InOut, NamedPipeServerStream.MaxAllowedServerInstances, PipeTransmissionMode.Byte);
            srv.WaitForConnection();
            Thread w = new Thread(Serve);
            w.IsBackground = true;
            w.Start(srv);
        }
    }
}
'@
    $fakeBinDir = Join-Path $root 'fakebin'
    New-Item -ItemType Directory -Force -Path $fakeBinDir | Out-Null
    $fakeExe = Join-Path $fakeBinDir 'sotd.exe'
    $compiled = $false
    try {
        Add-Type -TypeDefinition $fakeSrc -OutputAssembly $fakeExe -OutputType ConsoleApplication
        $compiled = Test-Path -LiteralPath $fakeExe
    } catch { $compileErr = $_.Exception.Message }
    Check 'the fake daemon compiles' $compiled "Add-Type failed: $compileErr"

    function Clear-FakeEnv {
        foreach ($k in @('FAKE_SOTD_EXIT_AFTER_MS', 'FAKE_SOTD_BIND_DELAY_MS', 'FAKE_SOTD_LEASE_OUTCOME', 'FAKE_SOTD_HELLO_REFUSAL', 'FAKE_SOTD_LOG')) {
            Remove-Item "Env:\$k" -ErrorAction SilentlyContinue
        }
    }
    function New-FakePrefix([string]$Name) {
        $p = Join-Path $root $Name
        New-Fixture -Prefix $p -WithCapsule -SotdSource $fakeExe
        return $p
    }
    function Stop-FakeOn([string]$Pipe) {
        Get-DaemonProcs (Get-PipePath $Pipe) | ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }
    }
