<#
.SYNOPSIS
    Checks, builds and tests the tree, then creates dist/phoxium-v<version>-windows-x64.zip with its
    SHA-256 file and a record of the commit it was built from.
.DESCRIPTION
    Run it on a clean tree, after committing. The zip contains one top-level folder with the two
    executables, LICENSE, README.md, docs/REFERENCE.md (the detailed reference the README links to), CHANGELOG.md,
    THIRD-PARTY.md (the licence texts of every dependency and the
    notices of the bundled C/C++ code and font, from scripts/gen-third-party.ps1), BUILD-INFO.txt (commit,
    toolchain) and RUNNING.txt. dist/<name>.commit holds the commit hash (with a `-dirty` and/or `-untested`
    suffix for a local trial) so scripts/release.ps1 can refuse a stale, dirty or untested zip.

    The executables are taken from the target directory that cargo reports (`cargo metadata`), so a CARGO_TARGET_DIR
    set for the session (scripts/check.ps1 does that) is honoured. The script refuses to package when the
    working tree is dirty or when an executable is older than the sources it was built from (a stale build: only
    the files that can change the executables count, scripts/build-checks.ps1 Test-BinaryInput, so a commit of
    tests or documentation does not trip it); -Force overrides both, for a local trial only.

    The release build (and the release tests, which reuse it) runs with --remap-path-prefix flags for the user
    profile, the repository, CARGO_HOME and the target folder (CARGO_ENCODED_RUSTFLAGS, this script's process
    only; ordinary builds are not affected), so that dependency panic locations do not embed local paths. The
    staged executables are then scanned for the user profile path, the repository path, the user name and the
    machine name (ASCII and UTF-16): any hit fails the packaging, -Force included.

    The tests run with PHOXIUM_REQUIRE_GPU=1: a machine without a usable Vulkan device fails the gate instead of
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
. (Join-Path $PSScriptRoot 'build-checks.ps1')

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
    Invoke-Checked 'build checks self test' { & powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot 'build-checks.ps1') -SelfTest }
}
# D-04: the release build and the release tests (same flags, so the tests reuse the build) remap local paths.
$savedFlags = Set-RemapRustFlags $root (Get-BinaryTargetDir $root)
try {
    Invoke-Checked 'cargo build' { cargo build --release --locked }
    if (-not $SkipTests) {
        # A machine without a usable Vulkan device must fail here, not skip the GPU suite.
        $oldGpu = $env:PHOXIUM_REQUIRE_GPU
        $env:PHOXIUM_REQUIRE_GPU = '1'
        try { Invoke-Checked 'cargo test' { cargo test --release --locked } }
        finally { $env:PHOXIUM_REQUIRE_GPU = $oldGpu }
    }
} finally { Restore-RustFlags $savedFlags }

# The licences of everything that is built into or with the binaries (workspace crates excluded).
$meta = (Invoke-Checked 'cargo metadata' { cargo metadata --format-version 1 --locked --filter-platform x86_64-pc-windows-msvc } | Out-String) | ConvertFrom-Json
$members = @($meta.workspace_members)
$used = @{}
foreach ($node in $meta.resolve.nodes) { $used[$node.id] = $true }
$rows = @($meta.packages | Where-Object { $used.ContainsKey($_.id) -and ($members -notcontains $_.id) } | Sort-Object name, version)
Assert-DeclaredLicences $rows (Join-Path $root 'deny.toml')
if (Get-Command cargo-deny -ErrorAction SilentlyContinue) {
    Invoke-Checked 'cargo deny' { cargo deny --locked check licenses }
    # D-15: the RustSec advisories, with a freshly fetched database (a network failure stops the packaging too:
    # run it again online, or check by hand with `cargo deny --offline check advisories` for a local trial).
    Invoke-Checked 'cargo deny advisories' { cargo deny --locked check advisories }
} else {
    Write-Warning 'cargo-deny is not installed (cargo install cargo-deny --locked): only the declared-licence filter ran.'
}

