<#
.SYNOPSIS
    Where the scripts look for the worktree folder and the shared cargo target folder. Dot-source it.
.DESCRIPTION
    Nothing here is tied to one machine: everything is derived from the location of the repository.
      - The worktree root is the folder that holds the git worktrees of the repository. It is
        `<parent of the repository>\<repository folder name>-wt` when that folder exists (the repository is the
        main checkout), or the parent folder itself when the repository sits in a folder whose name ends in
        `-wt` (the repository is a worktree). It is $null when there is no such folder.
      - The shared target folder is `<worktree root>\target-shared`, used so that worktrees and agents share the
        compiled dependencies. When there is no worktree root it is $null and cargo's own default applies.
    CARGO_TARGET_DIR, when set, always wins in the scripts that use these functions.
#>

function Get-WorktreeRoot([string]$RepoRoot) {
    $repo = $RepoRoot.TrimEnd('\', '/')
    $parent = Split-Path $repo -Parent
    if (-not $parent) { return $null }
    if ((Split-Path $parent -Leaf) -like '*-wt') { return $parent }
    $candidate = Join-Path $parent ((Split-Path $repo -Leaf) + '-wt')
    if (Test-Path -LiteralPath $candidate -PathType Container) { return $candidate }
    return $null
}

# The main checkout, from a repository that is either the main checkout or a worktree of it.
function Get-MainCheckout([string]$RepoRoot) {
    $repo = $RepoRoot.TrimEnd('\', '/')
    $parent = Split-Path $repo -Parent
    if ($parent -and (Split-Path $parent -Leaf) -like '*-wt') {
        $main = Join-Path (Split-Path $parent -Parent) ((Split-Path $parent -Leaf) -replace '-wt$', '')
        if (Test-Path -LiteralPath $main -PathType Container) { return $main }
    }
    return $repo
}

function Get-SharedTargetDir([string]$RepoRoot) {
    $wt = Get-WorktreeRoot $RepoRoot
    if ($wt) { return (Join-Path $wt 'target-shared') }
    return $null
}
