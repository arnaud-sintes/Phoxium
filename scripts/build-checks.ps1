<#
.SYNOPSIS
    Build and binary checks shared by scripts/package.ps1 and the live check scripts (check-capture.ps1,
    check-pixel-demo.ps1). Dot-source it; run it with -SelfTest to test the pure parts.
.DESCRIPTION
    - Which tracked files can change the executables (Test-BinaryInput), the newest of them
      (Get-NewestBinaryInput), and a refusal of binaries older than that (Assert-FreshBinaries). Tests,
      examples, documentation, scripts and the font tooling (subset.py, OFL.txt) are not inputs: a commit that only
      touches them leaves the executables valid (review D-09).
    - The release build's path remapping (Get-RemapRustFlags, Get-TrimCFlags, Set-RemapRustFlags): dependency panic
      locations and file!() strings, and the assertion messages of the C++ code (Dear ImGui), otherwise embed the
      cargo registry path under the user profile (review D-04).
    - The scan of a binary for private strings (Find-PrivateStrings, Get-PrivateNeedles): the user profile path,
      the repository path, the user name and the machine name, in ASCII and in UTF-16LE.
    - The cargo target folder of the binaries (Get-BinaryTargetDir) and the quoting of process arguments for
      Start-Process (ConvertTo-ProcessArgument; Windows PowerShell 5.1 joins -ArgumentList unquoted, review D-10).
.PARAMETER SelfTest
    Run the tests of the pure functions and exit with a non-zero code on a failure.
#>
param([switch]$SelfTest)

$ErrorActionPreference = 'Stop'

# Whether the tracked file $Path (repository-relative, / or \) can change what the executables contain: crate
# sources, build scripts and manifests, embedded assets (not their tooling or notes), shaders, the parameter table,
# the presets, the workspace manifests and the toolchain.
function Test-BinaryInput([string]$Path) {
    $p = $Path -replace '\\', '/'
    if ($p -match '^crates/[^/]+/(src/|build\.rs$|Cargo\.toml$)') { return $true }
    if ($p -match '^(crates/[^/]+/)?assets/') { return ($p -notmatch '\.(py|txt|md)$') }
    if ($p -match '^(shaders|params|presets)/') { return $true }
    return ($p -match '^(Cargo\.toml|Cargo\.lock|rust-toolchain\.toml)$')
}

# The newest write time among the tracked binary inputs of the repository at $Root: @{ Time; File }, or $null.
function Get-NewestBinaryInput([string]$Root) {
    $listing = & git -C $Root -c core.quotepath=false ls-files -- crates shaders params presets assets Cargo.toml Cargo.lock rust-toolchain.toml
    if ($LASTEXITCODE -ne 0) { throw "git ls-files failed in $Root" }
    $newest = $null
    foreach ($f in @($listing | Where-Object { $_ -and (Test-BinaryInput $_) })) {
        $p = Join-Path $Root $f
        if (-not (Test-Path -LiteralPath $p)) { continue }
        $t = (Get-Item -LiteralPath $p).LastWriteTimeUtc
        if (-not $newest -or $t -gt $newest.Time) { $newest = @{ Time = $t; File = $f } }
    }
    return $newest
}

# The executables of $Exes that are older than the newest binary input of $Root, as messages (empty: fresh).
function Get-StaleBinaries([string]$Root, [string[]]$Exes) {
    $newest = Get-NewestBinaryInput $Root
    $stale = @()
    foreach ($e in $Exes) {
        $built = (Get-Item -LiteralPath $e).LastWriteTimeUtc
        if ($newest -and $built -lt $newest.Time) {
            $stale += "$e (built $($built.ToString('s'))Z) is older than the source $($newest.File) (changed $($newest.Time.ToString('s'))Z)"
        }
    }
    return $stale
}