# The executables come from the target directory cargo reports, not from a fixed target\release.
$targetDir = $meta.target_directory
$exes = @('phoxium.exe') | ForEach-Object { Join-Path $targetDir "release\$_" }
foreach ($e in $exes) { if (-not (Test-Path -LiteralPath $e)) { throw "Missing $e after the build." } }
Write-Host "Executables from $targetDir\release"
foreach ($stale in @(Get-StaleBinaries $root $exes)) {
    $msg = "${stale}: the build is stale, so the zip would not match the commit."
    if ($Force) { Write-Warning "$msg Continuing because of -Force." } else { throw "$msg Rebuild (cargo build --release --locked) or use -Force for a local trial." }
}

$name = "phoxium-v$version-windows-x64"
$stage = Join-Path $root "dist\$name"
foreach ($leaf in @($name, "$name.zip", "$name.zip.sha256", "$name.commit")) {
    $old = Join-Path $root "dist\$leaf"
    try { if ([IO.Directory]::Exists($old)) { [IO.Directory]::Delete($old, $true) } else { [IO.File]::Delete($old) } } catch { }
}
New-Item -ItemType Directory -Force $stage | Out-Null
Copy-Item -LiteralPath $exes -Destination $stage
Copy-Item 'LICENSE' $stage
Copy-Item 'docs\CHANGELOG.md' $stage
# The README sits in docs/ in the repository (links to REFERENCE.md and ../LICENSE) and at the root of the zip, next to
# docs/REFERENCE.md and LICENSE: the two links are rewritten for that place.
$readme = [IO.File]::ReadAllText((Join-Path $root 'docs\README.md'), [Text.Encoding]::UTF8).Replace('](REFERENCE.md', '](docs/REFERENCE.md').Replace('](../LICENSE', '](LICENSE')
[IO.File]::WriteAllText((Join-Path $stage 'README.md'), $readme, (New-Object Text.UTF8Encoding($false)))
New-Item -ItemType Directory -Force (Join-Path $stage 'docs') | Out-Null
Copy-Item 'docs\REFERENCE.md' (Join-Path $stage 'docs')
# The staged executables are byte-for-byte the ones of the target directory.
foreach ($e in $exes) {
    $a = (Get-FileHash -LiteralPath $e -Algorithm SHA256).Hash
    $b = (Get-FileHash -LiteralPath (Join-Path $stage (Split-Path $e -Leaf)) -Algorithm SHA256).Hash
    if ($a -ne $b) { throw "The staged copy of $e differs from the original." }
}
# D-04: no local path, user or machine name in the published executables (no -Force bypass).
Assert-NoPrivateStrings $root @($exes | ForEach-Object { Join-Path $stage (Split-Path $_ -Leaf) }) $targetDir

# Licence texts of every dependency, then the repository notices (C/C++ sources, fonts).
Invoke-Checked 'gen-third-party' { & (Join-Path $PSScriptRoot 'gen-third-party.ps1') -OutFile (Join-Path $stage 'THIRD-PARTY.md') }

# What the binaries were built with (ENV.md, "Record the toolchain of a release").
$glslc = 'glslc'
if ($env:VULKAN_SDK -and (Test-Path (Join-Path $env:VULKAN_SDK 'Bin\glslc.exe'))) { $glslc = Join-Path $env:VULKAN_SDK 'Bin\glslc.exe' }
$rustc = ((Invoke-Checked 'rustc' { rustc -vV }) -join "`n").Trim()
$glslcVersion = 'not found'
try { $glslcVersion = ((& $glslc --version 2>&1) | ForEach-Object { "$_" }) -join "`n" } catch { }
$info = @("Phoxium $version", "commit $commit", '', 'rustc -vV', $rustc, '', 'glslc --version', $glslcVersion.Trim(), '')
[IO.File]::WriteAllText("$stage\BUILD-INFO.txt", (($info -join "`n") -replace "`r?`n", "`r`n"), (New-Object Text.UTF8Encoding $false))

