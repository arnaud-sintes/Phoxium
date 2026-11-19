<#
.SYNOPSIS
    Reports the cargo target folders and deletes the shared one only when the disk is nearly full.
.DESCRIPTION
    scripts\impact.ps1 builds into one shared target folder (default
    C:\projects\personal\ultimateCRT-wt\target-shared, or CARGO_TARGET_DIR) so that agents and worktrees
    stop rebuilding the external dependencies (windows, ash, the Dear ImGui C++ sources) from scratch.
    That folder is never deleted by the agents. This script:
      - prints the size of the shared folder, of every `target` folder under the worktree root and in the
        main checkout, and the free space of the drive;
      - deletes the shared folder only when the free space is below -MinFreeGB (default 30), or with -Force.
    It refuses to delete while cargo or rustc is running (a build holds the folder), unless -Force.
.PARAMETER MinFreeGB
    Free disk space under which the shared folder is deleted. Default 30.
.PARAMETER Force
    Delete the shared folder whatever the free space (and whatever is running).
.PARAMETER DryRun
    Report what would be deleted, delete nothing.
.PARAMETER SharedDir
    The shared target folder. Default: CARGO_TARGET_DIR when set, else the folder above.
.PARAMETER WorktreeRoot
    Folder holding the worktrees, scanned for `target` folders. Default C:\projects\personal\ultimateCRT-wt.
#>
param(
    [int]$MinFreeGB = 30,
    [switch]$Force,
    [switch]$DryRun,
    [string]$SharedDir,
    [string]$WorktreeRoot = 'C:\projects\personal\ultimateCRT-wt',
    [string]$MainCheckout = 'C:\projects\personal\ultimateCRT'
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
if (-not $SharedDir) {
    $SharedDir = $env:CARGO_TARGET_DIR
    if (-not $SharedDir) { $SharedDir = Join-Path $WorktreeRoot 'target-shared' }
}

function Get-DirSizeGB([string]$path) {
    if (-not (Test-Path $path)) { return $null }
    $sum = (Get-ChildItem $path -Recurse -Force -File -ErrorAction SilentlyContinue | Measure-Object -Property Length -Sum).Sum
    if (-not $sum) { return 0.0 }
    return [Math]::Round($sum / 1GB, 2)
}

$driveLetter = [IO.Path]::GetPathRoot($SharedDir).TrimEnd('\')
if (-not $driveLetter) { $driveLetter = 'C:' }
$free = [Math]::Round((Get-PSDrive ($driveLetter.TrimEnd(':'))).Free / 1GB, 1)

Write-Host "free disk space on ${driveLetter}: $free GB (threshold $MinFreeGB GB)"
Write-Host 'target folders:'
$rows = @()
$rows += [pscustomobject]@{ Folder = $SharedDir; GB = (Get-DirSizeGB $SharedDir); Kind = 'shared' }
foreach ($root in @($WorktreeRoot, $MainCheckout)) {
    if (-not (Test-Path $root)) { continue }
    $cands = @()
    if ($root -eq $MainCheckout) { $cands += (Join-Path $root 'target') }
    else { $cands += @(Get-ChildItem $root -Directory -ErrorAction SilentlyContinue | ForEach-Object { Join-Path $_.FullName 'target' }) }
    foreach ($c in $cands) {
        if ((Test-Path $c) -and ($c -ne $SharedDir)) { $rows += [pscustomobject]@{ Folder = $c; GB = (Get-DirSizeGB $c); Kind = 'per-worktree' } }
    }
}
foreach ($r in $rows) {
    $gb = if ($null -eq $r.GB) { 'absent' } else { "$($r.GB) GB" }
    Write-Host ("  {0,-12} {1,10}  {2}" -f $r.Kind, $gb, $r.Folder)
}

$sharedExists = Test-Path $SharedDir
if (-not $sharedExists) { Write-Host 'the shared target folder does not exist: nothing to delete.'; exit 0 }
if (-not $Force -and $free -ge $MinFreeGB) {
    Write-Host "free space is at least $MinFreeGB GB: the shared target folder is kept (use -Force to delete it anyway)."
    exit 0
}
$busy = @(Get-Process -Name cargo, rustc, cargo-clippy, clippy-driver -ErrorAction SilentlyContinue)
if ($busy.Count -gt 0 -and -not $Force) {
    Write-Host "cargo or rustc is running ($($busy.Count) process(es)): not deleting. Wait for the builds, or use -Force." -ForegroundColor Yellow
    exit 1
}
$why = if ($Force) { '-Force' } else { "free space $free GB < $MinFreeGB GB" }
if ($DryRun) { Write-Host "dry run: would delete $SharedDir ($why)"; exit 0 }
Write-Host "deleting $SharedDir ($why)" -ForegroundColor Yellow
Remove-Item -Recurse -Force $SharedDir
Write-Host 'deleted. The next cargo run rebuilds the dependencies once (the Dear ImGui C++ sources take about 16 s in release).'
exit 0