# Throws when an executable is missing or older than its sources (rebuild with cargo build --release --locked).
function Assert-FreshBinaries([string]$Root, [string[]]$Exes) {
    foreach ($e in $Exes) { if (-not (Test-Path -LiteralPath $e)) { throw "missing binary: $e (cargo build --release --locked)" } }
    $stale = @(Get-StaleBinaries $Root $Exes)
    if ($stale.Count -gt 0) { throw (($stale -join "`n") + "`nThe build is stale: rebuild (cargo build --release --locked).") }
}

# The cargo target folder holding release\*.exe: CARGO_TARGET_DIR when set, else what `cargo metadata` reports for
# the workspace at $Root (cargo's own default or configuration).
function Get-BinaryTargetDir([string]$Root) {
    if ($env:CARGO_TARGET_DIR) { return $env:CARGO_TARGET_DIR }
    Push-Location $Root
    try {
        $previous = $ErrorActionPreference
        $ErrorActionPreference = 'Continue'
        try { $json = (& cargo metadata --format-version 1 --no-deps 2>$null) | Out-String } finally { $ErrorActionPreference = $previous }
        if ($LASTEXITCODE -ne 0) { throw 'cargo metadata failed' }
    } finally { Pop-Location }
    return ($json | ConvertFrom-Json).target_directory
}

