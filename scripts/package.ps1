<#
.SYNOPSIS
    Checks, builds and tests the tree, then creates dist/ultimatecrt-v<version>-windows-x64.zip with its
    SHA-256 file and a record of the commit it was built from.
.DESCRIPTION
    Run it on a clean tree, after committing. The zip contains one top-level folder with the two
    executables, README.md, CHANGELOG.md, THIRD-PARTY.md (the licences of the dependencies) and
    RUNNING.txt. dist/<name>.commit holds the commit hash so scripts/release.ps1 can refuse a stale zip.
.PARAMETER SkipTests
    Skip fmt, clippy and the tests. Only for quick local packaging, never for a publish.
.PARAMETER AllowDirty
    Package a tree with uncommitted changes. Only for local trials: release.ps1 refuses such a zip.
#>
param([switch]$SkipTests, [switch]$AllowDirty)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue' # progress bars break non-interactive hosts and slow Compress-Archive
$root = Split-Path $PSScriptRoot -Parent
Set-Location $root

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
if ($dirty -and -not $AllowDirty) { throw 'Working tree is not clean: commit first (or use -AllowDirty for a local trial).' }
$commit = (git rev-parse HEAD).Trim()
if ($dirty) { $commit = "$commit-dirty" }
Write-Host "Packaging version $version at $commit"

if (-not $SkipTests) {
    Invoke-Checked 'cargo fmt' { cargo fmt --all --check }
    Invoke-Checked 'cargo clippy' { cargo clippy --all-targets --locked -- -D warnings }
}
Invoke-Checked 'cargo build' { cargo build --release --locked }
if (-not $SkipTests) {
    Invoke-Checked 'cargo test' { cargo test --release --locked }
}

# Licences of everything that is built into or with the binaries (workspace crates excluded).
$meta = (Invoke-Checked 'cargo metadata' { cargo metadata --format-version 1 --locked --filter-platform x86_64-pc-windows-msvc } | Out-String) | ConvertFrom-Json
$members = @($meta.workspace_members)
$used = @{}
foreach ($node in $meta.resolve.nodes) { $used[$node.id] = $true }
$rows = $meta.packages | Where-Object { $used.ContainsKey($_.id) -and ($members -notcontains $_.id) } | Sort-Object name, version
$missing = @($rows | Where-Object { -not $_.license })
if ($missing.Count -gt 0) { throw "Dependencies without a licence field: $(($missing | ForEach-Object { $_.name }) -join ', ')" }
$gpl = @($rows | Where-Object { $_.license -match 'GPL' -and $_.license -notmatch 'LGPL' })
if ($gpl.Count -gt 0) { throw "GPL dependency found: $(($gpl | ForEach-Object { $_.name }) -join ', ') (see DESIGN.md, dependencies)" }
$third = @('# Third-party licences', '',
    'Crates built into or with ultimateCRT, with the licence expression they declare (from `cargo metadata`).', '',
    '| Crate | Version | Licence |', '|---|---|---|')
foreach ($r in $rows) { $third += "| $($r.name) | $($r.version) | $($r.license) |" }
# Upstream notices the crate packages do not carry (Dear ImGui and cimgui, T-52a), from the repository file.
$third += ''
$third += (Get-Content (Join-Path $root 'THIRD-PARTY.md') -Encoding UTF8)

$name = "ultimatecrt-v$version-windows-x64"
$stage = Join-Path $root "dist\$name"
Remove-Item -Recurse -Force "dist\$name", "dist\$name.zip", "dist\$name.zip.sha256", "dist\$name.commit" -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $stage | Out-Null
Copy-Item 'target\release\ultimatecrt.exe', 'target\release\pixel-demo.exe', 'README.md', 'CHANGELOG.md' $stage
[IO.File]::WriteAllText("$stage\THIRD-PARTY.md", (($third -join "`n") + "`n"), (New-Object Text.UTF8Encoding $false))
$running = @"
ultimateCRT $version

1. Start the program to capture (any window; pixel-demo.exe is a demo source).
2. Start:  ultimatecrt.exe --window "title of that window"     (ultimatecrt.exe --help lists the options)
3. Fullscreen over the source, never taking focus:  ultimatecrt.exe --window "title" --fullscreen
   Quit with Ctrl+Alt+Q (Esc in a windowed output).

Needs Windows 11 x64 and a GPU driver with Vulkan 1.3. The executables are not code-signed: SmartScreen or
antivirus tools may warn on first run. Licences of the dependencies: THIRD-PARTY.md.
"@
[IO.File]::WriteAllText("$stage\RUNNING.txt", ($running -replace "`r?`n", "`r`n"), (New-Object Text.UTF8Encoding $false))

# The folder itself goes into the zip, so unzipping never scatters files.
Compress-Archive -Path $stage -DestinationPath "dist\$name.zip"
$hash = (Get-FileHash "dist\$name.zip" -Algorithm SHA256).Hash.ToLower()
[IO.File]::WriteAllText((Join-Path $root "dist\$name.zip.sha256"), "$hash  $name.zip`n", [Text.Encoding]::ASCII)
[IO.File]::WriteAllText((Join-Path $root "dist\$name.commit"), "$commit`n", [Text.Encoding]::ASCII)
Remove-Item -Recurse -Force $stage
Write-Host "Created dist\$name.zip ($hash) from $commit"
