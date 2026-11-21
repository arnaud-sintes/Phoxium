<#
.SYNOPSIS
    Publishes dist/phoxium-v<version>-windows-x64.zip as a GitHub release tagged v<version>, with the
    notes taken from the matching CHANGELOG.md section, in two steps: a DRAFT release first, then -Publish.
.DESCRIPTION
    Run it after the version bump, the changelog update, the commit and the push, and after
    scripts/package.ps1. It refuses to run off `main`, on a dirty tree, with an unpushed HEAD, with a zip
    built from another commit, with a zip built from a dirty tree or with `-SkipTests` (recorded as `-dirty` and
    `-untested` in the .commit file), when the tag, the workspace version and the changelog disagree (no dated
    section for the version, entries left under Unreleased, Cargo.lock at another version), when the zip lacks
    LICENSE, THIRD-PARTY.md or the executables, when `origin` is not the target repository, or when the tag
    already exists. It never overwrites a release. The release notes end with the SHA-256 of the zip and the
    unsigned-binary note.

    Without a switch it creates the release as a DRAFT on the target repository (-Repo, printed with the tag and
    the assets), checks that the uploaded assets have the sizes of the local files, and stops: nothing is public
    yet and the tag does not exist yet. Look at the draft (the printed URL), then run it again with -Publish: the
    same checks, plus a draft for the tag whose assets match the local files, then the draft becomes public and
    GitHub creates the tag at HEAD. Errors of git and gh are shown with their text. If it fails half way, see
    "Recovery" in ENV.md.
.PARAMETER DryRun
    Run every check and print the target, the assets and the release notes, but create nothing.
.PARAMETER Publish
    Second step: make the existing draft release of this version public (after the checks).
.PARAMETER AllowUntested
    Accept a zip built with `package.ps1 -SkipTests` (recorded as `-untested`). Only the owner's explicit decision
    justifies it; the release notes then say that the test step was skipped. A dirty build is still refused.
.PARAMETER Repo
    The GitHub repository (owner/name) of the release. Default arnaud-sintes/Phoxium; `origin` must point to it.
#>
param([switch]$DryRun, [switch]$Publish, [switch]$AllowUntested, [string]$Repo = 'arnaud-sintes/Phoxium')

$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
Set-Location $root

