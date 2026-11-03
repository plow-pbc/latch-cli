# Runs a command as a standard user would: this user's token with
# Administrators for deny only, its privileges cut to a standard user's, at
# medium integrity (Windows' "Basic User" level, as `runas /trustlevel:0x20000`
# gives, but waited for, with this console's output and its exit code).
# GitHub's Windows runners sign in as an administrator; the tests run so.
#
#   powershell -ExecutionPolicy Bypass -File scripts/as-standard-user.ps1 PROGRAM [ARGUMENT]...
$ErrorActionPreference = 'Stop'
if ($args.Count -lt 1) { [Console]::Error.WriteLine('usage: as-standard-user.ps1 PROGRAM [ARGUMENT]...'); exit 2 }

Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Text;

public static class LatchStandardUser {
    [DllImport("advapi32.dll", SetLastError = true)]
    static extern bool SaferCreateLevel(uint scope, uint level, uint flags, out IntPtr handle, IntPtr reserved);
    [DllImport("advapi32.dll", SetLastError = true)]
    static extern bool SaferComputeTokenFromLevel(IntPtr level, IntPtr token, out IntPtr output, uint flags, IntPtr reserved);
    [DllImport("advapi32.dll", SetLastError = true)]
    static extern bool SaferCloseLevel(IntPtr level);
    [DllImport("advapi32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    static extern bool ConvertStringSidToSidW(string sid, out IntPtr psid);
    [DllImport("advapi32.dll", SetLastError = true)]
    static extern bool SetTokenInformation(IntPtr token, int infoClass, ref TOKEN_MANDATORY_LABEL info, int length);
    [DllImport("advapi32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    static extern bool CreateProcessAsUserW(IntPtr token, string application, StringBuilder commandLine,
        IntPtr processAttributes, IntPtr threadAttributes, bool inheritHandles, uint flags,
        IntPtr environment, string directory, ref STARTUPINFO startup, out PROCESS_INFORMATION process);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern IntPtr GetStdHandle(int which);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool SetHandleInformation(IntPtr handle, uint mask, uint flags);
    [DllImport("kernel32.dll")]
    static extern uint WaitForSingleObject(IntPtr handle, uint milliseconds);
    [DllImport("kernel32.dll")]
    static extern bool GetExitCodeProcess(IntPtr process, out uint code);
    [DllImport("kernel32.dll")]
    static extern bool CloseHandle(IntPtr handle);

    [StructLayout(LayoutKind.Sequential)]
    struct SID_AND_ATTRIBUTES { public IntPtr Sid; public uint Attributes; }
    [StructLayout(LayoutKind.Sequential)]
    struct TOKEN_MANDATORY_LABEL { public SID_AND_ATTRIBUTES Label; }
    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    struct STARTUPINFO {
        public int cb; public string reserved, desktop, title;
        public int x, y, xSize, ySize, xChars, yChars, fill, flags;
        public short show, reserved2; public IntPtr reserved3, stdin, stdout, stderr;
    }
    [StructLayout(LayoutKind.Sequential)]
    struct PROCESS_INFORMATION { public IntPtr process, thread; public int pid, tid; }

    public static int Run(string commandLine, string directory) {
        IntPtr level, token;
        // SAFER_SCOPEID_USER, SAFER_LEVELID_NORMALUSER, SAFER_LEVEL_OPEN.
        if (!SaferCreateLevel(2, 0x20000, 1, out level, IntPtr.Zero)) throw new Win32Exception();
        bool computed = SaferComputeTokenFromLevel(level, IntPtr.Zero, out token, 0, IntPtr.Zero);
        SaferCloseLevel(level);
        if (!computed) throw new Win32Exception();
        // Medium integrity, a standard user's (S-1-16-8192); SE_GROUP_INTEGRITY.
        IntPtr medium;
        if (!ConvertStringSidToSidW("S-1-16-8192", out medium)) throw new Win32Exception();
        TOKEN_MANDATORY_LABEL label = new TOKEN_MANDATORY_LABEL();
        label.Label.Sid = medium;
        label.Label.Attributes = 0x20;
        int size = Marshal.SizeOf(typeof(TOKEN_MANDATORY_LABEL)) + 12;
        // TokenIntegrityLevel.
        if (!SetTokenInformation(token, 25, ref label, size)) throw new Win32Exception();
        STARTUPINFO startup = new STARTUPINFO();
        startup.cb = Marshal.SizeOf(typeof(STARTUPINFO));
        startup.flags = 0x100;
        startup.stdin = GetStdHandle(-10);
        startup.stdout = GetStdHandle(-11);
        startup.stderr = GetStdHandle(-12);
        foreach (IntPtr handle in new[] { startup.stdin, startup.stdout, startup.stderr }) SetHandleInformation(handle, 1, 1);
        PROCESS_INFORMATION process;
        if (!CreateProcessAsUserW(token, null, new StringBuilder(commandLine), IntPtr.Zero, IntPtr.Zero,
                true, 0, IntPtr.Zero, directory, ref startup, out process)) throw new Win32Exception();
        WaitForSingleObject(process.process, 0xFFFFFFFF);
        uint code;
        GetExitCodeProcess(process.process, out code);
        CloseHandle(process.thread);
        CloseHandle(process.process);
        CloseHandle(token);
        return (int)code;
    }
}
'@

# The command line as Windows' C runtime splits it again.
function Quoted($word) {
  if ($word -notmatch '[\s"]' -and $word) { return $word }
  '"' + ($word -replace '(\\*)"', '$1$1\"' -replace '(\\+)$', '$1$1') + '"'
}
$program = (Get-Command $args[0] -CommandType Application | Select-Object -First 1).Source
$line = (@($program) + @($args | Select-Object -Skip 1) | ForEach-Object { Quoted "$_" }) -join ' '
exit [LatchStandardUser]::Run($line, (Get-Location).Path)
