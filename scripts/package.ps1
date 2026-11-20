<#
.SYNOPSIS
    Checks, builds and tests the tree, then creates dist/ultimatecrt-v<version>-windows-x64.zip with its
    SHA-256 file and a record of the commit it was built from.
.DESCRIPTION
    Run it on a clean tree, after committing. The zip contains one top-level folder with the two
    executables, LICENSE, README.md, CHANGELOG.md, THIRD-PARTY.md (the licence texts of every dependency and the
    notices of the bundled C/C++ code and font, from scripts/gen-third-party.ps1), BUILD-INFO.txt (commit,
    toolchain) and RUNNING.txt. dist/<name>.commit holds the commit hash (with a `-dirty` and/or `-untested`
    suffix for a local trial) so scripts/release.ps1 can refuse a stale, dirty or untested zip.

    The executables are taken from the target directory that cargo reports (`cargo metadata`), so a CARGO_TARGET_DIR
    set for the session (scripts/check.ps1 does that) is honoured. The script refuses to package when the
    working tree is dirty or when an executable is older than the sources it was built from (a stale build);
    -Force overrides both, for a local trial only.

    The tests run with UCRT_REQUIRE_GPU=1: a machine without a usable Vulkan device fails the gate instead of
    skipping the whole GPU suite. cargo-deny (deny.toml) runs when installed; the declared-licence filter of
    scripts/licence-policy.ps1 always runs.
.PARAMETER SkipTests
    Skip fmt, clippy, the tests and the self tests. Only for quick local packaging: the zip is recorded as
    `<commit>-untested` and release.ps1 refuses it.
.PARAMETER AllowDirty
    Package a tree with uncommitted changes. Only for local trials: release.ps1 refuses such a zip.
.PARAMETER Force
    Package although the tree is dirty or an executable looks older than its sources. Local trials only.
#>
param([switch]$SkipTests, [switch]$AllowDirty, [switch]$Force)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue' # progress bars break non-interactive hosts and slow Compress-Archive
$root = Split-Path $PSScriptRoot -Parent
Set-Location $root
. (Join-Path $PSScriptRoot 'licence-policy.ps1')

# Windows PowerShell 5.1 turns anything a native tool writes to stderr (cargo progress) into an error
# under 'Stop', so the exit code is checked instead. Output is returned to the caller.
function Invoke-Checked([string]$what, [scriptblock]$block) {
    $previous = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try { & $block } finally { $ErrorActionPreference = $previous }
    if ($LASTEXITCODE -ne 0) { throw "$what failed with exit code $LASTEXITCODE" }
}

function Get-WorkspaceVersion {
    $inPackage = $false
    foreach ($line in Get-Content 'Cargo.toml') {
        if ($line -match '^\[(.+)\]\s*$') { $inPackage = ($Matches[1] -eq 'workspace.package'); continue }
        if ($inPackage -and $line -match '^\s*version\s*=\s*"([^"]+)"') { return $Matches[1] }
    }
    throw 'No version in [workspace.package] of Cargo.toml.'
}

# The newest write time among the tracked files a build depends on (sources, shaders, parameter table, presets,
# assets, manifests), and the file it is. Documentation and scripts do not enter the executables.
function Get-NewestBuildInput {
    $listing = Invoke-Checked 'git ls-files' { git -c core.quotepath=false ls-files -- crates shaders params presets assets Cargo.toml Cargo.lock rust-toolchain.toml }
    $newest = $null
    foreach ($f in @($listing | Where-Object { $_ })) {
        $p = Join-Path $root $f
        if (-not (Test-Path -LiteralPath $p)) { continue }
        $t = (Get-Item -LiteralPath $p).LastWriteTimeUtc
        if (-not $newest -or $t -gt $newest.Time) { $newest = @{ Time = $t; File = $f } }
    }
    return $newest
}

$version = Get-WorkspaceVersion
$dirty = [bool](git status --porcelain)
if ($dirty -and -not ($AllowDirty -or $Force)) {
    throw 'Working tree is not clean: commit first (or use -AllowDirty for a local trial). A release needs a clean tree.'
}
$commit = (git rev-parse HEAD).Trim()
if ($dirty) { $commit = "$commit-dirty" }
if ($SkipTests) { $commit = "$commit-untested" }
Write-Host "Packaging version $version at $commit"

if (-not $SkipTests) {
    Invoke-Checked 'cargo fmt' { cargo fmt --all --check }
    Invoke-Checked 'cargo clippy' { cargo clippy --all-targets --locked -- -D warnings }
    # Cheap self tests of the tooling the gate relies on.
    Invoke-Checked 'licence policy self test' { & powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot 'licence-policy.ps1') -SelfTest }
    Invoke-Checked 'impact self test' { & powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot 'impact.ps1') -SelfTest }
}
Invoke-Checked 'cargo build' { cargo build --release --locked }
if (-not $SkipTests) {
    # A machine without a usable Vulkan device must fail here, not skip the GPU suite.
    $oldGpu = $env:UCRT_REQUIRE_GPU
    $env:UCRT_REQUIRE_GPU = '1'
    try { Invoke-Checked 'cargo test' { cargo test --release --locked } }
    finally { $env:UCRT_REQUIRE_GPU = $oldGpu }
}

# The licences of everything that is built into or with the binaries (workspace crates excluded).
$meta = (Invoke-Checked 'cargo metadata' { cargo metadata --format-version 1 --locked --filter-platform x86_64-pc-windows-msvc } | Out-String) | ConvertFrom-Json
$members = @($meta.workspace_members)
$used = @{}
foreach ($node in $meta.resolve.nodes) { $used[$node.id] = $true }
$rows = @($meta.packages | Where-Object { $used.ContainsKey($_.id) -and ($members -notcontains $_.id) } | Sort-Object name, version)
Assert-DeclaredLicences $rows (Join-Path $root 'deny.toml')
if (Get-Command cargo-deny -ErrorAction SilentlyContinue) {
    Invoke-Checked 'cargo deny' { cargo deny --locked check licenses }
} else {
    Write-Warning 'cargo-deny is not installed (cargo install cargo-deny --locked): only the declared-licence filter ran.'
}

