<#
.SYNOPSIS
    Reports the cargo target folders and deletes the shared one only when the disk is nearly full (or with -Force),
    and only when it really is a cargo target folder in an expected place.
.DESCRIPTION
    scripts\impact.ps1 and scripts\check.ps1 build into one shared target folder (CARGO_TARGET_DIR when set, else
    `<worktree root>\target-shared` when the repository has a `<name>-wt` worktree folder next to it, else cargo's
    own `target` in the repository) so that agents and worktrees stop rebuilding the external dependencies
    (windows, ash, the Dear ImGui C++ sources) from scratch. This script:
      - prints the size of the shared folder, of every `target` folder under the worktree root and in the main
        checkout, and the free space of the drive;
      - deletes the shared folder only when the free space is below -MinFreeGB (default 30), or with -Force.
    Safety (the folder named by CARGO_TARGET_DIR is whatever the environment says, so it is checked):
      - never deleted, even with -Force: a drive root, the repository, the main checkout, the worktree root, any
        folder that contains one of them, and the user profile folder;
      - without -Force it must be a cargo target folder (it holds CACHEDIR.TAG or .rustc_info.json) located under
        the worktree root, under the main checkout or next to the repository; otherwise the script refuses and
        says why;
      - it asks for a typed confirmation (y) before deleting, unless -Force; without an interactive console it
        refuses instead of asking.
    It refuses to delete while cargo or rustc is running (a build holds the folder), unless -Force.
.PARAMETER MinFreeGB
    Free disk space under which the shared folder is deleted. Default 30.
.PARAMETER Force
    Delete the shared folder whatever the free space, whatever is running, without asking, and also when it does
    not look like a cargo target folder or sits outside the expected places (the hard refusals stay).
.PARAMETER DryRun
    Report what would be deleted or refused, delete nothing.
.PARAMETER SharedDir
    The shared target folder. Default: CARGO_TARGET_DIR when set, else <worktree root>\target-shared, else
    <repository>\target.
.PARAMETER WorktreeRoot
    Folder holding the worktrees, scanned for `target` folders. Default: derived from the repository location
    (scripts\shared-paths.ps1).
.PARAMETER MainCheckout
    The main checkout, scanned for its `target` folder. Default: derived from the repository location.
.PARAMETER SelfTest
    Run the tests of the safety checks and exit.
