<#
.SYNOPSIS
    Impact analysis of a change: selects the checks and tests a change needs, instead of running everything.
.DESCRIPTION
    Reads the changed files (committed since a base, uncommitted, untracked; or an explicit list), maps
    each to the modules, shaders, stages, parameters and presets it can affect, and prints a plan: the
    ordered cargo commands with a cost class and the reasons. With -Run it executes the plan.

    The model is derived from the repository on every run (no hard-coded test lists):
      - cargo workspace crates and their path dependencies (Cargo.toml);
      - the module graph of crates/ultimatecrt/src and crates/crt-model/src (`use crate::`, `super::`);
      - which integration tests reference which module (`ultimatecrt::<module>`, `crt_model::<module>`);
      - shaders: the #include graph, the stage that owns a shader (`<shader>.spv` in the stage Rust file)
        and the tests that name that stage;
      - parameters: ids of params/*.toml against the tests that name them;
      - data files embedded with include_str!/include_bytes! (presets, fonts, shaders read by unit tests).
    Anything the model cannot place falls back to FULL (fmt, clippy workspace, all tests, licence check).

    The test policy (when the plan is enough, when FULL is required) is in ENV.md, "Test policy".

    CARGO_TARGET_DIR defaults to a shared directory (see -TargetDir); CARGO_INCREMENTAL defaults to 0.
.PARAMETER Base
    Git ref to diff against. Default: the merge base with exec/integration, else HEAD~1 (see ENV.md).
.PARAMETER Files
    Explicit list of changed files (repository relative), instead of asking git.
.PARAMETER Full
    Force the full gate.
.PARAMETER Explain
    Print why each test program was selected.
.PARAMETER Run
    Execute the plan. Without it, the plan is only printed.
.PARAMETER DryRun
    Print the plan and the commands that -Run would execute, run nothing.
.PARAMETER KeepGoing
    With -Run, do not stop at the first failing command (cargo test gets --no-fail-fast).
.PARAMETER Rev
    Plan for one commit of history: the diff of <rev>^1..<rev> (a merge shows what it brought in).
.PARAMETER Validate
    Plan the last N first-parent commits of exec/integration and look for tests the plan missed.
.PARAMETER SelfTest
    Run the fixture tests of the model (a small fake repository), then exit.
.PARAMETER PassThru
    Return the plan (or the run result) as an object.
.PARAMETER Root
    Repository root (default: the parent of scripts/). Used by the self test.
.PARAMETER TargetDir
    Shared cargo target directory used when CARGO_TARGET_DIR is not set.
#>
param(
    [string]$Base,
    [string[]]$Files,
    [switch]$Full,
    [switch]$Explain,
    [switch]$Run,
    [switch]$DryRun,
    [switch]$KeepGoing,
    [string]$Rev,
    [int]$Validate = 0,
    [switch]$NoMerges,
    [switch]$SelfTest,
    [switch]$KeepFixture,
    [switch]$PassThru,
    [string]$Root,
    [string]$TargetDir = 'C:\projects\personal\ultimateCRT-wt\target-shared'
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
if (-not $Root) { $Root = Split-Path $PSScriptRoot -Parent }
$Root = (Resolve-Path $Root).Path.TrimEnd('\', '/')

# The packages and the stage layout the model relies on (documented in ENV.md, Test policy).
$MainPackageDir = 'ultimatecrt'
$ModelPackageDir = 'crt-model'
$StagePrefix = 'gpu::stages::'
$ParamModules = @('params', 'settings', 'presets')

# ---------------------------------------------------------------------------------------------------
# Text helpers
# ---------------------------------------------------------------------------------------------------

function Read-Text([string]$path) { [IO.File]::ReadAllText($path) }

# Drops full-line comments and trailing `//` comments (doc comments mention paths that are not code).
function Remove-Comments([string]$text) {
    $out = New-Object System.Collections.Generic.List[string]
    foreach ($l in ($text -split "`r?`n")) {
        if ($l -match '^\s*//') { $out.Add('') }
        else { $out.Add([regex]::Replace($l, '\s//[^"]*$', '')) }
    }
    return ($out -join "`n")
}

