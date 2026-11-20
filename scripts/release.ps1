<#
.SYNOPSIS
    Publishes dist/ultimatecrt-v<version>-windows-x64.zip as a GitHub release tagged v<version>, with the
    notes taken from the matching CHANGELOG.md section.
.DESCRIPTION
    Run it after the version bump, the changelog update, the commit and the push, and after
    scripts/package.ps1. It refuses to run off `main`, on a dirty tree, with an unpushed HEAD, with a zip
    built from another commit, with a zip built from a dirty tree or with `-SkipTests` (recorded as `-dirty` and
    `-untested` in the .commit file), when the tag, the workspace version and the changelog disagree (no dated
    section for the version, entries left under Unreleased, Cargo.lock at another version), when the zip lacks
    LICENSE, THIRD-PARTY.md or the executables, or when the release or the tag already exists. It never overwrites
    a release. The release notes end with the SHA-256 of the zip and the unsigned-binary note.
    If it fails after creating the tag, see "Recovery" in ENV.md.
.PARAMETER DryRun
    Run every check and print the release notes, but create nothing.
#>
param([switch]$DryRun)

$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
Set-Location $root

# Runs a native command, returns its output, throws on a non-zero exit code. Windows PowerShell 5.1 turns
# stderr output into an error under 'Stop', so the preference is relaxed around the call.
function Invoke-Native([string]$what, [scriptblock]$block) {
    $previous = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try { $out = & $block 2>$null } finally { $ErrorActionPreference = $previous }
    if ($LASTEXITCODE -ne 0) { throw "$what failed with exit code $LASTEXITCODE" }
    return $out
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
$tag = "v$version"
$name = "ultimatecrt-v$version-windows-x64"
$zip = "dist\$name.zip"
$sha = "dist\$name.zip.sha256"
$commitFile = "dist\$name.commit"

Invoke-Native 'gh auth status' { gh auth status } | Out-Null

$branch = (Invoke-Native 'git branch' { git rev-parse --abbrev-ref HEAD }).Trim()
if ($branch -ne 'main') { throw "Releases are made from main, current branch is '$branch'." }
if (Invoke-Native 'git status' { git status --porcelain }) { throw 'Working tree is not clean: commit first.' }
Invoke-Native 'git fetch' { git fetch origin --quiet --tags } | Out-Null
$head = (Invoke-Native 'git rev-parse' { git rev-parse HEAD }).Trim()
$upstream = (Invoke-Native 'git rev-parse' { git rev-parse '@{u}' }).Trim()
if ($head -ne $upstream) { throw 'HEAD is not pushed (or differs from its upstream): push first.' }

foreach ($f in $zip, $sha, $commitFile) {
    if (-not (Test-Path $f)) { throw "Missing ${f}: run scripts/package.ps1 on the committed tree." }
}
$built = (Get-Content $commitFile -Raw).Trim()
if ($built -match '-untested') { throw "The zip was built with -SkipTests (untested): run scripts/package.ps1 without -SkipTests." }
if ($built -match '-dirty') { throw "The zip was built from a dirty tree: commit, then run scripts/package.ps1 again." }
if ($built -ne $head) { throw "The zip was built from $built, HEAD is ${head}: run scripts/package.ps1 again." }
$expected = ((Get-Content $sha -Raw).Trim() -split '\s+')[0]
if ((Get-FileHash $zip -Algorithm SHA256).Hash.ToLower() -ne $expected) { throw "$zip does not match its checksum file." }

# The zip is the one for this version and carries what a public release must carry.
Add-Type -AssemblyName System.IO.Compression.FileSystem
$archive = [IO.Compression.ZipFile]::OpenRead((Resolve-Path $zip).Path)
try {
    $entries = @($archive.Entries | ForEach-Object { $_.FullName -replace '\\', '/' })
    foreach ($need in 'LICENSE', 'THIRD-PARTY.md', 'README.md', 'CHANGELOG.md', 'RUNNING.txt', 'BUILD-INFO.txt', 'ultimatecrt.exe', 'pixel-demo.exe') {
        if ($entries -notcontains "$name/$need") { throw "$zip has no $need (expected $name/$need): run scripts/package.ps1 again." }
    }
    $runningEntry = $archive.GetEntry("$name/RUNNING.txt")
    $reader = New-Object IO.StreamReader($runningEntry.Open())
    try { $firstLine = $reader.ReadLine() } finally { $reader.Dispose() }
    if ($firstLine -ne "ultimateCRT $version") { throw "RUNNING.txt of $zip says '$firstLine', expected 'ultimateCRT $version'." }
} finally { $archive.Dispose() }

# Version agreement: the lock file, the changelog and the tag all name the version of [workspace.package].
$lockText = Get-Content 'Cargo.lock' -Raw
foreach ($crate in 'appres', 'crt-model', 'paramgen', 'pixel-demo', 'ultimatecrt') {
    $m = [regex]::Match($lockText, '(?m)^name = "' + [regex]::Escape($crate) + '"\r?\nversion = "([^"]+)"')
    if (-not $m.Success) { throw "Cargo.lock has no entry for $crate." }
    if ($m.Groups[1].Value -ne $version) { throw "Cargo.lock has $crate $($m.Groups[1].Value), the workspace version is ${version}: run cargo build to refresh Cargo.lock and commit it." }
}
$changelogLines = @(Get-Content 'CHANGELOG.md')
$dated = @($changelogLines | Where-Object { $_ -match ('^## \[' + [regex]::Escape($version) + '\] - \d{4}-\d{2}-\d{2}\s*$') })
if ($dated.Count -ne 1) { throw "CHANGELOG.md needs exactly one heading '## [$version] - YYYY-MM-DD' (found $($dated.Count))." }
$unreleasedAt = -1
for ($i = 0; $i -lt $changelogLines.Count; $i++) { if ($changelogLines[$i] -match '^## \[Unreleased\]') { $unreleasedAt = $i; break } }
if ($unreleasedAt -ge 0) {
    for ($i = $unreleasedAt + 1; $i -lt $changelogLines.Count -and $changelogLines[$i] -notmatch '^## \['; $i++) {
        if ($changelogLines[$i] -match '^- ') { throw 'CHANGELOG.md still has entries under [Unreleased]: move them into the release section (ENV.md, Publish, step 2).' }
    }
}
if (Invoke-Native 'git tag' { git tag --list $tag }) { throw "Tag $tag already exists locally." }
if (Invoke-Native 'git ls-remote' { git ls-remote --tags origin "refs/tags/$tag" }) { throw "Tag $tag already exists on origin." }
$previous = $ErrorActionPreference
$ErrorActionPreference = 'Continue'
gh release view $tag 2>$null | Out-Null
$exists = ($LASTEXITCODE -eq 0)
$ErrorActionPreference = $previous
if ($exists) { throw "Release $tag already exists." }

# Notes: the changelog section "## [<version>]" up to the next "## [".
$lines = @(Get-Content 'CHANGELOG.md')
$start = -1
for ($i = 0; $i -lt $lines.Count; $i++) {
    if ($lines[$i] -match ('^## \[' + [regex]::Escape($version) + '\]')) { $start = $i; break }
}
if ($start -lt 0) { throw "CHANGELOG.md has no section for $version." }
$end = $lines.Count
for ($i = $start + 1; $i -lt $lines.Count; $i++) {
    if ($lines[$i] -match '^## \[') { $end = $i; break }
}
$body = @($lines[($start + 1)..($end - 1)] | Where-Object { $_ -notmatch '^\[.+\]:\s' })
$notes = ($body -join "`n").Trim()
if (-not ($notes -match '(?m)^- ')) { throw "The changelog section for $version has no entries." }
$notes += "`n`n---`n`nSHA-256 of ``$name.zip`` (also in the attached ``.sha256`` file): ``$expected```n`n" +
    "The executables are not code-signed, so Windows SmartScreen may warn on first run. To check your download: " +
    "``(Get-FileHash .\$name.zip -Algorithm SHA256).Hash`` in PowerShell must print the value above."
$notesFile = Join-Path $env:TEMP "ultimatecrt-notes-$version.md"
[IO.File]::WriteAllText($notesFile, $notes + "`n", (New-Object Text.UTF8Encoding $false))

try {
    if ($DryRun) {
        Write-Host "Dry run: would create release $tag at $head with $zip and $sha. Notes:"
        Write-Host $notes
        return
    }
    Invoke-Native 'gh release create' {
        gh release create $tag $zip $sha --title "ultimateCRT $tag" --notes-file $notesFile --target $head
    } | Out-Null
    Write-Host "Released $tag"
} finally {
    Remove-Item $notesFile -ErrorAction SilentlyContinue
}
