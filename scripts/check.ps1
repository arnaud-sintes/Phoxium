<#
.SYNOPSIS
    Local validation gate. By default it runs the impact plan of scripts\impact.ps1 (only the checks and
    tests the change needs); -Full runs everything: fmt check, clippy (-D warnings), all tests with the
    GPU required, licence check, and an optional live smoke run.
.DESCRIPTION
    Without -Full the change (committed since the merge base with exec/integration, uncommitted,
    untracked) is analysed and only the impacted checks run (ENV.md, "Test policy"). The plan falls back
    to the full gate by itself for build configuration, unknown files and anything it cannot place. Use
    -Full before a merge to main, a push or a release, and whenever unsure.

    With -Full it exits 0 only when every step passes. The licence check runs `cargo deny check licenses`
    (deny.toml) when cargo-deny is installed, and always runs the declared-licence check of cargo metadata
    (same policy as scripts/package.ps1: a licence field is required, GPL is refused).
.PARAMETER Smoke
    Also run the live smoke test: `ultimatecrt --pattern ramp --exit-after 10 --validation`. It opens
    an output window for 10 s, so it is opt-in. It fails on a non-zero exit or on any validation line
    on stderr.
.PARAMETER SkipTests
    Skip `cargo test` (fmt, clippy and licences still run). Full gate only.
.PARAMETER Full
    Run the whole gate (the behaviour before the impact analysis existed).
.PARAMETER Base
    Git ref the impact analysis diffs against (default: merge base with exec/integration, else HEAD~1).
#>
param([switch]$Smoke, [switch]$SkipTests, [switch]$Full, [string]$Base)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$root = Split-Path $PSScriptRoot -Parent
Set-Location $root
# Same shared build cache and no incremental cache as scripts\impact.ps1 (ENV.md, Test policy).
if (-not $env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR = 'C:\projects\personal\ultimateCRT-wt\target-shared' }
if (-not $env:CARGO_INCREMENTAL) { $env:CARGO_INCREMENTAL = '0' }

# Windows PowerShell 5.1 turns native stderr into errors under 'Stop': check the exit code instead.
function Invoke-Checked([string]$what, [scriptblock]$block) {
    $previous = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try { & $block } finally { $ErrorActionPreference = $previous }
    if ($LASTEXITCODE -ne 0) { throw "$what failed with exit code $LASTEXITCODE" }
}

function Step([string]$name) { Write-Host "== $name" -ForegroundColor Cyan }

# The declared-licence check of cargo metadata (same policy as scripts/package.ps1).
function Invoke-DeclaredLicences {
    Step 'declared licences'
    $meta = (Invoke-Checked 'cargo metadata' { cargo metadata --format-version 1 --locked --filter-platform x86_64-pc-windows-msvc } | Out-String) | ConvertFrom-Json
    $members = @($meta.workspace_members)
    $used = @{}
    foreach ($node in $meta.resolve.nodes) { $used[$node.id] = $true }
    $rows = @($meta.packages | Where-Object { $used.ContainsKey($_.id) -and ($members -notcontains $_.id) })
    $missing = @($rows | Where-Object { -not $_.license })
    if ($missing.Count -gt 0) { throw "Dependencies without a licence field: $(($missing | ForEach-Object { $_.name }) -join ', ')" }
    $gpl = @($rows | Where-Object { $_.license -match 'GPL' -and $_.license -notmatch 'LGPL' })
    if ($gpl.Count -gt 0) { throw "GPL dependency found: $(($gpl | ForEach-Object { $_.name }) -join ', ') (see DESIGN.md 11)" }
    Write-Host "declared licences OK ($($rows.Count) crates)"
}

function Invoke-Smoke {
    Step 'live smoke (opens a window for 10 s)'
    Invoke-Checked 'cargo build' { cargo build --locked -p ultimatecrt }
    $targetDir = $env:CARGO_TARGET_DIR
    if (-not $targetDir) { $targetDir = Join-Path $root 'target' }
    $exe = Join-Path $targetDir 'debug\ultimatecrt.exe'
    $errFile = [IO.Path]::GetTempFileName()
    try {
        $p = Start-Process -FilePath $exe -ArgumentList '--pattern', 'ramp', '--exit-after', '10', '--validation' `
            -RedirectStandardError $errFile -NoNewWindow -Wait -PassThru
        $err = @(Get-Content $errFile)
        $bad = @($err | Where-Object { $_ -match 'validation|VUID|\[(ERROR|WARNING)' })
        if ($bad.Count -gt 0) { $bad | ForEach-Object { Write-Host $_ }; throw "smoke run printed $($bad.Count) validation line(s)" }
        if ($p.ExitCode -ne 0) { $err | ForEach-Object { Write-Host $_ }; throw "smoke run exited with code $($p.ExitCode)" }
    } finally { Remove-Item $errFile -ErrorAction SilentlyContinue }
    Write-Host 'smoke OK'
}

# Default: the impact plan (it decides FULL by itself when the change needs everything).
if (-not $Full) {
    Step 'impact analysis (scripts\impact.ps1; -Full runs the whole gate)'
    $impactArgs = @{ Run = $true; PassThru = $true }
    if ($Base) { $impactArgs['Base'] = $Base }
    $result = & (Join-Path $PSScriptRoot 'impact.ps1') @impactArgs
    if (-not $result.Ok) { throw 'impact plan failed' }
    if ($result.Full) { Invoke-DeclaredLicences }
    if ($Smoke) { Invoke-Smoke } else { Write-Host 'live smoke skipped (use -Smoke; it opens a window)' }
    if ($result.Full) { Write-Host 'check OK (the impact plan fell back to the full gate)' -ForegroundColor Green }
    else { Write-Host 'check OK (impact plan only: run with -Full before a merge to main, a push or a release)' -ForegroundColor Green }
    return
}

Step 'cargo fmt'
Invoke-Checked 'cargo fmt' { cargo fmt --all --check }

Step 'cargo clippy'
Invoke-Checked 'cargo clippy' { cargo clippy --all-targets --locked -- -D warnings }

if (-not $SkipTests) {
    Step 'cargo test (UCRT_REQUIRE_GPU=1)'
    $oldGpu = $env:UCRT_REQUIRE_GPU
    $env:UCRT_REQUIRE_GPU = '1'
    try { Invoke-Checked 'cargo test' { cargo test --locked } }
    finally { $env:UCRT_REQUIRE_GPU = $oldGpu }
}

Step 'licences'
Invoke-DeclaredLicences
if (Get-Command cargo-deny -ErrorAction SilentlyContinue) {
    Invoke-Checked 'cargo deny' { cargo deny --locked check licenses }
} else {
    Write-Warning 'cargo-deny is not installed (cargo install cargo-deny --locked): only the declared-licence check ran.'
}

if ($Smoke) { Invoke-Smoke } else { Write-Host 'live smoke skipped (use -Smoke; it opens a window)' }

Write-Host 'check OK' -ForegroundColor Green