function Get-RelPath([string]$base, [string]$full) {
    return ($full.Substring($base.Length).TrimStart('\', '/') -replace '\\', '/')
}

function Add-Reason($table, [string]$key, [string]$reason) {
    if (-not $table.ContainsKey($key)) { $table[$key] = New-Object System.Collections.Generic.List[string] }
    if (-not $table[$key].Contains($reason)) { $table[$key].Add($reason) }
}

# ---------------------------------------------------------------------------------------------------
# Rust path references (`crate::a::{b, c::d}`), expanded to full paths
# ---------------------------------------------------------------------------------------------------

function Read-UseTree([string[]]$base, $out) {
    $segs = New-Object System.Collections.Generic.List[string]
    foreach ($b in $base) { $segs.Add($b) }
    while ($true) {
        while ($script:ti -lt $script:ts.Length -and [char]::IsWhiteSpace($script:ts[$script:ti])) { $script:ti++ }
        if ($script:ti -ge $script:ts.Length) { break }
        $c = $script:ts[$script:ti]
        if ($c -eq '{') {
            $script:ti++
            while ($true) {
                Read-UseTree $segs.ToArray() $out
                while ($script:ti -lt $script:ts.Length -and [char]::IsWhiteSpace($script:ts[$script:ti])) { $script:ti++ }
                if ($script:ti -lt $script:ts.Length -and $script:ts[$script:ti] -eq ',') { $script:ti++; continue }
                if ($script:ti -lt $script:ts.Length -and $script:ts[$script:ti] -eq '}') { $script:ti++ }
                break
            }
            return
        } elseif ($c -match '[A-Za-z0-9_]') {
            $start = $script:ti
            while ($script:ti -lt $script:ts.Length -and $script:ts[$script:ti] -match '[A-Za-z0-9_]') { $script:ti++ }
            $id = $script:ts.Substring($start, $script:ti - $start)
            if ($id -ne 'self' -or $segs.Count -eq 0) { $segs.Add($id) }
            $rest = $script:ts.Substring($script:ti, [Math]::Min(40, $script:ts.Length - $script:ti))
            $am = [regex]::Match($rest, '^\s+as\s+[A-Za-z0-9_]+')
            if ($am.Success) { $script:ti += $am.Length }
            if ($script:ti + 1 -lt $script:ts.Length -and $script:ts[$script:ti] -eq ':' -and $script:ts[$script:ti + 1] -eq ':') {
                $script:ti += 2
                continue
            }
            break
        } elseif ($c -eq '*') {
            $script:ti++
            break
        } else { break }
    }
    if ($segs.Count -gt 0) { $out.Add($segs.ToArray()) }
}

# All paths that start with `<prefix>::` in $text, each as a segment array including the prefix.
function Get-RustRefs([string]$text, [string]$prefix) {
    $out = New-Object System.Collections.Generic.List[object]
    $rx = '(?<![A-Za-z0-9_])' + [regex]::Escape($prefix) + '::'
    foreach ($m in [regex]::Matches($text, $rx)) {
        $script:ts = $text
        $script:ti = $m.Index + $m.Length
        Read-UseTree @($prefix) $out
    }
    return , $out
}

function Resolve-LongestNode([string[]]$segs, $nodes) {
    for ($n = $segs.Count; $n -ge 1; $n--) {
        $name = ($segs[0..($n - 1)] -join '::')
        if ($nodes.ContainsKey($name)) { return $name }
    }
    return $null
}

# Resolves a path found inside module $cur of a crate (crate::, self::, super::).
function Resolve-Internal([string[]]$segs, [string]$cur, $nodes) {
    $first = $segs[0]
    $rest = @()
    if ($segs.Count -gt 1) { $rest = $segs[1..($segs.Count - 1)] }
    if ($first -eq 'crate') { $base = @() }
    else {
        $base = @(); if ($cur) { $base = @($cur -split '::') }
        if ($first -eq 'super') {
            if ($base.Count -gt 0) { $base = @($base[0..($base.Count - 2)] | Where-Object { $_ }) }
            while ($rest.Count -gt 0 -and $rest[0] -eq 'super') {
                if ($base.Count -gt 0) { $base = @($base[0..($base.Count - 2)] | Where-Object { $_ }) }
                if ($rest.Count -gt 1) { $rest = $rest[1..($rest.Count - 1)] } else { $rest = @() }
            }
        }
    }
    $full = @($base) + @($rest)
    if ($full.Count -eq 0) { return $null }
    return (Resolve-LongestNode $full $nodes)
}

# External reference to a crate by name (`ultimatecrt::gpu::X`): the module it resolves to, '' for a root item.
function Get-ExternalRefs([string]$text, [string]$crateIdent, $nodes) {
    $set = New-Object System.Collections.Generic.HashSet[string]
    foreach ($segs in (Get-RustRefs $text $crateIdent)) {
        $rest = @(); if ($segs.Count -gt 1) { $rest = @($segs[1..($segs.Count - 1)]) }
        if ($rest.Count -eq 0) { [void]$set.Add(''); continue }
        $r = Resolve-LongestNode $rest $nodes
        if ($r) { [void]$set.Add($r) } else { [void]$set.Add('') }
    }
    return , $set
}

# ---------------------------------------------------------------------------------------------------
# Repository model
# ---------------------------------------------------------------------------------------------------

function Get-Modules([string]$srcDir, [string]$crateIdent) {
    $m = @{ Nodes = @{}; Lib = $null; Main = $null }
    if (-not (Test-Path $srcDir)) { return $m }
    foreach ($f in Get-ChildItem $srcDir -Recurse -Filter *.rs) {
        $rel = (Get-RelPath $srcDir $f.FullName) -replace '\.rs$', ''
        $raw = Read-Text $f.FullName
        $entry = @{ File = ($rel + '.rs'); Raw = $raw; Text = (Remove-Comments $raw); HasTests = ($raw -match '#\[cfg\(test\)\]') }
        if ($rel -eq 'lib') { $entry.Name = '<lib>'; $m.Lib = $entry; continue }
        if ($rel -eq 'main') { $entry.Name = '<main>'; $m.Main = $entry; continue }
        $parts = @($rel -split '/')
        if ($parts[-1] -eq 'mod' -and $parts.Count -gt 1) { $parts = @($parts[0..($parts.Count - 2)]) }
        $entry.Name = ($parts -join '::')
        $entry.Deps = New-Object System.Collections.Generic.HashSet[string]
        $m.Nodes[$entry.Name] = $entry
    }
    foreach ($n in $m.Nodes.Values) {
        # A module depends on the children it declares (mod child;): a child change reaches its users.
        if ($n.Name -match '^(.+)::([^:]+)$') {
            $par = $Matches[1]; $child = $Matches[2]
            if ($m.Nodes.ContainsKey($par) -and $m.Nodes[$par].Text -match ('\bmod\s+' + [regex]::Escape($child) + '\s*;')) { [void]$m.Nodes[$par].Deps.Add($n.Name) }
        }
        foreach ($prefix in 'crate', 'super', 'self') {
            foreach ($segs in (Get-RustRefs $n.Text $prefix)) {
                $r = Resolve-Internal $segs $n.Name $m.Nodes
                if ($r -and $r -ne $n.Name) { [void]$n.Deps.Add($r) }
            }
        }
    }
    return $m
}

# Modules that depend on $name directly (depth 1) and through one more hop (depth 2).
function Get-Dependants([string]$name, $nodes, [int]$maxDepth) {
    $result = @{}
    $frontier = @($name)
    for ($d = 1; $d -le $maxDepth; $d++) {
        $next = @()
        foreach ($n in $nodes.Values) {
            if ($n.Name -eq $name -or $result.ContainsKey($n.Name)) { continue }
            foreach ($t in $frontier) {
                if ($n.Deps.Contains($t)) { $result[$n.Name] = $d; $next += $n.Name; break }
            }
        }
        $frontier = $next
    }
    return $result
}

function New-Model([string]$Root) {
    $M = @{ Root = $Root }
    # Packages
    $M.PkgByDir = @{}
    foreach ($d in Get-ChildItem (Join-Path $Root 'crates') -Directory -ErrorAction SilentlyContinue) {
        $ct = Join-Path $d.FullName 'Cargo.toml'
        if (-not (Test-Path $ct)) { continue }
        $txt = Read-Text $ct
        $name = $d.Name
        if ($txt -match '(?m)^\s*name\s*=\s*"([^"]+)"') { $name = $Matches[1] }
        $deps = @()
        foreach ($mm in [regex]::Matches($txt, '(?m)^\s*[A-Za-z0-9_-]+\s*=\s*\{[^}]*path\s*=\s*"\.\./([^"/]+)"')) { $deps += $mm.Groups[1].Value }
        $M.PkgByDir[$d.Name] = @{ Dir = $d.Name; Name = $name; DepDirs = $deps }
    }
    # Reverse dependants (transitive) of a package dir, as package names (the package itself first).
    $M.Dependants = @{}
    foreach ($dir in $M.PkgByDir.Keys) {
        $set = New-Object System.Collections.Generic.List[string]
        $set.Add($M.PkgByDir[$dir].Name)
        $changed = $true
        $dirs = @($dir)
        while ($changed) {
            $changed = $false
            foreach ($p in $M.PkgByDir.Values) {
                foreach ($dd in $p.DepDirs) {
                    if ($dirs -contains $dd -and -not ($dirs -contains $p.Dir)) { $dirs += $p.Dir; $set.Add($p.Name); $changed = $true }
                }
            }
        }
        $M.Dependants[$dir] = $set.ToArray()
    }
    # Module graphs
    $ucDir = Join-Path $Root "crates/$MainPackageDir"
    $M.Uc = Get-Modules (Join-Path $ucDir 'src') 'ultimatecrt'
    $M.Cm = Get-Modules (Join-Path $Root "crates/$ModelPackageDir/src") 'crt_model'
    $M.UcCrateIdent = $MainPackageDir -replace '-', '_'
    $M.CmCrateIdent = $ModelPackageDir -replace '-', '_'
    # Integration tests of the main package
    $M.Tests = @{}
    $testsDir = Join-Path $ucDir 'tests'
    $common = $null
    if (Test-Path (Join-Path $testsDir 'common')) {
        $cfile = Get-ChildItem (Join-Path $testsDir 'common') -Filter *.rs | Select-Object -First 1
        if ($cfile) {
            $ct = Remove-Comments (Read-Text $cfile.FullName)
            $common = @{
                Uc = (Get-ExternalRefs $ct $M.UcCrateIdent $M.Uc.Nodes)
                Cm = (Get-ExternalRefs $ct $M.CmCrateIdent $M.Cm.Nodes)
                Gpu = ($ct -match 'Headless|gpu_required|gpu_or_skip')
            }
        }
    }
    if (Test-Path $testsDir) {
        foreach ($f in Get-ChildItem $testsDir -Filter *.rs -File) {
            $raw = Read-Text $f.FullName
            $txt = Remove-Comments $raw
            $uses = $txt -match '(?m)^\s*mod\s+common\s*;'
            $t = @{
                Name = $f.BaseName
                File = "crates/$MainPackageDir/tests/$($f.Name)"
                Text = $txt
                Lines = ($raw -split "`n").Count
                Uc = (Get-ExternalRefs $txt $M.UcCrateIdent $M.Uc.Nodes)
                Cm = (Get-ExternalRefs $txt $M.CmCrateIdent $M.Cm.Nodes)
                UsesCommon = $uses
            }
            if ($uses -and $common) {
                foreach ($x in $common.Uc) { [void]$t.Uc.Add($x) }
                foreach ($x in $common.Cm) { [void]$t.Cm.Add($x) }
            }
            $t.Gpu = ($txt -match 'Headless|gpu_required|gpu_or_skip') -or ($uses -and $common -and $common.Gpu)
            # An "after: "gun"" input is an injection point: that stage is not run by the test.
            $t.RunText = [regex]::Replace($txt, 'after:\s*"[A-Za-z0-9_]+"', 'after: _')
            $t.Process = ($txt -match 'CARGO_BIN_EXE|Command::new')
            $t.RenderSource = ($txt -match 'render_source')
            $t.EnableOnly = ($txt -match 'enable_only')
            # Chain test: runs the whole pipeline (no stage filter) or the offline render.
            $t.Chain = $t.Gpu -and ((-not $t.EnableOnly) -or $t.RenderSource)
            $t.Cost = 'cheap'
            if ($t.Gpu) {
                $t.Cost = 'GPU'
                if ($t.Process -or $t.RenderSource -or $t.Lines -ge 900) { $t.Cost = 'slow' }
            }
            $M.Tests[$t.Name] = $t
        }
    }
    # Measured times override the static cost class (a GPU test over 20 s is slow, one under it is not).
    $timings = Get-Timings
    foreach ($t in $M.Tests.Values) {
        if ($timings.ContainsKey($t.Name)) {
            $t.Seconds = $timings[$t.Name]
            if ($t.Gpu) { if ($t.Seconds -ge 20) { $t.Cost = 'slow' } else { $t.Cost = 'GPU' } }
        }
    }
    # Shaders and their includes
    $M.Shaders = @{}
    $shDir = Join-Path $Root 'shaders'
    if (Test-Path $shDir) {
        foreach ($f in Get-ChildItem $shDir -Recurse -File) {
            $rel = Get-RelPath $shDir $f.FullName
            $txt = Read-Text $f.FullName
            $inc = @()
            foreach ($mm in [regex]::Matches($txt, '(?m)^\s*#\s*include\s+"([^"]+)"')) { $inc += (Split-Path $mm.Groups[1].Value -Leaf) }
            $M.Shaders[$f.Name] = @{ Rel = "shaders/$rel"; Includes = $inc; Top = ($rel -notmatch '/') -and ($f.Extension -in '.comp', '.vert', '.frag') }
        }
    }
    # Parameter ids per params file
    $M.Params = @{}
    $pdir = Join-Path $Root 'params'
    if (Test-Path $pdir) {
        foreach ($f in Get-ChildItem $pdir -Filter *.toml -File) {
            $M.Params[$f.Name] = Read-Text $f.FullName
        }
    }
    return $M
}

# ---------------------------------------------------------------------------------------------------
# Impact model
# ---------------------------------------------------------------------------------------------------

# Measured run times of test programs (seconds), kept next to the shared target folder by -Run. Used to
# classify a test as slow and to estimate the cost of a plan. Absent file: the static classification only.
$script:TimingsFile = $null

function Get-Timings {
    $h = @{}
    if ($script:TimingsFile -and (Test-Path $script:TimingsFile)) {
        try {
            $o = (Read-Text $script:TimingsFile) | ConvertFrom-Json
            foreach ($p in $o.PSObject.Properties) { $h[$p.Name] = [double]$p.Value }
        } catch { }
    }
    return $h
}

function Save-Timings($new) {
    if (-not $script:TimingsFile -or $new.Count -eq 0) { return }
    $h = Get-Timings
    foreach ($k in $new.Keys) { $h[$k] = $new[$k] }
    try {
        $dir = Split-Path $script:TimingsFile -Parent
        if (-not (Test-Path $dir)) { New-Item -ItemType Directory -Force -Path $dir | Out-Null }
        $o = [ordered]@{}
        foreach ($k in ($h.Keys | Sort-Object)) { $o[$k] = $h[$k] }
        [IO.File]::WriteAllText($script:TimingsFile, ($o | ConvertTo-Json))
    } catch { }
}

function New-Selection {
    return @{
        Files = (New-Object System.Collections.Generic.List[string])
        Tests = @{}      # integration test name -> reasons
        Lib = @{}        # lib filter (module path) -> reasons
        LibAll = $false
        Bin = $false
        Pkg = @{}        # whole package tests: package name -> reasons
        UcAll = $false   # the whole main package
        Clippy = @{}     # package name -> reasons
        CheckWs = $false
        Full = (New-Object System.Collections.Generic.List[string])
        Ps = (New-Object System.Collections.Generic.List[string])
        Notes = (New-Object System.Collections.Generic.List[string])
    }
}

function Test-Matches-Node($t, [string]$node, [bool]$ancestors) {
    foreach ($r in $t.Uc) {
        if ($r -eq '') { continue }
        if ($r -eq $node) { return $true }
        if ($r.StartsWith($node + '::')) { return $true }
        if ($ancestors -and $node.StartsWith($r + '::')) { return $true }
    }
    return $false
}

function Test-StageNode([string]$node) { return $node.StartsWith($StagePrefix) }

function Add-Lib($M, $S, [string]$node, [string]$reason) {
    if ($M.Uc.Nodes.ContainsKey($node) -and $M.Uc.Nodes[$node].HasTests) { Add-Reason $S.Lib ($node + '::') $reason }
}

# Tests of one stage: the tests that name it, plus the whole-chain tests.
function Add-Stage($M, $S, [string]$stage, [string]$reason) {
    Add-Lib $M $S ($StagePrefix + $stage) $reason
    foreach ($t in $M.Tests.Values) {
        $why = $null
        if ($t.RunText -match ('"' + [regex]::Escape($stage) + '"')) { $why = "names stage `"$stage`"" }
        elseif ($t.Name -eq "stage_$stage") { $why = "named for stage $stage" }
        else {
            foreach ($sn in $M.Shaders.Keys) {
                if ($M.Shaders[$sn].Top -and $sn -match ('^' + [regex]::Escape($stage) + '\.') -and $t.Text -match [regex]::Escape($sn)) { $why = "mentions $sn"; break }
            }
        }
        if ($why) { Add-Reason $S.Tests $t.Name "$reason; $why" }
        elseif ($t.Chain) { Add-Reason $S.Tests $t.Name "$reason; runs the whole chain" }
    }
}

# A changed non-stage module: its unit tests, the tests that reference it, and its dependants (depth <= 2).
function Add-Node($M, $S, [string]$node, [string]$reason) {
    $nodes = $M.Uc.Nodes
    Add-Lib $M $S $node $reason
    foreach ($t in $M.Tests.Values) {
        if (Test-Matches-Node $t $node $true) { Add-Reason $S.Tests $t.Name "$reason; references ultimatecrt::$node" }
    }
    $deps = Get-Dependants $node $nodes 2
    foreach ($d in $deps.Keys) {
        $tag = "transitive (depth $($deps[$d])): $d uses $node"
        if (Test-StageNode $d) {
            $sn = $d.Substring($StagePrefix.Length)
            if ($sn -ne '' -and $sn -notmatch '::') { Add-Stage $M $S $sn "$reason, $tag" }
            continue
        }
        Add-Lib $M $S $d "$reason, $tag"
        foreach ($t in $M.Tests.Values) {
            if (Test-Matches-Node $t $d $true) { Add-Reason $S.Tests $t.Name "$reason, $tag; references ultimatecrt::$d" }
        }
    }
}

function Add-ClippyFor($M, $S, [string]$dir, [string]$reason) {
    if ($M.Dependants.ContainsKey($dir)) {
        foreach ($p in $M.Dependants[$dir]) { Add-Reason $S.Clippy $p $reason }
    }
}

# Source files (Rust) that reference a data file: include_str!/include_bytes!/path strings with its name.
function Get-Consumers($M, [string]$relPath) {
    $leaf = Split-Path $relPath -Leaf
    $rx = '[/"\\]' + [regex]::Escape($leaf) + '["\.]'
    $res = @{ Nodes = @(); Tests = @() }
    foreach ($n in $M.Uc.Nodes.Values) {
        if ($n.Text -match $rx -and $n.Text -match 'include_(str|bytes)!|read_to_string|read_dir|File::open') { $res.Nodes += $n.Name }
    }
    foreach ($t in $M.Tests.Values) {
        if ($t.Text -match $rx) { $res.Tests += $t.Name }
    }
    return $res
}

function Get-ChangedParamIds($M, [string]$file, [string]$diffBase, [string]$rev) {
    # Parameter ids of the changed blocks (all ids of the file when the diff is not available).
    $text = $M.Params[(Split-Path $file -Leaf)]
    $all = @()
    if ($text) { foreach ($mm in [regex]::Matches($text, '(?m)^\s*id\s*=\s*"([^"]+)"')) { $all += $mm.Groups[1].Value } }
    if (-not $diffBase) { return $all }
    $args2 = @('-C', $M.Root, 'diff', '-U0', '--no-renames')
    if ($rev) { $args2 += @($diffBase, $rev) } else { $args2 += $diffBase }
    $args2 += @('--', $file)
    $d = & git @args2 2>$null
    if ($LASTEXITCODE -ne 0 -or -not $d) { return $all }
    $ids = New-Object System.Collections.Generic.HashSet[string]
    $lines = @($text -split "`r?`n")
    $blockOf = @{}
    $cur = $null
    for ($i = 0; $i -lt $lines.Count; $i++) {
        if ($lines[$i] -match '^\s*\[\[param\]\]') { $cur = $i }
        $blockOf[$i + 1] = $cur
    }
    $blockId = @{}
    foreach ($start in ($blockOf.Values | Where-Object { $_ -ne $null } | Sort-Object -Unique)) {
        for ($j = $start; $j -lt [Math]::Min($lines.Count, $start + 40); $j++) {
            if ($lines[$j] -match '^\s*id\s*=\s*"([^"]+)"') { $blockId[$start] = $Matches[1]; break }
            if ($j -gt $start -and $lines[$j] -match '^\s*\[\[') { break }
        }
    }
    $header = $false
    foreach ($l in @($d)) {
        $s = "$l"
        if ($s -match '^@@ -\d+(?:,\d+)? \+(\d+)(?:,(\d+))? @@') {
            $from = [int]$Matches[1]
            $cnt = 1; if ($Matches[2] -ne '') { $cnt = [int]$Matches[2] }
            if ($cnt -eq 0) { $cnt = 1 }
            for ($k = $from; $k -lt ($from + $cnt); $k++) {
                if ($blockOf.ContainsKey($k) -and $blockOf[$k] -ne $null -and $blockId.ContainsKey($blockOf[$k])) { [void]$ids.Add($blockId[$blockOf[$k]]) }
            }
        } elseif ($s -match '^-\s*id\s*=\s*"([^"]+)"') { [void]$ids.Add($Matches[1]) }
        elseif ($s -match '^\+\s*id\s*=\s*"([^"]+)"') { [void]$ids.Add($Matches[1]) }
    }
    return @($ids)
}

# Plans one file. $ctx carries the git context for finer decisions (changed parameter ids).
function Add-FileImpact($M, $S, [string]$f, $ctx) {
    $p = $f
    $ucPrefix = "crates/$MainPackageDir/"
    # 1. Nothing to test
    if ($p -match '\.(md|txt)$' -or $p -match '(^|/)\.(gitignore|gitattributes)$' -or $p -match '(^|/)LICENSE') {
        $S.Files.Add("$p -> documentation, no tests"); return
    }
    if ($p -match '^crates/[^/]+/assets/.*\.py$') { $S.Files.Add("$p -> asset recipe (not part of the build), no tests"); return }
    if ($p -match '^spikes/') { $S.Files.Add("$p -> spike (own workspace, not in the product build), no tests"); return }
    if ($p -match '^scripts/') {
        if ($p -match '\.ps1$') { $S.Ps.Add($p); $S.Files.Add("$p -> script: PowerShell parse check only") }
        else { $S.Files.Add("$p -> script data, no cargo tests") }
        return
    }
    # 2. Build configuration: everything
    if ($p -match '^(Cargo\.toml|Cargo\.lock|rust-toolchain\.toml|rustfmt\.toml|deny\.toml|clippy\.toml)$' -or $p -match '^\.cargo/' -or $p -match '^crates/[^/]+/Cargo\.toml$' -or $p -match '(^|/)build\.rs$') {
        $S.Full.Add("$p changes the build configuration"); $S.Files.Add("$p -> FULL (build configuration)"); return
    }
    # 3. Crates
    if ($p -match '^crates/([^/]+)/(.+)$') {
        $dir = $Matches[1]; $rest = $Matches[2]
        if (-not $M.PkgByDir.ContainsKey($dir)) { $S.Full.Add("$p is in an unknown crate"); $S.Files.Add("$p -> FULL (unknown crate)"); return }
        $pkg = $M.PkgByDir[$dir].Name
        if ($dir -eq 'pixel-demo') {
            Add-Reason $S.Pkg $pkg "pixel-demo changed ($p)"
            if ($rest -match '\.rs$') { Add-ClippyFor $M $S $dir "$p changed" }
            $S.Files.Add("$p -> pixel-demo tests")
            if ($rest -eq 'src/options.rs') {
                $users = @($M.Tests.Values | Where-Object { $_.Text -match 'pixel-demo|pixel_demo' } | ForEach-Object { $_.Name })
                foreach ($u in $users) { Add-Reason $S.Tests $u "pixel-demo CLI (options.rs) changed and the test references pixel-demo" }
                $scripts = @(Get-ChildItem (Join-Path $M.Root 'scripts') -Filter *.ps1 -ErrorAction SilentlyContinue | Where-Object { $_.Name -notlike 'impact*' -and (Read-Text $_.FullName) -match 'pixel-demo' } | ForEach-Object { $_.Name })
                if ($scripts.Count -gt 0) { $S.Notes.Add("pixel-demo CLI changed: live scripts that use it (not run by the plan): " + ($scripts -join ', ')) }
            }
            return
        }
        if ($dir -eq $ModelPackageDir) {
            if ($rest -match '^src/(.+)\.rs$') {
                $rel = $Matches[1]
                Add-Reason $S.Pkg $pkg "crt-model changed ($p)"
                Add-ClippyFor $M $S $dir "$p changed"
                if ($rel -eq 'lib') {
                    foreach ($t in $M.Tests.Values) { if ($t.Cm.Count -gt 0) { Add-Reason $S.Tests $t.Name "crt-model lib.rs changed; references crt_model" } }
                    $S.LibAll = $true
                    $S.Files.Add("$p -> crt-model tests + every test referencing crt_model")
                    return
                }
                $parts = @($rel -split '/'); if ($parts[-1] -eq 'mod' -and $parts.Count -gt 1) { $parts = @($parts[0..($parts.Count - 2)]) }
                $node = $parts -join '::'
                $set = @{}; $set[$node] = 0
                $deps = Get-Dependants $node $M.Cm.Nodes 2
                foreach ($k in $deps.Keys) { $set[$k] = $deps[$k] }
                foreach ($k in $set.Keys) {
                    $tag = if ($set[$k] -eq 0) { "crt_model::$k changed" } else { "transitive (depth $($set[$k])): crt_model::$k uses crt_model::$node" }
                    foreach ($t in $M.Tests.Values) {
                        if ($t.Cm.Contains($k) -or $t.Cm.Contains('')) { Add-Reason $S.Tests $t.Name "$tag; references crt_model::$k" }
                    }
                    foreach ($n in $M.Uc.Nodes.Values) {
                        $refs = Get-ExternalRefs $n.Text $M.CmCrateIdent $M.Cm.Nodes
                        if ($refs.Contains($k) -or $refs.Contains('')) { Add-Lib $M $S $n.Name "$tag; ultimatecrt::$($n.Name) references crt_model::$k" }
                    }
                }
                $S.Files.Add("$p -> crt-model::$node, tests referencing crt_model::$node and its dependants")
                return
            }
            Add-Reason $S.Pkg $pkg "crt-model changed ($p)"
            $S.Files.Add("$p -> crt-model tests"); return
        }
        if ($dir -eq 'paramgen') {
            Add-Reason $S.Pkg $pkg "paramgen changed ($p)"
            Add-ClippyFor $M $S $dir "$p changed"
            $S.CheckWs = $true
            foreach ($t in $M.Tests.Values) { if ($t.Uc -contains 'params') { Add-Reason $S.Tests $t.Name "paramgen changed; references ultimatecrt::params" } }
            foreach ($pm in $ParamModules) { Add-Lib $M $S $pm "paramgen changed" }
            $S.Files.Add("$p -> paramgen tests, cargo check workspace, params-related tests"); return
        }
        if ($dir -eq $MainPackageDir) {
            if ($rest -eq 'src/lib.rs') {
                $S.UcAll = $true; Add-Reason $S.Pkg $pkg 'lib.rs changed (module list)'; Add-ClippyFor $M $S $dir "$p changed"
                $S.Files.Add("$p -> all ultimatecrt tests (module list changed)"); return
            }
            if ($rest -eq 'src/main.rs') {
                $S.Bin = $true; Add-ClippyFor $M $S $dir "$p changed"
                foreach ($t in $M.Tests.Values) { if ($t.Process) { Add-Reason $S.Tests $t.Name "main.rs changed; the test runs the binary" } }
                $S.Files.Add("$p -> binary tests and tests that run the binary"); return
            }
            if ($rest -match '^src/(.+)\.rs$') {
                $node = $null
                foreach ($n in $M.Uc.Nodes.Values) { if ($n.File -eq ($Matches[1] + '.rs')) { $node = $n.Name } }
                Add-ClippyFor $M $S $dir "$p changed"
                if (-not $node) { $S.Full.Add("$p is a source file the module model cannot place"); $S.Files.Add("$p -> FULL (unplaced source file)"); return }
                if ((Test-StageNode $node) -and $node -notmatch ('^' + [regex]::Escape($StagePrefix) + '[^:]+$') ) {
                    Add-Node $M $S $node "$p changed"; $S.Files.Add("$p -> module $node"); return
                }
                if (Test-StageNode $node) {
                    $sn = $node.Substring($StagePrefix.Length)
                    Add-Stage $M $S $sn "stage $sn changed ($p)"
                    $S.Files.Add("$p -> stage ${sn}: its unit tests, tests naming it, whole-chain tests")
                } else {
                    Add-Node $M $S $node "module $node changed ($p)"
                    $S.Files.Add("$p -> module $node, tests referencing it, dependants (depth 2)")
                }
                return
            }
            if ($rest -match '^tests/common/') {
                foreach ($t in $M.Tests.Values) { if ($t.UsesCommon) { Add-Reason $S.Tests $t.Name "tests/common changed; the test uses it" } }
                Add-Reason $S.Clippy $pkg "$p changed"
                $S.Files.Add("$p -> every test using common"); return
            }
            if ($rest -match '^tests/([^/]+)\.rs$') {
                $name = $Matches[1]
                if (Test-Path (Join-Path $M.Root $p)) { Add-Reason $S.Tests $name "test file changed ($p)" }
                Add-Reason $S.Clippy $pkg "$p changed"
                $S.Files.Add("$p -> that test program"); return
            }
            if ($rest -match '^assets/') {
                $c = Get-Consumers $M $p
                if ($c.Nodes.Count -eq 0 -and $c.Tests.Count -eq 0) { $S.Full.Add("$p is an asset no Rust file references"); $S.Files.Add("$p -> FULL (asset with no consumer)"); return }
                foreach ($n in $c.Nodes) { Add-Node $M $S $n "asset $p embedded by $n" }
                foreach ($t in $c.Tests) { Add-Reason $S.Tests $t "references asset $p" }
                $S.Files.Add("$p -> consumers: " + (($c.Nodes + $c.Tests) -join ', ')); return
            }
        }
    }
    # 4. Shaders
    if ($p -match '^shaders/(.+)$') {
        $name = Split-Path $p -Leaf
        $affected = New-Object System.Collections.Generic.List[string]
        $queue = @($name)
        while ($queue.Count -gt 0) {
            $cur = $queue[0]; $queue = @($queue | Select-Object -Skip 1)
            if ($M.Shaders.ContainsKey($cur) -and $M.Shaders[$cur].Top -and -not $affected.Contains($cur)) { $affected.Add($cur) }
            foreach ($sn in $M.Shaders.Keys) {
                if ($M.Shaders[$sn].Includes -contains $cur -and -not $queue.Contains($sn) -and -not $affected.Contains($sn)) { $queue += $sn }
            }
        }
        if ($affected.Count -eq 0 -and -not (Test-Path (Join-Path $M.Root $p))) { $affected.Add($name) }
        $desc = New-Object System.Collections.Generic.List[string]
        if ($affected.Count -eq 0) { $S.Notes.Add("$p is not a shader entry point and nothing includes it: compile check only") }
        foreach ($sh in $affected) {
            $owners = @($M.Uc.Nodes.Values | Where-Object { $_.Text -match ([regex]::Escape($sh) + '\.spv') } | ForEach-Object { $_.Name })
            $why = "shader $sh changed" + $(if ($sh -ne $name) { " (includes $name)" } else { '' })
            if ($owners.Count -eq 0) {
                foreach ($t in $M.Tests.Values) { if ($t.Chain) { Add-Reason $S.Tests $t.Name "$why; no stage owns it, runs the whole chain" } }
                $desc.Add("${sh}: no owner, chain tests")
            }
            foreach ($o in $owners) {
                if ((Test-StageNode $o) -and $o -match ('^' + [regex]::Escape($StagePrefix) + '[^:]+$')) {
                    $sn = $o.Substring($StagePrefix.Length)
                    Add-Stage $M $S $sn "$why, owned by stage $sn"
                    $desc.Add("${sh}: stage $sn")
                } else {
                    Add-Node $M $S $o "$why, used by $o"
                    $desc.Add("${sh}: module $o")
                }
            }
        }
        # Unit tests that read the shader sources (include_str! of the file, or a scan of the shaders folder).
        foreach ($n in $M.Uc.Nodes.Values) {
            $reads = ($n.Text -match [regex]::Escape($name) -and $n.Text -match 'include_str!') -or ($n.Text -match 'shaders' -and $n.Text -match 'read_dir')
            if ($reads) { Add-Lib $M $S $n.Name "unit tests read the shader sources ($p)" }
        }
        foreach ($t in $M.Tests.Values) { if ($t.Text -match ('include_str!.*' + [regex]::Escape($name))) { Add-Reason $S.Tests $t.Name "includes the source of $name" } }
        $S.Files.Add("$p -> " + ($desc -join '; '))
        return
    }
    # 5. Parameter table
    if ($p -match '^params/([^/]+\.toml)$') {
        $S.CheckWs = $true
        $ids = @(Get-ChangedParamIds $M $p $ctx.Base $ctx.Rev)
        foreach ($pm in $ParamModules) { Add-Lib $M $S $pm "parameter table changed ($p)" }
        foreach ($id in $ids) {
            $upper = $id.ToUpperInvariant()
            foreach ($t in $M.Tests.Values) {
                if ($t.Text -match ('"' + [regex]::Escape($id) + '"') -or $t.Text -cmatch ('\b' + [regex]::Escape($upper))) { Add-Reason $S.Tests $t.Name "references parameter $id ($p)" }
            }
            foreach ($n in $M.Uc.Nodes.Values) {
                if ($ParamModules -contains $n.Name) { continue }
                if ($n.Text -match ('"' + [regex]::Escape($id) + '"')) { Add-Lib $M $S $n.Name "references parameter $id ($p)" }
            }
        }
        if ($M.Tests.ContainsKey('presets')) { Add-Reason $S.Tests 'presets' "parameter table changed ($p): presets are checked against it" }
        $S.Files.Add("$p -> cargo check workspace, params/settings/presets tests, tests naming: " + $(if ($ids.Count -gt 0) { $ids -join ', ' } else { '(no id changed)' }))
        return
    }
    # 6. Presets
    if ($p -match '^presets/') {
        $c = Get-Consumers $M $p
        $nodes = @($c.Nodes)
        if ($nodes.Count -eq 0) { $S.Full.Add("$p is a preset no Rust file embeds"); $S.Files.Add("$p -> FULL (preset with no consumer)"); return }
        foreach ($n in $nodes) { Add-Node $M $S $n "preset file $p embedded by $n" }
        foreach ($t in $c.Tests) { Add-Reason $S.Tests $t "references $p" }
        foreach ($t in $M.Tests.Values) { if ($t.Text -match 'presets/tubes|all_presets') { Add-Reason $S.Tests $t.Name "iterates the presets" } }
        $S.CheckWs = $false
        $S.Files.Add("$p -> consumers: " + ($nodes -join ', ') + ', tests iterating the presets'); return
    }
    # 7. Anything else
    $S.Full.Add("$p is a file kind the model cannot place"); $S.Files.Add("$p -> FULL (unknown file kind)")
}

# ---------------------------------------------------------------------------------------------------
# Plan
# ---------------------------------------------------------------------------------------------------

function New-Command([string]$kind, [string[]]$cmdArgs, [string]$cost, [string]$title, $reasons, [string[]]$tests) {
    return @{ Kind = $kind; Args = $cmdArgs; Cost = $cost; Title = $title; Reasons = $reasons; Tests = $tests; Env = @{} }
}

function Get-TestUniverse($M) {
    # Test programs: every integration test file plus the unit-test target of each package.
    $u = New-Object System.Collections.Generic.List[string]
    foreach ($t in ($M.Tests.Keys | Sort-Object)) { $u.Add("test:$t") }
    foreach ($dir in ($M.PkgByDir.Keys | Sort-Object)) {
        $src = Join-Path $M.Root "crates/$dir/src"
        $has = $false
        if (Test-Path $src) { foreach ($f in Get-ChildItem $src -Recurse -Filter *.rs) { if ((Read-Text $f.FullName) -match '#\[cfg\(test\)\]') { $has = $true; break } } }
        if ($has) { $u.Add("unit:" + $M.PkgByDir[$dir].Name) }
    }
    return $u
}

function Get-ImpactPlan([string]$Root, [string[]]$ChangedFiles, [switch]$ForceFull, [string]$DiffBase, [string]$DiffRev) {
    $M = New-Model $Root
    $S = New-Selection
    $ctx = @{ Base = $DiffBase; Rev = $DiffRev }
    $files = @($ChangedFiles | ForEach-Object { (($_ -replace '\\', '/') -replace '^\./', '') } | Where-Object { $_ } | Sort-Object -Unique)
    if ($ForceFull) { $S.Full.Add('-Full requested') }
    foreach ($f in $files) { Add-FileImpact $M $S $f $ctx }
    $universe = Get-TestUniverse $M
    $plan = @{ Files = $files; Selection = $S; Commands = (New-Object System.Collections.Generic.List[object]); Universe = $universe; Selected = (New-Object System.Collections.Generic.List[string]); Model = $M }

    $cmds = $plan.Commands
    $cmds.Add((New-Command 'cargo' @('fmt', '--all', '--', '--check') 'cheap' 'format check' @('always') @()))

    if ($S.Full.Count -gt 0) {
        $cmds.Add((New-Command 'cargo' @('clippy', '--workspace', '--all-targets', '--locked', '--', '-D', 'warnings') 'slow' 'clippy, whole workspace' $S.Full @()))
        $t = New-Command 'cargo' @('test', '--workspace', '--locked') 'slow' 'all tests (GPU required)' $S.Full @()
        $t.Env['UCRT_REQUIRE_GPU'] = '1'
        $cmds.Add($t)
        $cmds.Add((New-Command 'deny' @('deny', '--locked', 'check', 'licenses') 'cheap' 'licence check (cargo-deny, skipped when not installed)' $S.Full @()))
        foreach ($u in $universe) { $plan.Selected.Add($u) }
        return $plan
    }

    # Clippy on the affected packages
    if ($S.Clippy.Count -gt 0) {
        $a = @('clippy')
        foreach ($p in ($S.Clippy.Keys | Sort-Object)) { $a += @('-p', $p) }
        $a += @('--all-targets', '--locked', '--', '-D', 'warnings')
        $why = @(); foreach ($k in ($S.Clippy.Keys | Sort-Object)) { $why += "$k : " + ($S.Clippy[$k][0]) }
        $cmds.Add((New-Command 'cargo' $a 'slow' 'clippy on the affected packages' $why @()))
    }
    if ($S.CheckWs) {
        $cmds.Add((New-Command 'cargo' @('check', '--workspace', '--all-targets', '--locked') 'slow' 'compile everything (parameter table / generator changed)' @('params or paramgen changed') @()))
    }
    # Other packages
    foreach ($p in ($S.Pkg.Keys | Sort-Object)) {
        if ($p -eq $M.PkgByDir[$MainPackageDir].Name) { continue }
        $cmds.Add((New-Command 'cargo' @('test', '-p', $p, '--locked') 'cheap' "tests of package $p" $S.Pkg[$p] @("package:$p")))
        $plan.Selected.Add("unit:$p")
    }
    $ucName = $M.PkgByDir[$MainPackageDir].Name
    if ($S.UcAll) {
        $t = New-Command 'cargo' @('test', '-p', $ucName, '--locked') 'slow' "all tests of $ucName (GPU required)" $S.Pkg[$ucName] @()
        $t.Env['UCRT_REQUIRE_GPU'] = '1'
        $cmds.Add($t)
        foreach ($u in $universe) { if ($u -eq "unit:$ucName" -or $u.StartsWith('test:')) { $plan.Selected.Add($u) } }
        return $plan
    }
    # Unit tests of the main package
    if ($S.Lib.Count -gt 0 -or $S.Bin) {
        $a = @('test', '-p', $ucName, '--locked')
        $reasons = @()
        if ($S.Bin) { $a += @('--bin', $ucName); $reasons += 'binary changed' }
        if ($S.Lib.Count -gt 0) {
            $a += '--lib'
            $a += '--'
            foreach ($k in ($S.Lib.Keys | Sort-Object)) { $a += $k; $reasons += "$k : " + $S.Lib[$k][0] }
        }
        $cmds.Add((New-Command 'cargo' $a 'cheap' "unit tests of $ucName (filtered by module)" $reasons @($S.Lib.Keys | Sort-Object)))
        $plan.Selected.Add("unit:$ucName")
    }
    # Integration tests by cost class
    $byCost = @{ cheap = @(); GPU = @(); slow = @() }
    foreach ($name in ($S.Tests.Keys | Sort-Object)) {
        if (-not $M.Tests.ContainsKey($name)) { continue }
        $byCost[$M.Tests[$name].Cost] += $name
        $plan.Selected.Add("test:$name")
    }
    foreach ($cls in 'cheap', 'GPU', 'slow') {
        $names = @($byCost[$cls])
        if ($names.Count -eq 0) { continue }
        $a = @('test', '-p', $ucName, '--locked')
        foreach ($n in $names) { $a += @('--test', $n) }
        $why = @(); foreach ($n in $names) { $why += "$n : " + $S.Tests[$n][0] }
        $c = New-Command 'cargo' $a $cls "integration tests ($cls)" $why $names
        $known = @($names | Where-Object { $M.Tests[$_].ContainsKey('Seconds') })
        if ($known.Count -gt 0) {
            $sum = 0.0; foreach ($n in $known) { $sum += $M.Tests[$n].Seconds }
            $c.Title += (" (measured {0:N0} s for {1} of {2} programs)" -f $sum, $known.Count, $names.Count)
        }
        $c.Env['UCRT_REQUIRE_GPU'] = '1'
        $cmds.Add($c)
    }
    # PowerShell parse check
    if ($S.Ps.Count -gt 0) {
        $cmds.Add((New-Command 'psparse' @($S.Ps.ToArray()) 'cheap' 'PowerShell parse check of the changed scripts' @($S.Ps.ToArray()) @()))
    }
    return $plan
}

function Format-Command($c) {
    if ($c.Kind -eq 'psparse') { return 'powershell parse check: ' + ($c.Args -join ', ') }
    $line = 'cargo ' + ($c.Args -join ' ')
    if ($c.Env.Count -gt 0) { $line = (($c.Env.Keys | Sort-Object | ForEach-Object { "$_=$($c.Env[$_])" }) -join ' ') + ' ' + $line }
    return $line
}

function Write-Reasons([string]$name, $reasons) {
    foreach ($r in ($reasons | Select-Object -First 2)) { Write-Host ("               - {0}: {1}" -f $name, $r) }
    if ($reasons.Count -gt 2) { Write-Host ("               - {0}: (+{1} more reason(s))" -f $name, ($reasons.Count - 2)) }
}

function Write-Plan($plan, [switch]$Explain, [string]$Header) {
    $S = $plan.Selection
    $skipped = @($plan.Universe | Where-Object { -not $plan.Selected.Contains($_) })
    Write-Host $Header -ForegroundColor Cyan
    Write-Host "Changed files ($($plan.Files.Count)):"
    foreach ($l in $S.Files) { Write-Host "  $l" }
    if ($S.Full.Count -gt 0) {
        Write-Host 'Decision: FULL gate. Reasons:' -ForegroundColor Yellow
        foreach ($r in ($S.Full | Select-Object -Unique)) { Write-Host "  - $r" }
    }
    foreach ($n in $S.Notes) { Write-Host "Note: $n" -ForegroundColor Yellow }
    Write-Host 'Plan:'
    $i = 1
    foreach ($c in $plan.Commands) {
        Write-Host ("  {0}. [{1,-5}] {2}" -f $i, $c.Cost, (Format-Command $c))
        Write-Host ("               {0}" -f $c.Title)
        if ($Explain) {
            foreach ($t in $c.Tests) {
                if ($S.Tests.ContainsKey($t)) { Write-Reasons $t $S.Tests[$t] }
                elseif ($S.Lib.ContainsKey($t)) { Write-Reasons $t $S.Lib[$t] }
                elseif ($t -like 'package:*') { $pk = $t.Substring(8); foreach ($r in $S.Pkg[$pk]) { Write-Host ("               - {0}: {1}" -f $pk, $r) } }
            }
            if ($c.Tests.Count -eq 0) { foreach ($r in ($c.Reasons | Select-Object -First 6)) { Write-Host "               - $r" } }
        }
        $i++
    }
    $sel = @($plan.Selected | Select-Object -Unique)
    Write-Host ("Test programs selected: {0} of {1}; skipped by impact analysis: {2} of {1}" -f $sel.Count, $plan.Universe.Count, $skipped.Count) -ForegroundColor Green
    if ($skipped.Count -gt 0) { Write-Host ("  not run: " + (($skipped | ForEach-Object { $_ -replace '^test:', '' }) -join ', ')) }
}

# ---------------------------------------------------------------------------------------------------
# Running
# ---------------------------------------------------------------------------------------------------

function Invoke-Plan($plan, [switch]$KeepGoing, [switch]$DryRun) {
    $res = @{ Full = ($plan.Selection.Full.Count -gt 0); Pass = 0; Fail = 0; Ignored = 0; Failed = (New-Object System.Collections.Generic.List[string]); Ran = (New-Object System.Collections.Generic.List[string]); Ok = $true }
    foreach ($c in $plan.Commands) {
        Write-Host ("== {0}: {1}" -f $c.Title, (Format-Command $c)) -ForegroundColor Cyan
        if ($DryRun) { continue }
        if ($c.Kind -eq 'psparse') {
            foreach ($f in $c.Args) {
                $path = Join-Path $plan.Model.Root $f
                if (-not (Test-Path $path)) { continue }
                $tokens = $null; $errs = $null
                [void][System.Management.Automation.Language.Parser]::ParseFile($path, [ref]$tokens, [ref]$errs)
                if ($errs -and $errs.Count -gt 0) {
                    $res.Ok = $false; $res.Failed.Add("parse: $f")
                    foreach ($e in $errs) { Write-Host "  $f : $($e.Message) (line $($e.Extent.StartLineNumber))" -ForegroundColor Red }
                } else { Write-Host "  $f parses" }
            }
            if (-not $res.Ok -and -not $KeepGoing) { break }
            continue
        }
        if ($c.Kind -eq 'deny') {
            if (-not (Get-Command cargo-deny -ErrorAction SilentlyContinue)) { Write-Warning 'cargo-deny is not installed: licence check skipped'; continue }
        }
        $cargoArgs = @($c.Args)
        if ($KeepGoing -and $cargoArgs[0] -eq 'test') { $cargoArgs = @($cargoArgs[0], '--no-fail-fast') + @($cargoArgs | Select-Object -Skip 1) }
        $saved = @{}
        foreach ($k in $c.Env.Keys) { $saved[$k] = [Environment]::GetEnvironmentVariable($k); [Environment]::SetEnvironmentVariable($k, $c.Env[$k]) }
        $captured = New-Object System.Collections.Generic.List[string]
        $prev = $ErrorActionPreference; $ErrorActionPreference = 'Continue'
        $label = ''
        try {
            & cargo @cargoArgs 2>&1 | ForEach-Object {
                $line = "$_"
                Write-Host $line
                $captured.Add($line)
            }
            $code = $LASTEXITCODE
        } finally {
            $ErrorActionPreference = $prev
            foreach ($k in $saved.Keys) { [Environment]::SetEnvironmentVariable($k, $saved[$k]) }
        }
        $newTimings = @{}
        $lastTest = $null
        foreach ($line in $captured) {
            if ($line -cmatch 'Running tests[\\/]([A-Za-z0-9_]+)\.rs') { $lastTest = $Matches[1] }
            elseif ($line -cmatch '^\s*Running ') { $lastTest = $null }
            if ($lastTest -and $line -match 'test result: .* finished in ([0-9.]+)s') { $newTimings[$lastTest] = [double]$Matches[1]; $lastTest = $null }
        }
        Save-Timings $newTimings
        foreach ($line in $captured) {
            if ($line -match 'Running (?:unittests )?(\S+)') { $label = $Matches[1] -replace '^tests[\\/]', '' -replace '\.rs$', ''; $res.Ran.Add($label) }
            if ($line -match 'test result: \w+\. (\d+) passed; (\d+) failed; (\d+) ignored') {
                $res.Pass += [int]$Matches[1]; $res.Fail += [int]$Matches[2]; $res.Ignored += [int]$Matches[3]
            }
        }
        if ($code -ne 0) {
            $res.Ok = $false; $res.Failed.Add($c.Title)
            Write-Host "FAILED (exit code $code): $($c.Title)" -ForegroundColor Red
            if (-not $KeepGoing) { break }
        }
    }
    $skipped = @($plan.Universe | Where-Object { -not $plan.Selected.Contains($_) })
    Write-Host ''
    Write-Host '== summary' -ForegroundColor Cyan
    if ($DryRun) { Write-Host 'dry run: nothing executed' }
    else {
        Write-Host ("tests: {0} passed, {1} failed, {2} ignored" -f $res.Pass, $res.Fail, $res.Ignored)
        if ($res.Failed.Count -gt 0) { Write-Host ("failed: " + ($res.Failed -join '; ')) -ForegroundColor Red }
    }
    $selCount = @($plan.Selected | Select-Object -Unique).Count
    Write-Host ("skipped by impact analysis: {0} of {1} test programs (selected {2})" -f $skipped.Count, $plan.Universe.Count, $selCount)
    if ($skipped.Count -gt 0) { Write-Host ("  not run: " + (($skipped | ForEach-Object { $_ -replace '^test:', '' }) -join ', ')) }
    if ($plan.Selection.Full.Count -eq 0) {
        Write-Host 'This was an impact plan, not the full gate: run it with -Full (or scripts\check.ps1 -Full) before a merge to main, a push or a release (ENV.md, Test policy).'
    }
    if ($res.Ok) { Write-Host 'impact OK' -ForegroundColor Green } else { Write-Host 'impact FAILED' -ForegroundColor Red }
    return $res
}

# ---------------------------------------------------------------------------------------------------
# Git
# ---------------------------------------------------------------------------------------------------

function Get-GitLines([string[]]$gitArgs) {
    $prev = $ErrorActionPreference; $ErrorActionPreference = 'Continue'
    try { $out = & git -C $Root @gitArgs 2>$null; $code = $LASTEXITCODE } finally { $ErrorActionPreference = $prev }
    if ($code -ne 0) { return $null }
    return , @($out | Where-Object { $_ })
}

function Get-DefaultBase {
    $head = Get-GitLines @('rev-parse', 'HEAD')
    $dirty = Get-GitLines @('status', '--porcelain')
    $mb = Get-GitLines @('merge-base', 'HEAD', 'exec/integration')
    if ($mb -and $head -and $mb[0] -ne $head[0]) { return $mb[0] }
    if ($dirty -and $dirty.Count -gt 0) { return 'HEAD' }
    $p = Get-GitLines @('rev-parse', 'HEAD~1')
    if ($p) { return $p[0] }
    return 'HEAD'
}

function Get-ChangedFiles([string]$base) {
    $set = New-Object System.Collections.Generic.HashSet[string]
    $d = Get-GitLines @('-c', 'core.quotepath=false', 'diff', '--name-only', '--no-renames', $base)
    if ($d) { foreach ($x in $d) { [void]$set.Add($x) } }
    $u = Get-GitLines @('-c', 'core.quotepath=false', 'ls-files', '--others', '--exclude-standard')
    if ($u) { foreach ($x in $u) { [void]$set.Add($x) } }
    return @($set)
}

# ---------------------------------------------------------------------------------------------------
# History validation: tests the plan left out that still mention something the commit changed
# ---------------------------------------------------------------------------------------------------

function Get-MissCheck($plan, [string]$rev) {
    $M = $plan.Model
    $misses = New-Object System.Collections.Generic.List[string]
    $selectedTests = @($plan.Selected | Where-Object { $_ -like 'test:*' } | ForEach-Object { $_.Substring(5) })
    if ($plan.Selection.Full.Count -gt 0) { return $misses }
    $names = @{}   # identifier -> where it changed
    $parent = "$rev^1"
    foreach ($f in $plan.Files) {
        # Tests can only reference the items of ultimatecrt and crt-model, shaders and parameters.
        if ($f -notmatch '^(crates/(ultimatecrt|crt-model)/|shaders/|params/)') { continue }
        $d = Get-GitLines @('diff', '-U0', '--no-renames', $parent, $rev, '--', $f)
        if (-not $d) { continue }
        foreach ($l in $d) {
            if ($f -match '\.rs$' -and $l -match '^[+-]\s*pub(\([a-z]+\))?\s+(?:const\s+|unsafe\s+)?(?:fn|struct|enum|const|static|type|trait)\s+([A-Za-z_][A-Za-z0-9_]*)') {
                # Only distinctive identifiers (snake_case with an underscore, CamelCase, long): plain words such as "apply" are noise.
                $nm = $Matches[2]
                if (($nm -match '_' -and $nm.Length -ge 6) -or $nm -cmatch '^[A-Z][a-z0-9]+[A-Z]' -or $nm.Length -ge 10) { $names[$nm] = $f }
            }
            if ($f -match '^params/.*\.toml$' -and $l -match '^[+-]\s*id\s*=\s*"([^"]+)"') { $names['"' + $Matches[1] + '"'] = $f }
        }
        if ($f -match '^shaders/') { $names[(Split-Path $f -Leaf)] = $f }
    }
    foreach ($t in $M.Tests.Values) {
        if ($selectedTests -contains $t.Name) { continue }
        foreach ($k in $names.Keys) {
            $pat = if ($k.StartsWith('"')) { [regex]::Escape($k) } elseif ($k -match '\.(comp|vert|frag|glsl)$') { [regex]::Escape($k) } else { '\b' + [regex]::Escape($k) + '\b' }
            if ($t.Text -match $pat) { $misses.Add("test $($t.Name) mentions $k (changed in $($names[$k])) but is not selected"); break }
        }
    }
    return $misses
}

function Invoke-Validate([int]$count, [bool]$noMerges, [bool]$quiet) {
    $ref = 'exec/integration'
    if (-not (Get-GitLines @('rev-parse', '--verify', $ref))) { $ref = 'HEAD' }
    if ($noMerges) { $revs = Get-GitLines @('log', '--no-merges', '--format=%H', '-n', $count, $ref) }
    else { $revs = Get-GitLines @('log', '--first-parent', '--format=%H', '-n', $count, $ref) }
    $summary = @()
    foreach ($r in $revs) {
        $subject = (Get-GitLines @('log', '-1', '--format=%h %s', $r))[0]
        $files = Get-GitLines @('-c', 'core.quotepath=false', 'diff', '--name-only', '--no-renames', "$r^1", $r)
        if (-not $files) { Write-Host "$subject : no changed files"; continue }
        $plan = Get-ImpactPlan -Root $Root -ChangedFiles $files -DiffBase "$r^1" -DiffRev $r
        if (-not $quiet) { Write-Plan $plan -Explain:$Explain -Header "== $subject" }
        $miss = @(Get-MissCheck $plan $r)
        if ($miss.Count -gt 0) { foreach ($x in $miss) { Write-Host "POSSIBLE MISS: $x" -ForegroundColor Red } }
        elseif (-not $quiet) { Write-Host 'miss check: every unselected test program is free of the pub items, shader names and parameter ids this commit changed' -ForegroundColor Green }
        $sel = @($plan.Selected | Select-Object -Unique).Count
        $summary += ("{0}: files {1}, selected {2} of {3}{4}, possible misses {5}" -f $subject, $files.Count, $sel, $plan.Universe.Count, $(if ($plan.Selection.Full.Count -gt 0) { ' (FULL)' } else { '' }), $miss.Count)
        Write-Host ''
    }
    Write-Host '== validation summary' -ForegroundColor Cyan
    $summary | ForEach-Object { Write-Host "  $_" }
}

# ---------------------------------------------------------------------------------------------------
# Self test: a small fake repository, expected selections
# ---------------------------------------------------------------------------------------------------

function Invoke-SelfTest {
    $fx = Join-Path ([IO.Path]::GetTempPath()) ("impact-fixture-" + [Guid]::NewGuid().ToString('N'))
    function W([string]$rel, [string]$content) {
        $p = Join-Path $fx $rel
        New-Item -ItemType Directory -Force -Path (Split-Path $p -Parent) | Out-Null
        [IO.File]::WriteAllText($p, $content)
    }
    try {
        W 'Cargo.toml' "[workspace]`nmembers = [`"crates/crt-model`", `"crates/paramgen`", `"crates/pixel-demo`", `"crates/ultimatecrt`"]`n"
        W 'crates/crt-model/Cargo.toml' "[package]`nname = `"crt-model`"`n"
        W 'crates/crt-model/src/lib.rs' "pub mod abl;`npub mod beam;`npub mod units;`n"
        W 'crates/crt-model/src/abl.rs' "use crate::beam::spot;`n#[cfg(test)]`nmod tests {}`n"
        W 'crates/crt-model/src/beam.rs' "use crate::units::mm;`npub fn spot() {}`n"
        W 'crates/crt-model/src/units.rs' "pub fn mm() {}`n"
        W 'crates/paramgen/Cargo.toml' "[package]`nname = `"paramgen`"`n"
        W 'crates/paramgen/src/lib.rs' "pub fn load() {}`n#[cfg(test)]`nmod tests {}`n"
        W 'crates/pixel-demo/Cargo.toml' "[package]`nname = `"pixel-demo`"`n"
        W 'crates/pixel-demo/src/main.rs' "fn main() {}`n#[cfg(test)]`nmod tests {}`n"
        W 'crates/pixel-demo/src/options.rs' "pub struct Options;`n"
        W 'crates/ultimatecrt/Cargo.toml' "[package]`nname = `"ultimatecrt`"`n[dependencies]`ncrt-model = { path = `"../crt-model`" }`n[build-dependencies]`nparamgen = { path = `"../paramgen`" }`n"
        W 'crates/ultimatecrt/build.rs' "fn main() {}`n"
        W 'crates/ultimatecrt/src/lib.rs' "pub mod gpu;`npub mod params;`npub mod placement;`npub mod presets;`npub mod scaling;`npub mod ui;`n"
        W 'crates/ultimatecrt/src/main.rs' "fn main() { ultimatecrt::gpu::run(); }`n"
        W 'crates/ultimatecrt/src/params.rs' "pub struct Params;`n#[cfg(test)]`nmod tests {}`n"
        W 'crates/ultimatecrt/src/scaling.rs' "pub struct View;`n#[cfg(test)]`nmod tests {}`n"
        W 'crates/ultimatecrt/src/placement.rs' "use crate::scaling::View;`nuse crate::params::Params;`n#[cfg(test)]`nmod tests {}`n"
        W 'crates/ultimatecrt/src/presets.rs' "use crate::params::Params;`nconst P: &str = include_str!(`"../../../presets/tubes/01-a.toml`");`n#[cfg(test)]`nmod tests {}`n"
        W 'crates/ultimatecrt/src/ui/mod.rs' "pub mod menu;`n"
        W 'crates/ultimatecrt/src/ui/menu.rs' "use crate::params::Params;`nuse crate::presets::Library;`n#[cfg(test)]`nmod tests {}`n"
        W 'crates/ultimatecrt/src/gpu/mod.rs' "pub mod stages;`npub mod pipeline;`npub struct Headless;`n"
        W 'crates/ultimatecrt/src/gpu/pipeline.rs' "use super::stages::abl::Abl;`nconst R: &[u8] = include_bytes!(concat!(env!(`"OUT_DIR`"), `"/resample.comp.spv`"));`n"
        W 'crates/ultimatecrt/src/gpu/stages/mod.rs' "pub mod abl;`npub mod screen;`n#[cfg(test)]`nmod tests { fn scan() { std::fs::read_dir(`"shaders`"); } }`n"
        W 'crates/ultimatecrt/src/gpu/stages/abl.rs' "use crate::params::Params;`nconst S: &[u8] = include_bytes!(concat!(env!(`"OUT_DIR`"), `"/abl.comp.spv`"));`nfn name() -> &'static str { `"abl`" }`n#[cfg(test)]`nmod tests {}`n"
        W 'crates/ultimatecrt/src/gpu/stages/screen.rs' "use crate::params::Params;`nconst S: &[u8] = include_bytes!(concat!(env!(`"OUT_DIR`"), `"/screen.comp.spv`"));`nfn name() -> &'static str { `"screen`" }`n#[cfg(test)]`nmod tests {}`n"
        W 'crates/ultimatecrt/tests/common/mod.rs' "use ultimatecrt::gpu::Headless;`npub fn gpu_or_skip() {}`n"
        W 'crates/ultimatecrt/tests/stage_abl.rs' "mod common;`nuse crt_model::abl::Abl;`nfn t() { gpu.enable_only(&[`"abl`"]); let _ = `"gun`"; }`n"
        W 'crates/ultimatecrt/tests/stage_screen.rs' "mod common;`nuse crt_model::beam::Spot;`nfn t() { gpu.enable_only(&[`"screen`"]); }`n"
        W 'crates/ultimatecrt/tests/pipeline_skip.rs' "mod common;`nuse ultimatecrt::gpu::Headless;`nfn t() { gpu.render(); }`n"
        W 'crates/ultimatecrt/tests/placement_modes.rs' "use ultimatecrt::placement::place;`nuse ultimatecrt::scaling::View;`nfn t() {}`n"
        W 'crates/ultimatecrt/tests/ui_menu.rs' "mod common;`nuse ultimatecrt::ui::menu::MenuRects;`nuse ultimatecrt::params::Params;`nfn t() { gpu.render(); }`n"
        W 'crates/ultimatecrt/tests/presets.rs' "mod common;`nuse ultimatecrt::presets::Library;`nfn t() { render_source(); let _ = `"preset`"; }`n"
        W 'crates/ultimatecrt/tests/smoke.rs' "use ultimatecrt::{scaling, params};`nfn t() {}`n"
        W 'crates/ultimatecrt/tests/units_only.rs' "use crt_model::units::mm;`nfn t() {}`n"
        W 'shaders/abl.comp' "#include `"abl.glsl`"`nvoid main() {}`n"
        W 'shaders/screen.comp' "#include `"beam.glsl`"`nvoid main() {}`n"
        W 'shaders/resample.comp' "void main() {}`n"
        W 'shaders/include/common.glsl' "float f;`n"
        W 'shaders/include/abl.glsl' "#include `"common.glsl`"`n"
        W 'shaders/include/beam.glsl' "#include `"common.glsl`"`n"
        W 'params/abl.toml' "[[param]]`nid = `"abl_enable`"`nstage = `"abl`"`n`n[[param]]`nid = `"abl_gain`"`nstage = `"abl`"`n"
        W 'params/beam.toml' "[[param]]`nid = `"beam_width`"`nstage = `"screen`"`n"
        W 'presets/tubes/01-a.toml' "id = `"a`"`n"
        W 'scripts/x.ps1' "Write-Host 1`n"
        W 'README.md' "# x`n"
        W 'CHANGELOG.md' "# c`n"
        W '.gitignore' "target`n"
        W 'ENV.md' "# e`n"
        W 'weird.bin' "x"

        $cases = @(
            @{ Name = 'doc'; Files = @('ENV.md', 'CHANGELOG.md', '.gitignore'); Tests = @(); NotTests = '*'; Cmds = @('fmt'); Full = $false }
            @{ Name = 'script'; Files = @('scripts/x.ps1'); Tests = @(); NotTests = '*'; Ps = $true; NoCargoTest = $true; Full = $false }
            @{ Name = 'pixel-demo file'; Files = @('crates/pixel-demo/src/options.rs'); Tests = @(); NotTests = '*'; Pkg = @('pixel-demo'); Full = $false }
            @{ Name = 'crt-model leaf (units)'; Files = @('crates/crt-model/src/units.rs'); Tests = @('stage_abl', 'stage_screen', 'units_only'); NotTests = @('smoke', 'ui_menu', 'placement_modes'); Pkg = @('crt-model'); Full = $false }
            @{ Name = 'crt-model abl only (no dependants)'; Files = @('crates/crt-model/src/abl.rs'); Tests = @('stage_abl'); NotTests = @('stage_screen', 'units_only', 'smoke'); Pkg = @('crt-model'); Full = $false }
            @{ Name = 'shader include'; Files = @('shaders/include/abl.glsl'); Tests = @('stage_abl', 'pipeline_skip', 'presets', 'ui_menu'); NotTests = @('stage_screen', 'smoke', 'units_only'); Lib = @('gpu::stages::abl::'); Full = $false }
            @{ Name = 'shared shader include (both stages)'; Files = @('shaders/include/common.glsl'); Tests = @('stage_abl', 'stage_screen'); NotTests = @('smoke'); Full = $false }
            @{ Name = 'stage shader'; Files = @('shaders/screen.comp'); Tests = @('stage_screen', 'pipeline_skip'); NotTests = @('stage_abl', 'smoke', 'placement_modes'); Lib = @('gpu::stages::screen::'); Full = $false }
            @{ Name = 'stage rust file'; Files = @('crates/ultimatecrt/src/gpu/stages/abl.rs'); Tests = @('stage_abl', 'pipeline_skip'); NotTests = @('stage_screen', 'smoke'); Lib = @('gpu::stages::abl::'); Full = $false }
            @{ Name = 'params toml (one stage)'; Files = @('params/beam.toml'); Tests = @(); NotTests = @('stage_abl', 'smoke'); Lib = @('params::', 'presets::'); CheckWs = $true; Full = $false }
            @{ Name = 'params toml id named by a test'; Files = @('params/abl.toml'); Tests = @(); NotTests = @('smoke'); CheckWs = $true; Full = $false }
            @{ Name = 'preset'; Files = @('presets/tubes/01-a.toml'); Tests = @('presets', 'ui_menu'); NotTests = @('stage_abl', 'smoke', 'units_only'); Lib = @('presets::'); Full = $false }
            @{ Name = 'ui module'; Files = @('crates/ultimatecrt/src/ui/menu.rs'); Tests = @('ui_menu'); NotTests = @('stage_abl', 'stage_screen', 'smoke', 'placement_modes'); Lib = @('ui::menu::'); Full = $false }
            @{ Name = 'leaf module with dependants (scaling)'; Files = @('crates/ultimatecrt/src/scaling.rs'); Tests = @('smoke', 'placement_modes'); NotTests = @('units_only'); Lib = @('scaling::', 'placement::'); Full = $false }
            @{ Name = 'paramgen'; Files = @('crates/paramgen/src/lib.rs'); Tests = @('smoke', 'ui_menu'); NotTests = @('units_only'); Pkg = @('paramgen'); CheckWs = $true; Full = $false }
            @{ Name = 'tests/common'; Files = @('crates/ultimatecrt/tests/common/mod.rs'); Tests = @('stage_abl', 'stage_screen', 'pipeline_skip', 'ui_menu', 'presets'); NotTests = @('smoke', 'units_only'); Full = $false }
            @{ Name = 'one test file'; Files = @('crates/ultimatecrt/tests/smoke.rs'); Tests = @('smoke'); NotTests = @('stage_abl', 'ui_menu'); Full = $false }
            @{ Name = 'Cargo.lock'; Files = @('Cargo.lock'); Full = $true }
            @{ Name = 'Cargo.toml of a crate'; Files = @('crates/pixel-demo/Cargo.toml'); Full = $true }
            @{ Name = 'build.rs'; Files = @('crates/ultimatecrt/build.rs'); Full = $true }
            @{ Name = 'unknown file kind'; Files = @('weird.bin'); Full = $true }
            @{ Name = 'mixed doc and unknown'; Files = @('ENV.md', 'weird.bin'); Full = $true }
        )
        $fail = 0; $n = 0
        foreach ($c in $cases) {
            $n++
            $plan = Get-ImpactPlan -Root $fx -ChangedFiles $c.Files
            $sel = @($plan.Selected | Where-Object { $_ -like 'test:*' } | ForEach-Object { $_.Substring(5) })
            $problems = @()
            if ($c.Full -ne ($plan.Selection.Full.Count -gt 0)) { $problems += "expected Full=$($c.Full)" }
            if (-not $c.Full) {
                foreach ($t in @($c.Tests)) { if ($t -and ($sel -notcontains $t)) { $problems += "missing test $t" } }
                if ($c.NotTests -eq '*') { if ($sel.Count -gt 0) { $problems += "expected no integration tests, got " + ($sel -join ',') } }
                else { foreach ($t in @($c.NotTests)) { if ($t -and ($sel -contains $t)) { $problems += "unexpected test $t" } } }
                foreach ($k in @($c.Lib)) { if ($k -and -not $plan.Selection.Lib.ContainsKey($k)) { $problems += "missing lib filter $k" } }
                foreach ($k in @($c.Pkg)) { if ($k -and -not $plan.Selection.Pkg.ContainsKey($k)) { $problems += "missing package $k" } }
                if ($c.Ps -and $plan.Selection.Ps.Count -eq 0) { $problems += 'expected a PowerShell parse check' }
                if ($c.NoCargoTest -and @($plan.Commands | Where-Object { $_.Args -and $_.Args[0] -eq 'test' }).Count -gt 0) { $problems += 'expected no cargo test' }
                if ($c.CheckWs -and -not $plan.Selection.CheckWs) { $problems += 'expected cargo check --workspace' }
                if ($c.Name -eq 'doc' -and $plan.Commands.Count -ne 1) { $problems += 'a doc change must only run the format check' }
            }
            if ($c.Name -eq 'params toml id named by a test') {
                # no fixture test names abl_enable: only params/settings/presets and the generic checks must run
                if ($sel -contains 'stage_abl') { $problems += 'stage_abl does not name any parameter id, must not be selected' }
            }
            if ($problems.Count -eq 0) { Write-Host ("  ok   {0,2}. {1}" -f $n, $c.Name) }
            else { $fail++; Write-Host ("  FAIL {0,2}. {1}: {2}" -f $n, $c.Name, ($problems -join '; ')) -ForegroundColor Red }
        }
        Write-Host ("self test: {0} case(s), {1} failure(s)" -f $cases.Count, $fail)
        return $fail
    } finally {
        if ($KeepFixture) { Write-Host "fixture kept: $fx" } else { Remove-Item -Recurse -Force $fx -ErrorAction SilentlyContinue }
    }
}

# ---------------------------------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------------------------------

# Dot-sourcing the script loads the functions only.
if ($MyInvocation.InvocationName -eq '.') { return }

if ($SelfTest) {
    $f = Invoke-SelfTest
    if ($PassThru) { return $f }
    exit ([int]($f -gt 0))
}

if (-not $env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR = $TargetDir }
if (-not $env:CARGO_INCREMENTAL) { $env:CARGO_INCREMENTAL = '0' }
$script:TimingsFile = Join-Path $env:CARGO_TARGET_DIR 'impact-timings.json'
Set-Location $Root

if ($Validate -gt 0) { Invoke-Validate $Validate ([bool]$NoMerges) (-not $Explain); exit 0 }

$diffBase = $null; $diffRev = $null
if ($Files -and $Files.Count -gt 0) {
    $changed = @($Files | ForEach-Object { $_ -split ',' } | Where-Object { $_ })
    $header = "== impact plan for an explicit list of $($changed.Count) file(s)"
} elseif ($Rev) {
    $diffBase = "$Rev^1"; $diffRev = $Rev
    $changed = Get-GitLines @('-c', 'core.quotepath=false', 'diff', '--name-only', '--no-renames', $diffBase, $diffRev)
    if (-not $changed) { $changed = @() }
    $header = "== impact plan for commit $Rev (diff against $diffBase)"
} else {
    if (-not $Base) { $Base = Get-DefaultBase }
    $diffBase = $Base
    $changed = @(Get-ChangedFiles $Base)
    $header = "== impact plan (base $Base, committed, uncommitted and untracked changes)"
}
$plan = Get-ImpactPlan -Root $Root -ChangedFiles $changed -ForceFull:$Full -DiffBase $diffBase -DiffRev $diffRev
Write-Plan $plan -Explain:$Explain -Header $header
Write-Host "cargo target dir: $env:CARGO_TARGET_DIR (CARGO_INCREMENTAL=$env:CARGO_INCREMENTAL)"

if ($Run -or $DryRun) {
    $result = Invoke-Plan $plan -KeepGoing:$KeepGoing -DryRun:$DryRun
    if ($PassThru) { return $result }
    if (-not $result.Ok) { exit 1 }
    exit 0
}
if ($PassThru) { return $plan }
exit 0
