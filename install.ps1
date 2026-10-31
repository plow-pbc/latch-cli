# Installs latch on Windows from its releases on releases.plow.co: the newest
# one, or the tag given first. Options go on to the release's
# install-binary.ps1. Once installed, `latch update` installs newer builds.
# --client-only installs latch alone, for a machine that only asks hosts.
#
#   powershell -ExecutionPolicy Bypass -File install.ps1 [TAG|latest] [--bin-dir DIR] [--state-dir DIR] [--browser chrome|camoufox]... [--no-browser] [--client-only] [--no-login] [--non-interactive] [--no-modify-path]
#
# Nothing in the release's archive runs until SHA256SUMS is signed with the
# release key and lists the archive, as install.sh checks on Linux. Windows
# has no tool for minisign's signatures, so this script checks them itself:
# the Ed25519 signature of the file's BLAKE2b-512 hash, the same check
# openssl makes in install.sh.
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$usage = 'usage: install.ps1 [TAG|latest] [--bin-dir DIR] [--state-dir DIR] [--browser chrome|camoufox]... [--no-browser] [--client-only] [--no-login] [--non-interactive] [--no-modify-path]'
$version = 'latest'
$options = @($args)
if ($options.Count -gt 0) {
  if ($options[0] -in '-h', '--help') { $usage; exit 0 }
  if ($options[0] -notlike '-*') { $version = $options[0]; $options = @($options | Select-Object -Skip 1) }
}
$releases = 'https://releases.plow.co/latch-cli'
# The minisign public key (id DA695E5C4D2D36B9) releases are signed with, as
# latch update checks them: nothing installs unless the release's SHA256SUMS,
# and the latest.json that names it, are signed with this key. A signed
# latest.json names a promoted release, not necessarily the newest: an older
# one, served again, still checks out.
$releaseKey = 'RWS5Ni1NXF5p2g7U9HvReN/6scLD8gpzhEZVW//t8Dkn4SyhqAdExFTu'
function Die($text) { [Console]::Error.WriteLine($text); exit 1 }
if ($env:PROCESSOR_ARCHITECTURE -ne 'AMD64') { Die 'unsupported OS/architecture' }
$target = 'x86_64-pc-windows-msvc'

Add-Type -ReferencedAssemblies System.Numerics -TypeDefinition @'
using System;
using System.Numerics;
using System.Security.Cryptography;

// minisign's signature check: Ed25519 (RFC 8032) over the file's BLAKE2b-512
// hash (RFC 7693), from their specifications.
public static class LatchMinisign {
    static readonly ulong[] IV = {
        0x6a09e667f3bcc908UL, 0xbb67ae8584caa73bUL, 0x3c6ef372fe94f82bUL, 0xa54ff53a5f1d36f1UL,
        0x510e527fade682d1UL, 0x9b05688c2b3e6c1fUL, 0x1f83d9abfb41bd6bUL, 0x5be0cd19137e2179UL };
    static readonly byte[][] SIGMA = {
        new byte[] { 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15 },
        new byte[] { 14, 10, 4, 8, 9, 15, 13, 6, 1, 12, 0, 2, 11, 7, 5, 3 },
        new byte[] { 11, 8, 12, 0, 5, 2, 15, 13, 10, 14, 3, 6, 7, 1, 9, 4 },
        new byte[] { 7, 9, 3, 1, 13, 12, 11, 14, 2, 6, 5, 10, 4, 0, 15, 8 },
        new byte[] { 9, 0, 5, 7, 2, 4, 10, 15, 14, 1, 11, 12, 6, 8, 3, 13 },
        new byte[] { 2, 12, 6, 10, 0, 11, 8, 3, 4, 13, 7, 5, 15, 14, 1, 9 },
        new byte[] { 12, 5, 1, 15, 14, 13, 4, 10, 0, 7, 6, 3, 9, 2, 8, 11 },
        new byte[] { 13, 11, 7, 14, 12, 1, 3, 9, 5, 0, 15, 4, 8, 6, 2, 10 },
        new byte[] { 6, 15, 14, 9, 11, 3, 0, 8, 12, 2, 13, 7, 1, 4, 10, 5 },
        new byte[] { 10, 2, 8, 4, 7, 6, 1, 5, 15, 11, 9, 14, 3, 12, 13, 0 } };