#>
param(
    [int]$MinFreeGB = 30,
    [switch]$Force,
    [switch]$DryRun,
    [string]$SharedDir,
    [string]$WorktreeRoot,
    [string]$MainCheckout,
    [switch]$SelfTest
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
. (Join-Path $PSScriptRoot 'shared-paths.ps1')
$repoRoot = Split-Path $PSScriptRoot -Parent

function Get-FullPath([string]$p) {
    return [IO.Path]::GetFullPath($p).TrimEnd('\', '/')
}

function Test-IsInside([string]$child, [string]$parent) {
    # True when $child is $parent or below it (case-insensitive, whole path segments).
    $c = (Get-FullPath $child).TrimEnd('\') + '\'
    $p = (Get-FullPath $parent).TrimEnd('\') + '\'
    return $c.StartsWith($p, [StringComparison]::OrdinalIgnoreCase)
}

# $null when the folder may be deleted, else the reason it may not.
function Get-DeleteRefusal([string]$Path, [string]$Repo, [string]$WtRoot, [string]$Main, [bool]$ForceIt) {
    $full = Get-FullPath $Path
    if ($full -match '^[A-Za-z]:$' -or $full -eq '' -or [IO.Path]::GetPathRoot($full).TrimEnd('\') -eq $full) {
        return "$Path is a drive root: never deleted."
    }
    $protected = @($Repo, $Main, $WtRoot, $env:USERPROFILE, $env:SystemRoot, $env:ProgramFiles) | Where-Object { $_ }
    foreach ($q in $protected) {
        if (Test-IsInside $q $full) { return "$full is (or contains) $(Get-FullPath $q): never deleted." }
    }
    if ($ForceIt) { return $null }
    if (-not (Test-Path -LiteralPath $full -PathType Container)) { return $null }
    $marked = (Test-Path -LiteralPath (Join-Path $full 'CACHEDIR.TAG')) -or (Test-Path -LiteralPath (Join-Path $full '.rustc_info.json'))
    if (-not $marked) {
        return "$full does not look like a cargo target folder (no CACHEDIR.TAG and no .rustc_info.json): not deleted (use -Force if you are sure)."
    }
    $expected = @($WtRoot, $Main) | Where-Object { $_ }
    $ok = $false
    foreach ($e in $expected) { if (Test-IsInside $full $e) { $ok = $true; break } }
    # Next to the repository: a direct sibling folder of it.
    if (-not $ok -and ((Get-FullPath (Split-Path $full -Parent)) -eq (Get-FullPath (Split-Path $Repo -Parent)))) { $ok = $true }
    if (-not $ok) {
        return "$full is a cargo target folder but not under the worktree root, the main checkout or next to the repository: not deleted (use -Force if you are sure)."
    }
    return $null
}

function Invoke-CleanSelfTest {
    $fail = 0
    $tmp = Join-Path ([IO.Path]::GetTempPath()) ('clean-targets-test-' + [Guid]::NewGuid().ToString('N'))
    try {
        $repo = Join-Path $tmp 'proj'
        $wt = Join-Path $tmp 'proj-wt'
        $shared = Join-Path $wt 'target-shared'
        $other = Join-Path $tmp 'elsewhere\target'
        $plain = Join-Path $wt 'notes'
        foreach ($d in $repo, $wt, $shared, $other, $plain) { New-Item -ItemType Directory -Force $d | Out-Null }
        Set-Content -LiteralPath (Join-Path $shared 'CACHEDIR.TAG') 'Signature: 8a477f597d28d172789f06886806bc55'
        Set-Content -LiteralPath (Join-Path $other '.rustc_info.json') '{}'
        $root = [IO.Path]::GetPathRoot($tmp)
        $cases = @(
            @{ N = 'a drive root is refused'; P = $root; F = $false; Refused = $true },
            @{ N = 'a drive root is refused with -Force'; P = $root; F = $true; Refused = $true },
            @{ N = 'the repository is refused with -Force'; P = $repo; F = $true; Refused = $true },
            @{ N = 'the worktree root is refused with -Force'; P = $wt; F = $true; Refused = $true },
            @{ N = 'a parent of the repository is refused with -Force'; P = $tmp; F = $true; Refused = $true },
            @{ N = 'a non-cargo folder is refused'; P = $plain; F = $false; Refused = $true },
            @{ N = 'a non-cargo folder is accepted with -Force'; P = $plain; F = $true; Refused = $false },
            @{ N = 'a cargo folder outside the expected places is refused'; P = $other; F = $false; Refused = $true },
            @{ N = 'a cargo folder under the worktree root is accepted'; P = $shared; F = $false; Refused = $false }
        )
        $n = 0
        foreach ($c in $cases) {
            $n++
            $r = Get-DeleteRefusal $c.P $repo $wt $repo $c.F
            $ok = ($c.Refused -eq [bool]$r)
            if ($ok) { Write-Host ("  ok   {0,2}. {1}" -f $n, $c.N) } else { $fail++; Write-Host ("  FAIL {0,2}. {1} (got: {2})" -f $n, $c.N, $r) -ForegroundColor Red }
        }
        Write-Host ("clean-targets self test: {0} case(s), {1} failure(s)" -f $cases.Count, $fail)
    } finally { Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue }
    return $fail
}

if ($SelfTest) {
    $f = Invoke-CleanSelfTest
    exit ([int]($f -gt 0))
}

if (-not $WorktreeRoot) { $WorktreeRoot = Get-WorktreeRoot $repoRoot }
if (-not $MainCheckout) { $MainCheckout = Get-MainCheckout $repoRoot }
if (-not $SharedDir) {
    $SharedDir = $env:CARGO_TARGET_DIR
    if (-not $SharedDir) { $SharedDir = Get-SharedTargetDir $repoRoot }
    if (-not $SharedDir) { $SharedDir = Join-Path $repoRoot 'target' }
}

function Get-DirSizeGB([string]$path) {
    if (-not (Test-Path $path)) { return $null }
    $sum = (Get-ChildItem $path -Recurse -Force -File -ErrorAction SilentlyContinue | Measure-Object -Property Length -Sum).Sum
    if (-not $sum) { return 0.0 }
    return [Math]::Round($sum / 1GB, 2)
}

# Safety first: a folder that may not be deleted is reported even when the disk is not full.
$refusal = Get-DeleteRefusal $SharedDir $repoRoot $WorktreeRoot $MainCheckout ([bool]$Force)
if ($refusal) {
    Write-Host "refused: $refusal" -ForegroundColor Red
    exit 2
}
if (-not (Test-Path $SharedDir)) { Write-Host 'the shared target folder does not exist: nothing to delete.'; exit 0 }
$driveRoot = [IO.Path]::GetPathRoot((Get-FullPath $SharedDir))
$driveName = $driveRoot.TrimEnd('\').TrimEnd(':')
$free = $null
if ($driveName.Length -eq 1) { $free = [Math]::Round((Get-PSDrive $driveName).Free / 1GB, 1) }
if ($null -eq $free) { Write-Host "free disk space: unknown for $driveRoot (threshold $MinFreeGB GB)"; $free = [double]::PositiveInfinity }
else { Write-Host "free disk space on $($driveName):: $free GB (threshold $MinFreeGB GB)".Replace('::', ':') }
Write-Host 'target folders:'
$rows = @()
$rows += [pscustomobject]@{ Folder = $SharedDir; GB = (Get-DirSizeGB $SharedDir); Kind = 'shared' }
$scanned = @{}
foreach ($scanRoot in @($WorktreeRoot, $MainCheckout)) {
    if (-not $scanRoot -or -not (Test-Path $scanRoot)) { continue }
    $cands = @()
    if ($scanRoot -eq $MainCheckout) { $cands += (Join-Path $scanRoot 'target') }
    else { $cands += @(Get-ChildItem $scanRoot -Directory -ErrorAction SilentlyContinue | ForEach-Object { Join-Path $_.FullName 'target' }) }
    foreach ($c in $cands) {
        if ((Test-Path $c) -and ((Get-FullPath $c) -ne (Get-FullPath $SharedDir)) -and -not $scanned.ContainsKey($c)) {
            $scanned[$c] = $true
            $rows += [pscustomobject]@{ Folder = $c; GB = (Get-DirSizeGB $c); Kind = 'per-worktree' }
        }
    }
}
foreach ($r in $rows) {
    $gb = if ($null -eq $r.GB) { 'absent' } else { "$($r.GB) GB" }
    Write-Host ("  {0,-12} {1,10}  {2}" -f $r.Kind, $gb, $r.Folder)
}

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
if (-not $Force) {
    $answer = $null
    try { $answer = Read-Host "Delete $SharedDir ($why)? Type y to confirm" } catch { $answer = $null }
    if ($answer -ne 'y') { Write-Host 'not confirmed (or no interactive console): nothing deleted. Use -Force to skip the question.' -ForegroundColor Yellow; exit 1 }
}
Write-Host "deleting $SharedDir ($why)" -ForegroundColor Yellow
Remove-Item -Recurse -Force -LiteralPath $SharedDir
Write-Host 'deleted. The next cargo run rebuilds the dependencies once (the Dear ImGui C++ sources take about 16 s in release).'
exit 0
