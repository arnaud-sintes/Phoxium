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
    (same filter as scripts/package.ps1, scripts/licence-policy.ps1: a licence field is required, GPL, LGPL and AGPL
    are refused unless deny.toml allows the licence). -Full also runs the self tests of scripts/impact.ps1 and
    scripts/licence-policy.ps1.
.PARAMETER Smoke
    Also run the live smoke test: `phoxium --pattern ramp --exit-after 10 --validation`. It opens
    an output window for 10 s, so it is opt-in. It fails on a non-zero exit or on any validation line
    on stderr.
.PARAMETER SmokeGpu
    Vulkan device index (`phoxium --list-gpus`) the smoke run uses (`--gpu`). Default: the automatic choice, the adapter of
    the monitor the output covers, which depends on the monitor layout (an Intel UHD 770 reports a lower
    `maxPerStageDescriptorStorageImages`, 16, than the optics layout declares, 22: one known validation line, QUESTIONS REV-2 notes).
.PARAMETER SkipTests
    Skip `cargo test` (fmt, clippy and licences still run). Full gate only.
.PARAMETER Full
    Run the whole gate (the behaviour before the impact analysis existed).
.PARAMETER Base
    Git ref the impact analysis diffs against (default: merge base with exec/integration, else HEAD~1).
#>
param([switch]$Smoke, [switch]$SkipTests, [switch]$Full, [string]$Base, [int]$SmokeGpu = -1)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$root = Split-Path $PSScriptRoot -Parent
Set-Location $root
# Same shared build cache and no incremental cache as scripts\impact.ps1 (ENV.md, Test policy). The shared folder
# exists only when the repository has a <name>-wt worktree folder next to it (scripts\shared-paths.ps1); otherwise
# cargo's own target folder is used.
. (Join-Path $PSScriptRoot 'shared-paths.ps1')
. (Join-Path $PSScriptRoot 'licence-policy.ps1')
if (-not $env:CARGO_TARGET_DIR) {
    $shared = Get-SharedTargetDir $root
    if ($shared) { $env:CARGO_TARGET_DIR = $shared }
}
if (-not $env:CARGO_INCREMENTAL) { $env:CARGO_INCREMENTAL = '0' }

# Windows PowerShell 5.1 turns native stderr into errors under 'Stop': check the exit code instead.
function Invoke-Checked([string]$what, [scriptblock]$block) {
    $previous = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try { & $block } finally { $ErrorActionPreference = $previous }
    if ($LASTEXITCODE -ne 0) { throw "$what failed with exit code $LASTEXITCODE" }
}

function Step([string]$name) { Write-Host "== $name" -ForegroundColor Cyan }

# The declared-licence check of cargo metadata (same filter as scripts/package.ps1: scripts/licence-policy.ps1).
function Invoke-DeclaredLicences {
    Step 'declared licences'
    $meta = (Invoke-Checked 'cargo metadata' { cargo metadata --format-version 1 --locked --filter-platform x86_64-pc-windows-msvc } | Out-String) | ConvertFrom-Json
    $members = @($meta.workspace_members)
    $used = @{}
    foreach ($node in $meta.resolve.nodes) { $used[$node.id] = $true }
    $rows = @($meta.packages | Where-Object { $used.ContainsKey($_.id) -and ($members -notcontains $_.id) })
    Assert-DeclaredLicences $rows (Join-Path $root 'deny.toml')
    Write-Host "declared licences OK ($($rows.Count) crates)"
}

function Invoke-Smoke {
    Step 'live smoke (opens a window for 10 s)'
    Invoke-Checked 'cargo build' { cargo build --locked -p phoxium }
    $targetDir = $env:CARGO_TARGET_DIR
    if (-not $targetDir) { $targetDir = Join-Path $root 'target' }
    $exe = Join-Path $targetDir 'debug\phoxium.exe'
    $errFile = [IO.Path]::GetTempFileName()
    try {
        $smokeArgs = @('--pattern', 'ramp', '--exit-after', '10', '--validation')
        if ($SmokeGpu -ge 0) { $smokeArgs += @('--gpu', "$SmokeGpu") }
        $p = Start-Process -FilePath $exe -ArgumentList $smokeArgs `
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

Step 'script self tests (impact.ps1, licence-policy.ps1)'
Invoke-Checked 'impact self test' { & powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot 'impact.ps1') -SelfTest }
Invoke-Checked 'licence policy self test' { & powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot 'licence-policy.ps1') -SelfTest }

Step 'cargo fmt'
Invoke-Checked 'cargo fmt' { cargo fmt --all --check }

Step 'cargo clippy'
Invoke-Checked 'cargo clippy' { cargo clippy --all-targets --locked -- -D warnings }

if (-not $SkipTests) {
    Step 'cargo test (PHOXIUM_REQUIRE_GPU=1)'
    $oldGpu = $env:PHOXIUM_REQUIRE_GPU
    $env:PHOXIUM_REQUIRE_GPU = '1'
    try { Invoke-Checked 'cargo test' { cargo test --locked } }
    finally { $env:PHOXIUM_REQUIRE_GPU = $oldGpu }
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