    static ulong Rot(ulong x, int n) { return (x >> n) | (x << (64 - n)); }

    static void G(ulong[] v, int a, int b, int c, int d, ulong x, ulong y) {
        v[a] = v[a] + v[b] + x; v[d] = Rot(v[d] ^ v[a], 32);
        v[c] = v[c] + v[d]; v[b] = Rot(v[b] ^ v[c], 24);
        v[a] = v[a] + v[b] + y; v[d] = Rot(v[d] ^ v[a], 16);
        v[c] = v[c] + v[d]; v[b] = Rot(v[b] ^ v[c], 63);
    }

    static void Compress(ulong[] h, byte[] block, ulong count, bool last) {
        ulong[] m = new ulong[16];
        for (int i = 0; i < 16; i++) m[i] = BitConverter.ToUInt64(block, i * 8);
        ulong[] v = new ulong[16];
        for (int i = 0; i < 8; i++) { v[i] = h[i]; v[i + 8] = IV[i]; }
        v[12] ^= count;
        if (last) v[14] = ~v[14];
        for (int r = 0; r < 12; r++) {
            byte[] s = SIGMA[r % 10];
            G(v, 0, 4, 8, 12, m[s[0]], m[s[1]]);
            G(v, 1, 5, 9, 13, m[s[2]], m[s[3]]);
            G(v, 2, 6, 10, 14, m[s[4]], m[s[5]]);
            G(v, 3, 7, 11, 15, m[s[6]], m[s[7]]);
            G(v, 0, 5, 10, 15, m[s[8]], m[s[9]]);
            G(v, 1, 6, 11, 12, m[s[10]], m[s[11]]);
            G(v, 2, 7, 8, 13, m[s[12]], m[s[13]]);
            G(v, 3, 4, 9, 14, m[s[14]], m[s[15]]);
        }
        for (int i = 0; i < 8; i++) h[i] ^= v[i] ^ v[i + 8];
    }

    public static byte[] Blake2b512(byte[] data) {
        ulong[] h = (ulong[])IV.Clone();
        h[0] ^= 0x01010000UL ^ 64UL;
        byte[] block = new byte[128];
        int offset = 0;
        ulong count = 0;
        while (data.Length - offset > 128) {
            Array.Copy(data, offset, block, 0, 128);
            count += 128;
            Compress(h, block, count, false);
            offset += 128;
        }
        Array.Clear(block, 0, 128);
        Array.Copy(data, offset, block, 0, data.Length - offset);
        count += (ulong)(data.Length - offset);
        Compress(h, block, count, true);
        byte[] digest = new byte[64];
        for (int i = 0; i < 8; i++) Array.Copy(BitConverter.GetBytes(h[i]), 0, digest, i * 8, 8);
        return digest;
    }

    static readonly BigInteger P = BigInteger.Pow(2, 255) - 19;
    static readonly BigInteger L = BigInteger.Pow(2, 252) + BigInteger.Parse("27742317777372353535851937790883648493");
    static readonly BigInteger D = Mod(-121665 * Inv(121666));
    static readonly BigInteger SqrtM1 = BigInteger.ModPow(2, (P - 1) / 4, P);

    static BigInteger Mod(BigInteger x) { BigInteger r = x % P; return r.Sign < 0 ? r + P : r; }
    static BigInteger Inv(BigInteger x) { return BigInteger.ModPow(Mod(x), P - 2, P); }

