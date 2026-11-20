<#
.SYNOPSIS
    The licence policy of the dependencies as PowerShell functions, shared by scripts/package.ps1,
    scripts/check.ps1 and scripts/gen-third-party.ps1. Dot-source it; run it with -SelfTest to test the filter.
.DESCRIPTION
    Policy (DESIGN.md 11, deny.toml): every dependency declares a licence, and no GPL-family licence (GPL, LGPL,
    AGPL, in any version or in an OR expression with a permissive one) is accepted unless deny.toml itself allows
    that licence id (its `allow` list or a [[licenses.exceptions]] entry). cargo-deny is the authority when it is
    installed; this filter is the fallback that always runs and the check that package.ps1 cannot skip.
.PARAMETER SelfTest
    Run the tests of the filter and exit with a non-zero code on a failure.
#>
param([switch]$SelfTest)

$ErrorActionPreference = 'Stop'

# GPL-family licence ids that deny.toml allows (an empty list today). Reads every quoted id inside an `allow = [...]`.
function Get-DenyAllowedGpl([string]$denyPath) {
    $ids = New-Object System.Collections.Generic.List[string]
    if (-not (Test-Path $denyPath)) { return @() }
    $text = [IO.File]::ReadAllText($denyPath)
    # Comments are dropped first: a licence named in a comment is not an allowance.
    $text = [regex]::Replace($text, '(?m)#.*$', '')
    foreach ($block in [regex]::Matches($text, '(?s)\ballow\s*=\s*\[(.*?)\]')) {
        foreach ($q in [regex]::Matches($block.Groups[1].Value, '"([^"]+)"')) {
            $id = $q.Groups[1].Value
            if ($id -match '(?i)GPL') { $ids.Add($id) }
        }
    }
    return @($ids)
}

# The GPL-family ids of an SPDX-like expression that are not in $Allowed. Handles "A OR B", "A AND B", "A WITH X",
# parentheses, the older "A/B" form and a trailing "+".
function Find-ForbiddenLicence([string]$Expression, [string[]]$Allowed = @()) {
    $bad = New-Object System.Collections.Generic.List[string]
    foreach ($tok in ($Expression -split '[\s()/]+')) {
        if (-not $tok -or $tok -in 'OR', 'AND', 'WITH') { continue }
        if ($tok -notmatch '(?i)GPL') { continue }
        $id = $tok.TrimEnd('+')
        if ($Allowed -contains $id -or $Allowed -contains $tok) { continue }
        $bad.Add($tok)
    }
    return @($bad)
}

# Throws when a package has no licence field or a forbidden one. $Rows: the `packages` of cargo metadata.
function Assert-DeclaredLicences($Rows, [string]$DenyPath) {
    $missing = @($Rows | Where-Object { -not $_.license -and -not $_.license_file })
    if ($missing.Count -gt 0) { throw "Dependencies without a licence field: $(($missing | ForEach-Object { $_.name }) -join ', ')" }
    $allowed = Get-DenyAllowedGpl $DenyPath
    $found = @()
    foreach ($r in $Rows) {
        if (-not $r.license) { continue }
        $bad = Find-ForbiddenLicence $r.license $allowed
        if ($bad.Count -gt 0) { $found += "$($r.name) $($r.version) ($($r.license))" }
    }
    if ($found.Count -gt 0) {
        throw "GPL-family dependency not allowed by deny.toml: $($found -join '; ') (see DESIGN.md 11, dependencies)"
    }
}

function Invoke-PolicySelfTest {
    $fail = 0
    function Check([string]$name, [bool]$ok) {
        if ($ok) { Write-Host "  ok   $name" } else { $script:policyFail++; Write-Host "  FAIL $name" -ForegroundColor Red }
    }
    $script:policyFail = 0
    Check 'LGPL-2.1-only is rejected' ((Find-ForbiddenLicence 'LGPL-2.1-only').Count -eq 1)
    Check 'MIT OR LGPL-3.0 is rejected' ((Find-ForbiddenLicence 'MIT OR LGPL-3.0').Count -eq 1)
    Check 'GPL-3.0-or-later WITH exception is rejected' ((Find-ForbiddenLicence 'GPL-3.0-or-later WITH GCC-exception-3.1').Count -eq 1)
    Check 'AGPL-3.0+ is rejected' ((Find-ForbiddenLicence 'AGPL-3.0+').Count -eq 1)
    Check '(MIT OR GPL-2.0) AND Zlib is rejected' ((Find-ForbiddenLicence '(MIT OR GPL-2.0) AND Zlib').Count -eq 1)
    Check 'MIT/GPL-2.0 (old form) is rejected' ((Find-ForbiddenLicence 'MIT/GPL-2.0').Count -eq 1)
    Check 'MIT OR Apache-2.0 passes' ((Find-ForbiddenLicence 'MIT OR Apache-2.0').Count -eq 0)
    Check 'Apache-2.0 WITH LLVM-exception passes' ((Find-ForbiddenLicence 'Apache-2.0 WITH LLVM-exception').Count -eq 0)
    Check 'Unicode-3.0 passes' ((Find-ForbiddenLicence 'Unicode-3.0').Count -eq 0)
    Check 'an id allowed by deny.toml passes' ((Find-ForbiddenLicence 'LGPL-2.1-only' @('LGPL-2.1-only')).Count -eq 0)
    Check 'another GPL id is still rejected when one is allowed' ((Find-ForbiddenLicence 'GPL-3.0-only' @('LGPL-2.1-only')).Count -eq 1)
    $tmp = [IO.Path]::GetTempFileName()
    try {
        [IO.File]::WriteAllText($tmp, "[licenses]`n# allow = [`"LGPL-3.0`"] in a comment`nallow = [`"MIT`", `"Apache-2.0`"]`n")
        Check 'a licence named in a deny.toml comment is not an allowance' ((Get-DenyAllowedGpl $tmp).Count -eq 0)
        [IO.File]::WriteAllText($tmp, "[licenses]`nallow = [`"MIT`"]`n[[licenses.exceptions]]`nname = `"x`"`nallow = [`"LGPL-3.0-only`"]`n")
        Check 'an exceptions entry is read' ((Get-DenyAllowedGpl $tmp) -contains 'LGPL-3.0-only')
    } finally { Remove-Item $tmp -ErrorAction SilentlyContinue }
    $rows = @([pscustomobject]@{ name = 'a'; version = '1'; license = 'MIT OR LGPL-3.0'; license_file = $null })
    $threw = $false
    try { Assert-DeclaredLicences $rows (Join-Path (Split-Path $PSScriptRoot -Parent) 'deny.toml') } catch { $threw = $true }
    Check 'Assert-DeclaredLicences throws on MIT OR LGPL-3.0 with the repository deny.toml' $threw
    $rows = @([pscustomobject]@{ name = 'b'; version = '1'; license = $null; license_file = $null })
    $threw = $false
    try { Assert-DeclaredLicences $rows (Join-Path (Split-Path $PSScriptRoot -Parent) 'deny.toml') } catch { $threw = $true }
    Check 'Assert-DeclaredLicences throws on a missing licence field' $threw
    Write-Host "licence policy self test: $($script:policyFail) failure(s)"
    return $script:policyFail
}

if ($SelfTest) {
    $f = Invoke-PolicySelfTest
    exit ([int]($f -gt 0))
}