# The executables come from the target directory cargo reports, not from a fixed target\release.
$targetDir = $meta.target_directory
$exes = @('ultimatecrt.exe', 'pixel-demo.exe') | ForEach-Object { Join-Path $targetDir "release\$_" }
foreach ($e in $exes) { if (-not (Test-Path -LiteralPath $e)) { throw "Missing $e after the build." } }
Write-Host "Executables from $targetDir\release"
$newest = Get-NewestBuildInput
foreach ($e in $exes) {
    $built = (Get-Item -LiteralPath $e).LastWriteTimeUtc
    if ($newest -and $built -lt $newest.Time) {
        $msg = "$e (built $($built.ToString('s'))Z) is older than the source $($newest.File) (changed $($newest.Time.ToString('s'))Z): the build is stale, so the zip would not match the commit."
        if ($Force) { Write-Warning "$msg Continuing because of -Force." } else { throw "$msg Rebuild (cargo build --release --locked) or use -Force for a local trial." }
    }
}

$name = "ultimatecrt-v$version-windows-x64"
$stage = Join-Path $root "dist\$name"
Remove-Item -Recurse -Force "dist\$name", "dist\$name.zip", "dist\$name.zip.sha256", "dist\$name.commit" -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $stage | Out-Null
Copy-Item -LiteralPath $exes -Destination $stage
Copy-Item 'LICENSE', 'README.md', 'CHANGELOG.md' $stage
# The staged executables are byte-for-byte the ones of the target directory.
foreach ($e in $exes) {
    $a = (Get-FileHash -LiteralPath $e -Algorithm SHA256).Hash
    $b = (Get-FileHash -LiteralPath (Join-Path $stage (Split-Path $e -Leaf)) -Algorithm SHA256).Hash
    if ($a -ne $b) { throw "The staged copy of $e differs from the original." }
}

# Licence texts of every dependency, then the repository notices (C/C++ sources, fonts).
Invoke-Checked 'gen-third-party' { & (Join-Path $PSScriptRoot 'gen-third-party.ps1') -OutFile (Join-Path $stage 'THIRD-PARTY.md') }

# What the binaries were built with (ENV.md, "Record the toolchain of a release").
$glslc = 'glslc'
if ($env:VULKAN_SDK -and (Test-Path (Join-Path $env:VULKAN_SDK 'Bin\glslc.exe'))) { $glslc = Join-Path $env:VULKAN_SDK 'Bin\glslc.exe' }
$rustc = ((Invoke-Checked 'rustc' { rustc -vV }) -join "`n").Trim()
$glslcVersion = 'not found'
try { $glslcVersion = ((& $glslc --version 2>&1) | ForEach-Object { "$_" }) -join "`n" } catch { }
$info = @("ultimateCRT $version", "commit $commit", '', 'rustc -vV', $rustc, '', 'glslc --version', $glslcVersion.Trim(), '')
[IO.File]::WriteAllText("$stage\BUILD-INFO.txt", (($info -join "`n") -replace "`r?`n", "`r`n"), (New-Object Text.UTF8Encoding $false))

$running = @"
ultimateCRT $version

1. Start the program to capture (any window; pixel-demo.exe is a demo source).
2. Start ultimatecrt.exe (double-click) and choose that window in the list it shows (Enter or double-click).
3. F11: fullscreen over the source, never taking focus. F10: settings (Change source, Detach, every parameter).
   Quit with Ctrl+Alt+End or the close button. Developer options: ultimatecrt.exe --help from a terminal.
   F10 and F11 are registered system-wide while the program runs (no other program gets these keys meanwhile);
   both can be rebound in the settings menu.

Needs Windows 11 x64 and a GPU driver with Vulkan 1.3.

The executables are NOT code-signed. Windows SmartScreen may show "Windows protected your PC" on first run
(More info, then Run anyway), and antivirus tools may warn. To check that the zip is the one that was published,
compare its SHA-256 with the .sha256 file of the release page:
    PowerShell:  (Get-FileHash .\$name.zip -Algorithm SHA256).Hash
    cmd:         certutil -hashfile $name.zip SHA256
The two values must be identical (letters in upper or lower case). The source is on GitHub
(arnaud-sintes/ultimateCRT) and the executables can be rebuilt from it (README.md, "Building").

ultimateCRT is released under the MIT licence (LICENSE). Licences of the dependencies: THIRD-PARTY.md.
"@
[IO.File]::WriteAllText("$stage\RUNNING.txt", ($running -replace "`r?`n", "`r`n"), (New-Object Text.UTF8Encoding $false))

# The folder itself goes into the zip, so unzipping never scatters files.
Compress-Archive -Path $stage -DestinationPath "dist\$name.zip"
$hash = (Get-FileHash "dist\$name.zip" -Algorithm SHA256).Hash.ToLower()
[IO.File]::WriteAllText((Join-Path $root "dist\$name.zip.sha256"), "$hash  $name.zip`n", [Text.Encoding]::ASCII)
[IO.File]::WriteAllText((Join-Path $root "dist\$name.commit"), "$commit`n", [Text.Encoding]::ASCII)
Remove-Item -Recurse -Force $stage
Write-Host "Created dist\$name.zip ($hash) from $commit"
if ($commit -match '-(dirty|untested)') { Write-Warning 'This zip is a local trial: release.ps1 refuses it.' }