    static BigInteger Little(byte[] bytes, int offset, int length) {
        byte[] unsigned = new byte[length + 1];
        Array.Copy(bytes, offset, unsigned, 0, length);
        return new BigInteger(unsigned);
    }

    // Points in extended coordinates: X, Y, Z, T with x = X/Z, y = Y/Z, xy = T/Z.
    static BigInteger[] Add(BigInteger[] p, BigInteger[] q) {
        BigInteger a = Mod((p[1] - p[0]) * (q[1] - q[0]));
        BigInteger b = Mod((p[1] + p[0]) * (q[1] + q[0]));
        BigInteger c = Mod(2 * p[3] * q[3] * D);
        BigInteger d = Mod(2 * p[2] * q[2]);
        BigInteger e = b - a, f = d - c, g = d + c, h = b + a;
        return new BigInteger[] { Mod(e * f), Mod(g * h), Mod(f * g), Mod(e * h) };
    }

    static BigInteger[] Times(BigInteger scalar, BigInteger[] point) {
        BigInteger[] sum = { 0, 1, 1, 0 };
        while (scalar.Sign > 0) {
            if (!scalar.IsEven) sum = Add(sum, point);
            point = Add(point, point);
            scalar >>= 1;
        }
        return sum;
    }

    static byte[] Encode(BigInteger[] point) {
        BigInteger z = Inv(point[2]);
        BigInteger x = Mod(point[0] * z), y = Mod(point[1] * z);
        byte[] bytes = new byte[32];
        byte[] little = y.ToByteArray();
        Array.Copy(little, bytes, Math.Min(32, little.Length));
        if (!x.IsEven) bytes[31] |= 0x80;
        return bytes;
    }

    static BigInteger[] Decode(byte[] bytes) {
        byte[] copy = (byte[])bytes.Clone();
        int sign = copy[31] >> 7;
        copy[31] &= 0x7f;
        BigInteger y = Little(copy, 0, 32);
        if (y >= P) return null;
        BigInteger x2 = Mod((y * y - 1) * Inv(D * y * y + 1));
        BigInteger x = BigInteger.ModPow(x2, (P + 3) / 8, P);
        if (Mod(x * x - x2) != 0) x = Mod(x * SqrtM1);
        if (Mod(x * x - x2) != 0) return null;
        if (x.IsZero && sign == 1) return null;
        if ((x.IsEven ? 0 : 1) != sign) x = P - x;
        return new BigInteger[] { x, y, 1, Mod(x * y) };
    }

    static BigInteger[] Base() {
        byte[] y = new byte[32];
        byte[] little = Mod(4 * Inv(5)).ToByteArray();
        Array.Copy(little, y, Math.Min(32, little.Length));
        return Decode(y);
    }

    public static bool Ed25519(byte[] key, byte[] message, byte[] signature) {
        if (key.Length != 32 || signature.Length != 64) return false;
        BigInteger[] a = Decode(key);
        if (a == null) return false;
        BigInteger s = Little(signature, 32, 32);
        if (s >= L) return false;
        byte[] r = new byte[32];
        Array.Copy(signature, r, 32);
        byte[] input = new byte[64 + message.Length];
        Array.Copy(r, 0, input, 0, 32);
        Array.Copy(key, 0, input, 32, 32);
        Array.Copy(message, 0, input, 64, message.Length);
        BigInteger k;
        using (SHA512 sha = SHA512.Create()) k = Little(sha.ComputeHash(input), 0, 64) % L;
        BigInteger[] ka = Times(k, a);
        BigInteger[] minus = { Mod(-ka[0]), ka[1], ka[2], Mod(-ka[3]) };
        byte[] check = Encode(Add(Times(s, Base()), minus));
        for (int i = 0; i < 32; i++) if (check[i] != r[i]) return false;
        return true;
    }