# Runs a native command, returns its standard output, throws on a non-zero exit code with the command's error
# text (stderr). Windows PowerShell 5.1 turns stderr output into an error under 'Stop', so the preference is
# relaxed around the call and stderr is captured separately (D-11: never hidden).
function Invoke-Native([string]$what, [scriptblock]$block) {
    $previous = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try { $all = @(& $block 2>&1) } finally { $ErrorActionPreference = $previous }
    $code = $LASTEXITCODE
    $out = @($all | Where-Object { $_ -isnot [System.Management.Automation.ErrorRecord] } | ForEach-Object { "$_" })
    $err = @($all | Where-Object { $_ -is [System.Management.Automation.ErrorRecord] } | ForEach-Object { "$_" })
    if ($code -ne 0) {
        $detail = (($err + $out) -join "`n").Trim()
        if ($detail) { throw "$what failed with exit code ${code}:`n$detail" }
        throw "$what failed with exit code $code"
    }
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

# The release $tag of $Repo as an object (tagName, isDraft, url, assets), or $null when there is none. Any other
# failure of gh is an error with its text.
function Get-Release([string]$tag) {
    $previous = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try { $all = @(gh release view $tag --repo $Repo --json 'tagName,isDraft,url,assets' 2>&1) } finally { $ErrorActionPreference = $previous }
    $code = $LASTEXITCODE
    $text = (@($all | ForEach-Object { "$_" }) -join "`n").Trim()
    if ($code -eq 0) { return ($text | ConvertFrom-Json) }
    if ($text -match '(?i)release not found|not found') { return $null }
    throw "gh release view $tag --repo $Repo failed with exit code ${code}:`n$text"
}

# Throws unless the release's assets are exactly the local files $files (same names and sizes).
function Assert-ReleaseAssets($release, [string[]]$files) {
    $assets = @($release.assets)
    $want = @($files | ForEach-Object { Split-Path $_ -Leaf })
    $have = @($assets | ForEach-Object { $_.name })
    if ((@($want | Sort-Object) -join '|') -ne (@($have | Sort-Object) -join '|')) {
        throw "Release $($release.tagName) has the assets [$($have -join ', ')], expected [$($want -join ', ')]."
    }
    foreach ($f in $files) {
        $a = $assets | Where-Object { $_.name -eq (Split-Path $f -Leaf) }
        $size = (Get-Item -LiteralPath $f).Length
        if ([int64]$a.size -ne $size) { throw "Asset $($a.name) of release $($release.tagName) has $($a.size) bytes, the local file $size." }
    }
}

$version = Get-WorkspaceVersion
$tag = "v$version"
$name = "phoxium-v$version-windows-x64"
$zip = "dist\$name.zip"
$sha = "dist\$name.zip.sha256"
$commitFile = "dist\$name.commit"

Invoke-Native 'gh auth status' { gh auth status } | Out-Null

# D-11: the target repository is explicit; origin (where HEAD was pushed and the tag will live) must be it.
$origin = (Invoke-Native 'git remote get-url' { git remote get-url origin }) -join ''
if ($origin -notmatch ('[/:]' + [regex]::Escape($Repo) + '(\.git)?/?$')) {
    $hint = "give -Repo or fix the remote"
    if ($origin -match '(?i)[/:]arnaud-sintes/ultimateCRT(\.git)?/?$' -and $Repo -eq 'arnaud-sintes/Phoxium') {
        # The project was renamed from ultimateCRT to Phoxium (2026-11-21): the GitHub repository follows by hand.
        $hint = "the project was renamed: first rename the GitHub repository arnaud-sintes/ultimateCRT to Phoxium " +
            "(GitHub, Settings, Repository name), then run: git remote set-url origin https://github.com/arnaud-sintes/Phoxium.git"
    }
    throw "origin is $origin, not the target repository $Repo ($hint)."
}
$branch = ((Invoke-Native 'git branch' { git rev-parse --abbrev-ref HEAD }) -join '').Trim()
if ($branch -ne 'main') { throw "Releases are made from main, current branch is '$branch'." }
if (Invoke-Native 'git status' { git status --porcelain }) { throw 'Working tree is not clean: commit first.' }
Invoke-Native 'git fetch' { git fetch origin --quiet --tags } | Out-Null
$head = ((Invoke-Native 'git rev-parse' { git rev-parse HEAD }) -join '').Trim()
$upstream = ((Invoke-Native 'git rev-parse' { git rev-parse '@{u}' }) -join '').Trim()
if ($head -ne $upstream) { throw 'HEAD is not pushed (or differs from its upstream): push first.' }

foreach ($f in $zip, $sha, $commitFile) {
    if (-not (Test-Path $f)) { throw "Missing ${f}: run scripts/package.ps1 on the committed tree." }
}
$built = (Get-Content $commitFile -Raw).Trim()
$untested = $built -match '-untested'
if ($untested -and -not $AllowUntested) { throw "The zip was built with -SkipTests (untested): run scripts/package.ps1 without -SkipTests (or, on the owner's explicit decision, pass -AllowUntested)." }
if ($untested) { $built = $built -replace '-untested', ''; Write-Warning 'The zip was built WITHOUT the test step (-AllowUntested): the release notes will say so.' }
if ($built -match '-dirty') { throw "The zip was built from a dirty tree: commit, then run scripts/package.ps1 again." }
if ($built -ne $head) { throw "The zip was built from $built, HEAD is ${head}: run scripts/package.ps1 again." }
$expected = ((Get-Content $sha -Raw).Trim() -split '\s+')[0]
if ((Get-FileHash $zip -Algorithm SHA256).Hash.ToLower() -ne $expected) { throw "$zip does not match its checksum file." }

# The zip is the one for this version and carries what a public release must carry.
Add-Type -AssemblyName System.IO.Compression.FileSystem
$archive = [IO.Compression.ZipFile]::OpenRead((Resolve-Path $zip).Path)
try {
    $entries = @($archive.Entries | ForEach-Object { $_.FullName -replace '\\', '/' })
    foreach ($need in 'LICENSE', 'THIRD-PARTY.md', 'README.md', 'docs/REFERENCE.md', 'CHANGELOG.md', 'RUNNING.txt', 'BUILD-INFO.txt', 'phoxium.exe') {
        if ($entries -notcontains "$name/$need") { throw "$zip has no $need (expected $name/$need): run scripts/package.ps1 again." }
    }
    $runningEntry = $archive.GetEntry("$name/RUNNING.txt")
    $reader = New-Object IO.StreamReader($runningEntry.Open())
    try { $firstLine = $reader.ReadLine() } finally { $reader.Dispose() }
    if ($firstLine -ne "Phoxium $version") { throw "RUNNING.txt of $zip says '$firstLine', expected 'Phoxium $version'." }
} finally { $archive.Dispose() }

# Version agreement: the lock file, the changelog and the tag all name the version of [workspace.package].
$lockText = Get-Content 'Cargo.lock' -Raw
foreach ($crate in 'appres', 'crt-model', 'paramgen', 'pixel-demo', 'phoxium') {
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
# A draft release has no tag yet: the tag appears when the draft is published.
if (Invoke-Native 'git tag' { git tag --list $tag }) { throw "Tag $tag already exists locally." }
if (Invoke-Native 'git ls-remote' { git ls-remote --tags origin "refs/tags/$tag" }) { throw "Tag $tag already exists on origin." }
$existing = Get-Release $tag
$assetFiles = @($zip, $sha)

Write-Host "Target repository: $Repo"
Write-Host "Tag:               $tag (at $head, created when the draft is published)"
Write-Host 'Assets:'
foreach ($f in $assetFiles) { Write-Host ("  {0} ({1} bytes)" -f (Split-Path $f -Leaf), (Get-Item -LiteralPath $f).Length) }
Write-Host "SHA-256 of the zip: $expected"

if ($Publish) {
    # Step 2: the draft made by step 1, for this tag, with these assets.
    if (-not $existing) { throw "No draft release $tag on ${Repo}: run scripts/release.ps1 (without -Publish) first." }
    if (-not $existing.isDraft) { throw "Release $tag on $Repo is already public: nothing to publish." }
    Assert-ReleaseAssets $existing $assetFiles
    if ($DryRun) {
        Write-Host "Dry run: would publish the draft $($existing.url)."
        return
    }
    Invoke-Native 'gh release edit' { gh release edit $tag --repo $Repo --draft=false } | Out-Null
    Invoke-Native 'git fetch' { git fetch origin --quiet --tags } | Out-Null
    $tagged = ((Invoke-Native 'git rev-parse' { git rev-parse "$tag^{commit}" }) -join '').Trim()
    if ($tagged -ne $head) { throw "Published, but tag $tag points to $tagged, not HEAD ${head}: see Recovery in ENV.md." }
    Write-Host "Published $tag on ${Repo}: $((Get-Release $tag).url)"
    return
}

if ($existing) {
    $kind = if ($existing.isDraft) { 'a draft' } else { 'a public release' }
    throw "Release $tag already exists on $Repo ($kind, $($existing.url)). A draft is published with -Publish; a release is never overwritten."
}

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
if ($untested) { $notes += "`n`nThis build was packaged without running the test step (the owner's decision); the last full test run was on an earlier commit." }
$notesFile = Join-Path $env:TEMP "phoxium-notes-$version.md"
[IO.File]::WriteAllText($notesFile, $notes + "`n", (New-Object Text.UTF8Encoding $false))

try {
    if ($DryRun) {
        Write-Host "Dry run: would create the DRAFT release $tag on $Repo at $head with $zip and $sha. Notes:"
        Write-Host $notes
        return
    }
    Invoke-Native 'gh release create' {
        gh release create $tag $zip $sha --repo $Repo --draft --title "Phoxium $tag" --notes-file $notesFile --target $head
    } | Out-Null
    $draft = Get-Release $tag
    if (-not $draft -or -not $draft.isDraft) { throw "gh release create succeeded but no draft $tag is visible on $Repo." }
    Assert-ReleaseAssets $draft $assetFiles
    Write-Host "Draft release created (not public, no tag yet): $($draft.url)"
    Write-Host 'Check it (notes, assets), then run: .\scripts\release.ps1 -Publish'
} finally {
    # Best-effort cleanup: a failure here must never change the exit code.
    try { [IO.File]::Delete($notesFile) } catch { }
}
