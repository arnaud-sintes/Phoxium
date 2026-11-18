<#
.SYNOPSIS
    Local validation gate: fmt check, clippy (-D warnings), tests with the GPU required, licence check,
    and an optional live smoke run.
.DESCRIPTION
    Exits 0 only when every step passes. The licence check runs `cargo deny check licenses` (deny.toml)
    when cargo-deny is installed, and always runs the declared-licence check of cargo metadata (same
    policy as scripts/package.ps1: a licence field is required, GPL is refused).
.PARAMETER Smoke
    Also run the live smoke test: `ultimatecrt --pattern ramp --exit-after 10 --validation`. It opens
    an output window for 10 s, so it is opt-in. It fails on a non-zero exit or on any validation line
    on stderr.
.PARAMETER SkipTests
    Skip `cargo test` (fmt, clippy and licences still run).
#>
param([switch]$Smoke, [switch]$SkipTests)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$root = Split-Path $PSScriptRoot -Parent
Set-Location $root

# Windows PowerShell 5.1 turns native stderr into errors under 'Stop': check the exit code instead.
function Invoke-Checked([string]$what, [scriptblock]$block) {
    $previous = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try { & $block } finally { $ErrorActionPreference = $previous }
    if ($LASTEXITCODE -ne 0) { throw "$what failed with exit code $LASTEXITCODE" }
}

function Step([string]$name) { Write-Host "== $name" -ForegroundColor Cyan }

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
if (Get-Command cargo-deny -ErrorAction SilentlyContinue) {
    Invoke-Checked 'cargo deny' { cargo deny --locked check licenses }
} else {
    Write-Warning 'cargo-deny is not installed (cargo install cargo-deny --locked): only the declared-licence check ran.'
}

if ($Smoke) {
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
} else {
    Write-Host 'live smoke skipped (use -Smoke; it opens a window)'
}

Write-Host 'check OK' -ForegroundColor Green