    // A minisign signature (its second line, base64) of `data`, by the
    // public key `key` (base64): the prehashed kind ("ED") minisign makes,
    // from that same key.
    public static bool Signed(string key, string signature, byte[] data) {
        byte[] k = Convert.FromBase64String(key), s = Convert.FromBase64String(signature);
        if (k.Length != 42 || s.Length != 74 || k[0] != 'E' || k[1] != 'd' || s[0] != 'E' || s[1] != 'D') return false;
        for (int i = 2; i < 10; i++) if (k[i] != s[i]) return false;
        byte[] publicKey = new byte[32], bytes = new byte[64];
        Array.Copy(k, 10, publicKey, 0, 32);
        Array.Copy(s, 10, bytes, 0, 64);
        return Ed25519(publicKey, Blake2b512(data), bytes);
    }
}
'@

$tmp = Join-Path ([IO.Path]::GetTempPath()) ('latch-install-' + [guid]::NewGuid().ToString('N'))
# The release's files, apart from where its archive is unpacked: nothing the
# archive carries can stand in for one of them.
$release = Join-Path $tmp 'release'
$unpacked = Join-Path $tmp 'unpacked'
New-Item -ItemType Directory $release, $unpacked | Out-Null
try {
  function Download($path) {
    $file = Join-Path $release ($path -replace '.*/', '')
    try { Invoke-WebRequest -UseBasicParsing -Uri "$releases/$path" -OutFile $file }
    catch { Die "could not download $path" }
  }
  function Signed($name) {
    $file = Join-Path $release $name
    $lines = @(Get-Content "$file.minisig")
    if ($lines.Count -lt 2 -or -not [LatchMinisign]::Signed($releaseKey, $lines[1], [IO.File]::ReadAllBytes($file))) {
      Die "$name is not signed with latch's release key; nothing installed"
    }
  }
  # `latest` is the release latest.json names: the newest promoted one.
  if ($version -eq 'latest') {
    Download 'latest.json'
    Download 'latest.json.minisig'
    $version = (Get-Content (Join-Path $release 'latest.json') -Raw | ConvertFrom-Json).tag
  }
  if ($version -notmatch '^v[0-9]+\.[0-9]+\.[0-9]+(-[a-zA-Z0-9.-]+)?$') { Die 'expected a release tag such as v0.1.0-1327, or latest' }
  $archive = "latch-$version-$target.zip"
  foreach ($file in $archive, 'SHA256SUMS', 'SHA256SUMS.minisig') { Download "releases/$version/$file" }
  Signed 'SHA256SUMS'
  if (Test-Path (Join-Path $release 'latest.json')) { Signed 'latest.json' }
  # Only the selected archive is checked; other targets need not be downloaded.
  $listed = Get-Content (Join-Path $release 'SHA256SUMS') | Where-Object { ($_ -split '\s+')[1] -eq $archive } | Select-Object -First 1
  if (-not $listed) { Die "SHA256SUMS does not list $archive; nothing installed" }
  $hash = (Get-FileHash -Algorithm SHA256 (Join-Path $release $archive)).Hash.ToLower()
  if ($hash -ne ($listed -split '\s+')[0].ToLower()) { Die "$archive does not match SHA256SUMS; nothing installed" }
  Write-Output "${archive}: OK"
  Expand-Archive -Path (Join-Path $release $archive) -DestinationPath $unpacked
  $setup = Join-Path $unpacked "latch-$version-$target"
  if ($options -contains '--client-only') {
    Write-Output 'Binary defaults to %LOCALAPPDATA%\Programs\latch (override with --bin-dir); installing latch alone, for asking hosts.'
  } else {
    Write-Output 'Binary defaults to %LOCALAPPDATA%\Programs\latch (override with --bin-dir); gog and browsers use ~\.latch\plugins (override with --state-dir).'
  }
  & powershell.exe -NoProfile -ExecutionPolicy Bypass -File (Join-Path $setup 'install-binary.ps1') @options (Join-Path $setup 'latch.exe')
  if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
  & (Join-Path $setup 'latch.exe') --version
} finally {
  Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
