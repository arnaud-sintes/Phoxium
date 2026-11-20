<#
.SYNOPSIS
    Writes the third-party notices bundle of the release zip: the licence TEXTS of every dependency built into or
    with the executables, then the repository notices of THIRD-PARTY.md (Dear ImGui, cimgui, ProggyClean, stb, Inter).
.DESCRIPTION
    The dependencies are the non-workspace packages of `cargo metadata --locked` for x86_64-pc-windows-msvc. For
    each one the script copies the licence and notice files found at the top of the package folder (the folder of
    its manifest in the cargo registry: LICENSE*, COPYING*, NOTICE*, UNLICENSE*, including LICENSE-MIT and
    license-apache-2.0 style names). A package with no such file fails the script, except the packages listed in
    $CoveredByRepoNotices, whose notices THIRD-PARTY.md of the repository carries (they ship no file). The licence
    policy (licence field present, no GPL-family licence unless deny.toml allows it) is checked first.
    The script prints a count check: licence blocks written against the packages found.
.PARAMETER OutFile
    The file to write (UTF-8 without BOM, LF line ends). Required.
.PARAMETER Root
    Repository root (default: the parent of scripts/).
#>
param(
    [Parameter(Mandatory = $true)][string]$OutFile,
    [string]$Root
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
if (-not $Root) { $Root = Split-Path $PSScriptRoot -Parent }
$Root = (Resolve-Path $Root).Path.TrimEnd('\', '/')
. (Join-Path $PSScriptRoot 'licence-policy.ps1')

# Packages that ship no licence file; THIRD-PARTY.md of the repository carries their notices.
$CoveredByRepoNotices = @('dear-imgui-rs', 'dear-imgui-sys', 'dear-imgui-ash', 'dear-imgui-build-support')

$prev = $ErrorActionPreference
$ErrorActionPreference = 'Continue'
try {
    $json = (& cargo metadata --format-version 1 --locked --filter-platform x86_64-pc-windows-msvc --manifest-path (Join-Path $Root 'Cargo.toml') | Out-String)
    $code = $LASTEXITCODE
} finally { $ErrorActionPreference = $prev }
if ($code -ne 0) { throw "cargo metadata failed with exit code $code" }
$meta = $json | ConvertFrom-Json
$members = @($meta.workspace_members)
$used = @{}
foreach ($node in $meta.resolve.nodes) { $used[$node.id] = $true }
$rows = @($meta.packages | Where-Object { $used.ContainsKey($_.id) -and ($members -notcontains $_.id) } | Sort-Object name, version)

Assert-DeclaredLicences $rows (Join-Path $Root 'deny.toml')

$sb = New-Object System.Text.StringBuilder
function Add-Line([string]$s) { [void]$sb.Append($s).Append("`n") }

Add-Line '# Third-party licences'
Add-Line ''
Add-Line 'ultimateCRT itself is MIT-licensed (LICENSE). This file carries the licence texts of the third-party code that is'
Add-Line 'built into or with the executables: first every Rust crate (the licence expression it declares, from `cargo metadata`)'
Add-Line 'then the licence and notice files of its package, unmodified), then the notices of the C and C++ sources and the'
Add-Line 'font that the crates do not carry.'
Add-Line ''
Add-Line '## Crates'
Add-Line ''
Add-Line '| Crate | Version | Licence |'
Add-Line '|---|---|---|'
foreach ($r in $rows) { Add-Line "| $($r.name) | $($r.version) | $($r.license) |" }
Add-Line ''
Add-Line '## Licence texts of the crates'

$blocks = 0
$files = 0
$noFile = New-Object System.Collections.Generic.List[string]
foreach ($r in $rows) {
    $dir = Split-Path $r.manifest_path -Parent
    $found = @(Get-ChildItem -LiteralPath $dir -File -ErrorAction SilentlyContinue |
            Where-Object { $_.Name -match '^(?i)(licen[sc]e|copying|notice|unlicense)' -and $_.Length -gt 0 } | Sort-Object Name)
    Add-Line ''
    Add-Line "### $($r.name) $($r.version)"
    Add-Line ''
    Add-Line "Declared licence: $($r.license)"
    if ($found.Count -eq 0) {
        if ($CoveredByRepoNotices -contains $r.name) {
            Add-Line ''
            Add-Line 'The package ships no licence file; its notice is in the section "Dear ImGui crates" below.'
            $blocks++
            continue
        }
        $noFile.Add("$($r.name) $($r.version) ($dir)")
        continue
    }
    $blocks++
    foreach ($f in $found) {
        $text = [IO.File]::ReadAllText($f.FullName, [Text.Encoding]::UTF8) -replace "`r`n", "`n"
        Add-Line ''
        Add-Line "File: $($f.Name)"
        Add-Line ''
        # A tilde fence: licence texts never contain a line of four tildes.
        Add-Line '~~~~'
        [void]$sb.Append($text.TrimEnd()).Append("`n")
        Add-Line '~~~~'
        $files++
    }
}
if ($noFile.Count -gt 0) {
    throw "Packages with no licence or notice file (add the file, or cover them in THIRD-PARTY.md and `$CoveredByRepoNotices): $($noFile -join '; ')"
}
if ($blocks -ne $rows.Count) { throw "Count check failed: $blocks licence blocks for $($rows.Count) packages." }

# The repository notices (C/C++ sources and the font): the sections of THIRD-PARTY.md, without its own intro.
Add-Line ''
Add-Line '# Notices the crates do not carry'
Add-Line ''
$repoNotices = [IO.File]::ReadAllText((Join-Path $Root 'THIRD-PARTY.md'), [Text.Encoding]::UTF8) -replace "`r`n", "`n"
$firstSection = $repoNotices.IndexOf("`n## ")
if ($firstSection -lt 0) { throw 'THIRD-PARTY.md has no section.' }
Add-Line $repoNotices.Substring($firstSection + 1).TrimEnd()

$dest = $OutFile
if (-not [IO.Path]::IsPathRooted($dest)) { $dest = Join-Path (Get-Location).Path $dest }
[IO.File]::WriteAllText($dest, $sb.ToString(), (New-Object Text.UTF8Encoding $false))

# Count check against Cargo.lock: the packages of the lock file that are not bundled are the workspace crates and
# the dependencies of other platforms (not built for x86_64-pc-windows-msvc, so not in the zip).
$memberNames = @($meta.packages | Where-Object { $members -contains $_.id } | ForEach-Object { $_.name })
$lockPackages = @([regex]::Matches([IO.File]::ReadAllText((Join-Path $Root 'Cargo.lock')), '(?m)^name = "([^"]+)"\r?\nversion = "([^"]+)"') |
        ForEach-Object { @{ Name = $_.Groups[1].Value; Version = $_.Groups[2].Value } } | Where-Object { $memberNames -notcontains $_.Name })
$bundled = @{}
foreach ($r in $rows) { $bundled["$($r.name) $($r.version)"] = $true }
$notBundled = @($lockPackages | Where-Object { -not $bundled.ContainsKey("$($_.Name) $($_.Version)") } | ForEach-Object { "$($_.Name) $($_.Version)" })
Write-Host ("third-party notices: {0} licence blocks for {1} packages built for x86_64-pc-windows-msvc ({2} licence files copied)" -f $blocks, $rows.Count, $files)
Write-Host ("count check: Cargo.lock lists {0} non-workspace packages; not bundled because other platforms only: {1}" -f $lockPackages.Count, $(if ($notBundled.Count -gt 0) { $notBundled -join ', ' } else { 'none' }))
Write-Host "wrote $dest"