# One argument for Start-Process -ArgumentList (Windows PowerShell 5.1 joins the list with spaces and quotes
# nothing): quoted by the rules of the C runtime's command-line parser when it is empty or holds a space, a tab
# or a double quote (backslashes before a quote, or before the closing quote, are doubled).
function ConvertTo-ProcessArgument([string]$Arg) {
    if ($Arg.Length -gt 0 -and $Arg -notmatch '[\s"]') { return $Arg }
    $sb = New-Object System.Text.StringBuilder
    [void]$sb.Append('"')
    $slashes = 0
    foreach ($c in $Arg.ToCharArray()) {
        if ($c -eq '\') { $slashes++; continue }
        if ($c -eq '"') { [void]$sb.Append('\' * (2 * $slashes + 1)); [void]$sb.Append('"') }
        else { [void]$sb.Append('\' * $slashes); [void]$sb.Append($c) }
        $slashes = 0
    }
    [void]$sb.Append('\' * (2 * $slashes))
    [void]$sb.Append('"')
    return $sb.ToString()
}

# The --remap-path-prefix flags of a release build of the repository at $Root with the target folder $TargetDir:
# the user profile to ~, the repository to src, CARGO_HOME (the registry and git sources) to cargo, the target
# folder to target. rustc applies the LAST matching rule, so the more specific folders come later. No -Z flag.
function Get-RemapRustFlags([string]$Root, [string]$TargetDir) {
    $cargoHome = $env:CARGO_HOME
    if (-not $cargoHome) { $cargoHome = Join-Path $env:USERPROFILE '.cargo' }
    $rules = @()
    if ($env:USERPROFILE) { $rules += , @($env:USERPROFILE, '~') }
    $rules += , @($Root, 'src')
    $rules += , @($cargoHome, 'cargo')
    if ($TargetDir) { $rules += , @($TargetDir, 'target') }
    $flags = @()
    foreach ($r in $rules) {
        $from = ([IO.Path]::GetFullPath($r[0])).TrimEnd('\', '/')
        $flags += "--remap-path-prefix=$from=$($r[1])"
    }
    return $flags
}

# The MSVC flags that trim CARGO_HOME (the registry sources) and the target folder $TargetDir (sources generated in
# OUT_DIR) from __FILE__ in the C/C++ code built by the cc crate (Dear ImGui: its assertion messages are wide
# strings with the full source path; --remap-path-prefix does not reach them and the documented /pathmap option
# only rewrites debug information, measured). `/d1trimfile:` is an undocumented MSVC front-end option, used for
# reproducible builds; the private-string scan is the guard if it stops working. A folder holding a space is left
# out (the cc crate splits CFLAGS on whitespace).
function Get-TrimCFlags([string]$TargetDir) {
    $cargoHome = $env:CARGO_HOME
    if (-not $cargoHome) { $cargoHome = Join-Path $env:USERPROFILE '.cargo' }
    $flags = @()
    foreach ($d in @($cargoHome, $TargetDir)) {
        if (-not $d) { continue }
        $full = ([IO.Path]::GetFullPath($d)).TrimEnd('\', '/')
        if ($full -match '\s') { continue }
        $flags += "/d1trimfile:$full\"
    }
    return $flags
}

# Sets CARGO_ENCODED_RUSTFLAGS (one flag per 0x1F-separated field, so paths with spaces stay whole) to the flags
# of RUSTFLAGS, if any, plus the remapping flags, and adds Get-TrimCFlags to CFLAGS and CXXFLAGS. Returns the
# previous values for Restore-RustFlags. Note: like any RUSTFLAGS, this replaces the `build.rustflags` of a cargo
# configuration (the repository has none).
function Set-RemapRustFlags([string]$Root, [string]$TargetDir) {
    $saved = @{
        RUSTFLAGS = $env:RUSTFLAGS; CARGO_ENCODED_RUSTFLAGS = $env:CARGO_ENCODED_RUSTFLAGS
        CFLAGS = $env:CFLAGS; CXXFLAGS = $env:CXXFLAGS
    }
    if ($env:CARGO_ENCODED_RUSTFLAGS) { throw 'CARGO_ENCODED_RUSTFLAGS is already set: unset it for the packaging build.' }
    $flags = @()
    if ($env:RUSTFLAGS) { $flags += @($env:RUSTFLAGS -split '\s+' | Where-Object { $_ }) }
    $flags += Get-RemapRustFlags $Root $TargetDir
    $env:CARGO_ENCODED_RUSTFLAGS = $flags -join [char]0x1f
    $env:RUSTFLAGS = $null
    $trim = @(Get-TrimCFlags $TargetDir)
    if ($trim.Count -lt 2) {
        Write-Warning 'CARGO_HOME or the target folder holds a space: those C++ source paths are not trimmed (the private-string scan decides).'
    }
    $env:CFLAGS = (@($env:CFLAGS) + $trim | Where-Object { $_ }) -join ' '
    $env:CXXFLAGS = (@($env:CXXFLAGS) + $trim | Where-Object { $_ }) -join ' '
    return $saved
}

function Restore-RustFlags($Saved) {
    foreach ($k in 'RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS', 'CFLAGS', 'CXXFLAGS') {
        [Environment]::SetEnvironmentVariable($k, $Saved[$k], 'Process') # $null removes it
    }
}

# The strings no shipped binary may contain: the user profile path, the repository path and the target folder
# $TargetDir if given (both separators), the user name and the machine name (3 characters at least,
# case-insensitive matching).
function Get-PrivateNeedles([string]$Root, [string]$TargetDir) {
    $n = @()
    foreach ($p in @($env:USERPROFILE, $Root, $TargetDir)) {
        if (-not $p) { continue }
        $full = ([IO.Path]::GetFullPath($p)).TrimEnd('\', '/')
        $n += $full
        $n += ($full -replace '\\', '/')
    }
    foreach ($s in @($env:USERNAME, $env:COMPUTERNAME)) { if ($s) { $n += $s } }
    return @($n | Where-Object { $_.Length -ge 3 } | Select-Object -Unique)
}

# The needles of $Needles found in the bytes of $File, each as "<needle> (ASCII|UTF-16)"; empty when clean.
function Find-PrivateStrings([string]$File, [string[]]$Needles) {
    $bytes = [IO.File]::ReadAllBytes($File)
    $views = @(
        @{ Name = 'ASCII'; Text = [Text.Encoding]::GetEncoding(28591).GetString($bytes) },
        @{ Name = 'UTF-16'; Text = [Text.Encoding]::Unicode.GetString($bytes) }
    )
    if ($bytes.Length -gt 1) { $views += @{ Name = 'UTF-16'; Text = [Text.Encoding]::Unicode.GetString($bytes, 1, $bytes.Length - 1) } }
    $found = @()
    foreach ($needle in $Needles) {
        foreach ($v in $views) {
            if ($v.Text.IndexOf($needle, [StringComparison]::OrdinalIgnoreCase) -ge 0) {
                $found += "$needle ($($v.Name))"
                break
            }
        }
    }
    return $found
}

# Throws when one of $Exes contains a private string (see Get-PrivateNeedles).
function Assert-NoPrivateStrings([string]$Root, [string[]]$Exes, [string]$TargetDir) {
    $needles = Get-PrivateNeedles $Root $TargetDir
    $bad = @()
    foreach ($e in $Exes) {
        foreach ($f in @(Find-PrivateStrings $e $needles)) { $bad += "$(Split-Path $e -Leaf): $f" }
    }
    if ($bad.Count -gt 0) {
        throw ("The executables contain private strings (user profile, repository path, user or machine name):`n  " + ($bad -join "`n  ") + "`nBuild with the remapping of scripts/build-checks.ps1 (package.ps1 does).")
    }
}

function Invoke-BuildChecksSelfTest {
    function Check([bool]$ok, [string]$what) { if (-not $ok) { $script:selfFail += $what } }
    $script:selfFail = @()
    # Binary inputs (review D-09).
    foreach ($p in @('crates/ultimatecrt/src/app.rs', 'crates/crt-model/src/lib.rs', 'crates/ultimatecrt/build.rs',
            'crates/pixel-demo/Cargo.toml', 'crates/ultimatecrt/assets/fonts/Inter-Regular.ttf', 'assets/icon/icon.ico',
            'shaders/include/geometry.glsl', 'params/composite.toml', 'presets/tubes/01.toml', 'Cargo.lock',
            'Cargo.toml', 'rust-toolchain.toml', 'crates\ultimatecrt\src\main.rs')) {
        Check (Test-BinaryInput $p) "binary input: $p"
    }
    foreach ($p in @('crates/ultimatecrt/tests/stage_geometry.rs', 'crates/appres/examples/gen_icon.rs',
            'crates/ultimatecrt/assets/fonts/subset.py', 'crates/ultimatecrt/assets/fonts/OFL.txt', 'assets/icon/README.md',
            'scripts/package.ps1', 'README.md', 'CHANGELOG.md', 'deny.toml', 'crates/ultimatecrt/benches/x.rs')) {
        Check (-not (Test-BinaryInput $p)) "not a binary input: $p"
    }
    # Argument quoting (review D-10): what the C runtime parser reads back.
    $cases = @(
        @('plain', 'plain'), @('C:\Temp\a b\known.png', '"C:\Temp\a b\known.png"'), @('', '""'),
        @('say "hi"', '"say \"hi\""'), @('C:\dir with space\', '"C:\dir with space\\"'), @('a\b', 'a\b')
    )
    foreach ($c in $cases) {
        $got = ConvertTo-ProcessArgument $c[0]
        Check ($got -ceq $c[1]) "quote '$($c[0])': got '$got', want '$($c[1])'"
    }
    # Remapping: the registry under the profile maps to cargo, the profile to ~, later rules more specific.
    $saved = @{ USERPROFILE = $env:USERPROFILE; CARGO_HOME = $env:CARGO_HOME }
    try {
        $env:USERPROFILE = 'C:\Users\Some One'; $env:CARGO_HOME = $null
        $flags = @(Get-RemapRustFlags 'D:\code\repo' 'D:\code\repo\target')
        Check ($flags.Count -eq 4) "remap count $($flags.Count)"
        Check ($flags[0] -eq '--remap-path-prefix=C:\Users\Some One=~') "remap profile: $($flags[0])"
        Check ($flags[1] -eq '--remap-path-prefix=D:\code\repo=src') "remap root: $($flags[1])"
        Check ($flags[2] -eq '--remap-path-prefix=C:\Users\Some One\.cargo=cargo') "remap cargo: $($flags[2])"
        Check ($flags[3] -eq '--remap-path-prefix=D:\code\repo\target=target') "remap target: $($flags[3])"
        # The scan: ASCII and UTF-16 needles found, case-insensitive; a clean file passes.
        $needles = @(Get-PrivateNeedles 'D:\code\repo')
        Check ($needles -contains 'C:\Users\Some One' -and $needles -contains 'C:/Users/Some One' -and $needles -contains 'D:\code\repo') "needles: $($needles -join ', ')"
        # The C/C++ trim: CARGO_HOME with a trailing separator; none for a path with a space.
        $trim = @(Get-TrimCFlags 'D:\code\target')
        Check ($trim.Count -eq 1 -and $trim[0] -eq '/d1trimfile:D:\code\target\') "trim flags with a space: $($trim -join ' ')"
        $env:USERPROFILE = 'C:\Users\someone'
        $trim = @(Get-TrimCFlags 'D:\code\target\')
        Check (($trim -join ' ') -eq '/d1trimfile:C:\Users\someone\.cargo\ /d1trimfile:D:\code\target\') "trim flags: $($trim -join ' ')"
        $env:CARGO_HOME = 'E:\cargo-home\'
        $trim = @(Get-TrimCFlags '')
        Check (($trim -join ' ') -eq '/d1trimfile:E:\cargo-home\') "trim flags, CARGO_HOME: $($trim -join ' ')"
        Check (@(Get-PrivateNeedles 'D:\code\repo' 'D:\t\shared') -contains 'D:/t/shared') 'target folder needle'
    } finally { $env:USERPROFILE = $saved.USERPROFILE; $env:CARGO_HOME = $saved.CARGO_HOME }
    $tmp = Join-Path ([IO.Path]::GetTempPath()) ('ucrt-build-checks-' + [guid]::NewGuid().ToString('N').Substring(0, 8))
    New-Item -ItemType Directory $tmp | Out-Null
    try {
        $needles = @('C:\Users\Some One', 'secretpc')
        $ascii = Join-Path $tmp 'ascii.bin'
        [IO.File]::WriteAllBytes($ascii, [byte[]](0, 1, 2) + [Text.Encoding]::ASCII.GetBytes('panic at c:\users\some one\.cargo\registry\x.rs') + [byte[]](0))
        $wide = Join-Path $tmp 'wide.bin'
        [IO.File]::WriteAllBytes($wide, [byte[]](7) + [Text.Encoding]::Unicode.GetBytes('host SECRETPC'))
        $clean = Join-Path $tmp 'clean.bin'
        [IO.File]::WriteAllBytes($clean, [Text.Encoding]::ASCII.GetBytes('cargo\registry\src\index.crates.io-1\x.rs ~\y'))
        $a = @(Find-PrivateStrings $ascii $needles)
        Check ($a.Count -eq 1 -and $a[0] -like 'C:\Users\Some One (ASCII)') "ascii scan: $($a -join '; ')"
        $w = @(Find-PrivateStrings $wide $needles)
        Check ($w.Count -eq 1 -and $w[0] -eq 'secretpc (UTF-16)') "utf-16 scan (odd offset): $($w -join '; ')"
        $c = @(Find-PrivateStrings $clean $needles)
        Check ($c.Count -eq 0) "clean scan: $($c -join '; ')"
    } finally { Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue }
    return $script:selfFail
}

if ($SelfTest) {
    $f = @(Invoke-BuildChecksSelfTest)
    if ($f.Count -eq 0) { Write-Host 'build-checks SelfTest: PASS'; exit 0 }
    $f | ForEach-Object { Write-Host "FAIL: $_" }
    Write-Host "build-checks SelfTest: FAIL ($($f.Count))"
    exit 1
}