$running = @"
Phoxium $version

1. Start the program to capture (any window; a game must be windowed or borderless, not exclusive fullscreen).
2. Start phoxium.exe (double-click) and choose that window in the list it shows (Enter or double-click).
3. F11: fullscreen over the source, never taking focus. F10: settings (Change source, Detach, every parameter).
   Quit with Ctrl+Alt+End or the close button. Developer options: phoxium.exe --help from a terminal.
   F10 and F11 are registered system-wide while the program runs (no other program gets these keys meanwhile);
   both can be rebound in the settings menu.

Needs Windows 11 x64 and a GPU driver with Vulkan 1.3.

The executables are NOT code-signed. Windows SmartScreen may show "Windows protected your PC" on first run
(More info, then Run anyway), and antivirus tools may warn. To check that the zip is the one that was published,
compare its SHA-256 with the .sha256 file of the release page:
    PowerShell:  (Get-FileHash .\$name.zip -Algorithm SHA256).Hash
    cmd:         certutil -hashfile $name.zip SHA256
The two values must be identical (letters in upper or lower case). The source is on GitHub
(arnaud-sintes/Phoxium) and the executables can be rebuilt from it (README.md, "Building from source").

Phoxium is released under the MIT licence (LICENSE). Licences of the dependencies: THIRD-PARTY.md.
At its first start Phoxium copies the settings folder of the earlier versions (%APPDATA%\ultimateCRT) to
%APPDATA%\Phoxium and leaves the old one in place.
"@
[IO.File]::WriteAllText("$stage\RUNNING.txt", ($running -replace "`r?`n", "`r`n"), (New-Object Text.UTF8Encoding $false))

# The folder itself goes into the zip, so unzipping never scatters files.
# Compress-Archive of Windows PowerShell 5.1 writes backslashes in the entry names (non-standard: Linux unzip and
# other tools mishandle them), so the zip is written with System.IO.Compression and forward-slash entry names.
Add-Type -AssemblyName System.IO.Compression
Add-Type -AssemblyName System.IO.Compression.FileSystem
$zipPath = Join-Path $root "dist\$name.zip"
try { [IO.File]::Delete($zipPath) } catch { }
$stageFull = (Resolve-Path $stage).Path.TrimEnd('\')
$zipStream = [IO.File]::Open($zipPath, [IO.FileMode]::CreateNew)
try {
    $zipArchive = New-Object IO.Compression.ZipArchive($zipStream, [IO.Compression.ZipArchiveMode]::Create)
    try {
        foreach ($file in (Get-ChildItem -LiteralPath $stageFull -Recurse -File | Sort-Object FullName)) {
            $entryName = $name + '/' + ($file.FullName.Substring($stageFull.Length + 1) -replace '\\', '/')
            [void][IO.Compression.ZipFileExtensions]::CreateEntryFromFile($zipArchive, $file.FullName, $entryName, [IO.Compression.CompressionLevel]::Optimal)
        }
    } finally { $zipArchive.Dispose() }
} finally { $zipStream.Dispose() }
$hash = (Get-FileHash "dist\$name.zip" -Algorithm SHA256).Hash.ToLower()
[IO.File]::WriteAllText((Join-Path $root "dist\$name.zip.sha256"), "$hash  $name.zip`n", [Text.Encoding]::ASCII)
[IO.File]::WriteAllText((Join-Path $root "dist\$name.commit"), "$commit`n", [Text.Encoding]::ASCII)
try { [IO.Directory]::Delete($stage, $true) } catch { }
Write-Host "Created dist\$name.zip ($hash) from $commit"
if ($commit -match '-(dirty|untested)') { Write-Warning 'This zip is a local trial: release.ps1 refuses it.' }
