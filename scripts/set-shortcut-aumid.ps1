# set-shortcut-aumid.ps1 — stamp an explicit Application User Model ID onto a
# .lnk so a window that declares the SAME AUMID merges into this shortcut's
# taskbar button instead of opening a second one.
#
# Why this exists: the Ship of Tools launcher shortcut runs powershell.exe
# (launch-sot.ps1), which spawns the real window (sot.exe, from a staged copy).
# Windows groups taskbar buttons by AUMID; sot.exe sets its AUMID explicitly in
# main.rs (SetCurrentProcessExplicitAppUserModelID). This script sets the
# MATCHING System.AppUserModel.ID on the shortcut so the two are one button.
# Keep the default in sync with APP_USER_MODEL_ID in rust/frontend/src/main.rs.
#
# The WScript.Shell COM can't write shell properties, so this drops to the
# shortcut's IPropertyStore (PKEY_AppUserModel_ID) via a small COM-interop shim.
#
#   pwsh -File scripts\set-shortcut-aumid.ps1 -LnkPath "C:\path\to\App.lnk"
#
# Note: Windows caches taskbar-pin metadata. After changing a *pinned*
# shortcut's AUMID, restart Explorer (or unpin + re-pin) for it to take effect.

[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$LnkPath,
    [string]$Aumid = 'ShipOfTools.Sot'
)

$ErrorActionPreference = 'Stop'

if (-not (Test-Path $LnkPath)) {
    Write-Error "shortcut not found: $LnkPath"
    exit 1
}
$LnkPath = (Resolve-Path $LnkPath).Path

Add-Type -Namespace SotShortcut -Name Aumid -MemberDefinition @'
[ComImport, Guid("00021401-0000-0000-C000-000000000046")]
private class CShellLink { }

[ComImport, InterfaceType(ComInterfaceType.InterfaceIsIUnknown),
 Guid("0000010b-0000-0000-C000-000000000046")]
private interface IPersistFile {
    void GetClassID(out Guid pClassID);
    [PreserveSig] int IsDirty();
    void Load([MarshalAs(UnmanagedType.LPWStr)] string pszFileName, int dwMode);
    void Save([MarshalAs(UnmanagedType.LPWStr)] string pszFileName, [MarshalAs(UnmanagedType.Bool)] bool fRemember);
    void SaveCompleted([MarshalAs(UnmanagedType.LPWStr)] string pszFileName);
    void GetCurFile([MarshalAs(UnmanagedType.LPWStr)] out string ppszFileName);
}

[StructLayout(LayoutKind.Sequential)]
private struct PROPERTYKEY { public Guid fmtid; public uint pid; }

[StructLayout(LayoutKind.Explicit)]
private struct PROPVARIANT { [FieldOffset(0)] public ushort vt; [FieldOffset(8)] public IntPtr p; }

[ComImport, InterfaceType(ComInterfaceType.InterfaceIsIUnknown),
 Guid("886d8eeb-8cf2-4446-8d02-cdba1dbdcf99")]
private interface IPropertyStore {
    void GetCount(out uint cProps);
    void GetAt(uint iProp, out PROPERTYKEY pkey);
    void GetValue(ref PROPERTYKEY key, out PROPVARIANT pv);
    void SetValue(ref PROPERTYKEY key, ref PROPVARIANT pv);
    void Commit();
}

[DllImport("ole32.dll")] private static extern int PropVariantClear(ref PROPVARIANT pvar);

public static void Set(string shortcutPath, string appId) {
    const ushort VT_LPWSTR = 31;
    const int STGM_READWRITE = 2;
    object o = new CShellLink();
    try {
        IPersistFile pf = (IPersistFile)o;
        pf.Load(shortcutPath, STGM_READWRITE);
        IPropertyStore store = (IPropertyStore)o;
        PROPERTYKEY key = new PROPERTYKEY {
            fmtid = new Guid("9F4C2855-9F79-4B39-A8D0-E1D42DE1D5F3"), pid = 5
        };
        PROPVARIANT pv = new PROPVARIANT { vt = VT_LPWSTR, p = Marshal.StringToCoTaskMemUni(appId) };
        store.SetValue(ref key, ref pv);
        store.Commit();
        pf.Save(shortcutPath, true);
        PropVariantClear(ref pv);
    } finally {
        Marshal.ReleaseComObject(o);
    }
}
'@

[SotShortcut.Aumid]::Set($LnkPath, $Aumid)

# Read it back to confirm.
$sh = New-Object -ComObject Shell.Application
$dir = Split-Path $LnkPath; $name = Split-Path $LnkPath -Leaf
$item = $sh.NameSpace($dir).ParseName($name)
$got = $item.ExtendedProperty("System.AppUserModel.ID")
Write-Host "Set AUMID '$got' on $LnkPath"
