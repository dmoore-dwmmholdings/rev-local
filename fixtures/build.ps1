#!/usr/bin/env pwsh
# Windows equivalent of fixtures/build.sh (SPEC §16.3).
#
# This is a DRIVER, not a second copy of the fixture. Both scripts read
# fixtures/content/git-basic/steps.json and copy the same file bodies out of
# fixtures/content/, so the bytes committed are identical by construction rather
# than by two people remembering to keep two scripts in step. Only the git
# invocations live in both places, and fixtures_parity asserts even those agree.
#
# The determinism rules are the same ones build.sh documents: fixed author AND
# committer identity and dates, git config isolated from the machine's own, and an
# explicit initial branch name.
#
# Usage: pwsh fixtures/build.ps1 [-Out <dir>]

[CmdletBinding()]
param(
    [string] $Out
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$FixtureRoot = Split-Path -Parent $PSCommandPath
if (-not $Out) { $Out = Join-Path $FixtureRoot 'out' }

$GitBasic = Join-Path $Out 'git-basic'
$GitBare = Join-Path $Out 'git-bare'
$ContentDir = Join-Path $FixtureRoot 'content/git-basic'
$StepsFile = Join-Path $ContentDir 'steps.json'

# --- determinism ------------------------------------------------------------
#
# Isolate from whatever the developer has configured. On Windows this matters more
# than on POSIX: a machine-wide core.autocrlf=true would rewrite every committed
# file and change every SHA.
$env:GIT_CONFIG_GLOBAL = if ($IsWindows) { 'NUL' } else { '/dev/null' }
$env:GIT_CONFIG_SYSTEM = $env:GIT_CONFIG_GLOBAL
$env:GIT_CONFIG_NOSYSTEM = '1'
$env:LC_ALL = 'C'
$env:TZ = 'UTC'

$steps = Get-Content -Raw -LiteralPath $StepsFile | ConvertFrom-Json

$AuthorName = $steps.author.name
$AuthorEmail = $steps.author.email
$BotName = $steps.bot.name
$BotEmail = $steps.bot.email
$BaseEpoch = [int64]$steps.base_epoch
$SecondsPerStep = [int64]$steps.seconds_per_step
$DefaultBranch = $steps.default_branch

# UTF-8 with no BOM. A BOM would be three extra bytes in the committed file and a
# different SHA from the bash build — the exact class of silent divergence this
# script's parity test exists to catch.
$Utf8NoBom = New-Object System.Text.UTF8Encoding($false)

# Write text with LF line endings regardless of platform.
#
# This is the CRLF guard. PowerShell's own output cmdlets emit CRLF on Windows,
# so every generated file would differ from the bash build and every SHA would
# change. Copied files are safe because Copy-Item moves bytes; only files this
# script GENERATES need this.
# The path is resolved against PowerShell's *current location* before it reaches
# .NET. Push-Location moves the PowerShell provider location and leaves the
# process's working directory where it was, so a relative path handed straight to
# [System.IO.File] resolves against the directory the script was launched from —
# the repository root — and the write fails, or worse, succeeds in the wrong place.
# Every caller here passes a path relative to the fixture being built.
function Write-LfFile {
    param([string] $Path, [string] $Text)
    $full = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Path)
    $normalized = $Text -replace "`r`n", "`n"
    [System.IO.File]::WriteAllText($full, $normalized, $Utf8NoBom)
}

function Set-CommitTime {
    param([int64] $Index)
    $stamp = $BaseEpoch + ($Index * $SecondsPerStep)
    $env:GIT_AUTHOR_DATE = "$stamp +0000"
    $env:GIT_COMMITTER_DATE = "$stamp +0000"
}

function Invoke-Git {
    param([Parameter(ValueFromRemainingArguments = $true)] [string[]] $GitArgs)
    & git @GitArgs
    if ($LASTEXITCODE -ne 0) {
        throw "git $($GitArgs -join ' ') failed with exit code $LASTEXITCODE"
    }
}

function Invoke-CommitAs {
    param([int64] $Index, [string] $Name, [string] $Email, [string] $Subject)
    Set-CommitTime -Index $Index
    $env:GIT_AUTHOR_NAME = $Name
    $env:GIT_AUTHOR_EMAIL = $Email
    $env:GIT_COMMITTER_NAME = $Name
    $env:GIT_COMMITTER_EMAIL = $Email
    Invoke-Git commit --quiet --no-gpg-sign -m $Subject
}

# --- manifest ---------------------------------------------------------------

$ManifestEntries = [System.Collections.Generic.List[string]]::new()

function Add-ManifestEntry {
    param([string] $Role, [string] $Subject)
    $sha = (& git rev-parse HEAD).Trim()
    $ManifestEntries.Add("    {""role"": ""$Role"", ""sha"": ""$sha"", ""subject"": ""$Subject""}")
}

function Write-Manifest {
    param([string] $Path)
    $body = @(
        '{',
        '  "fixture": "git-basic",',
        '  "generator": "fixtures/build.sh",',
        '  "default_branch": "main",',
        '  "commits": [',
        ($ManifestEntries -join ",`n"),
        '  ]',
        '}'
    ) -join "`n"
    Write-LfFile -Path $Path -Text ($body + "`n")
}

# --- git-basic --------------------------------------------------------------

Write-Host "fixtures: building $GitBasic"
foreach ($dir in @($GitBasic, $GitBare)) {
    if (Test-Path -LiteralPath $dir) { Remove-Item -Recurse -Force -LiteralPath $dir }
}
New-Item -ItemType Directory -Force -Path $GitBasic | Out-Null
Push-Location $GitBasic

try {
    Invoke-Git init --quiet --initial-branch=$DefaultBranch .
    Invoke-Git config core.autocrlf false
    Invoke-Git config core.fileMode true
    Invoke-Git config commit.gpgsign false

    foreach ($step in $steps.steps) {
        switch ($step.kind) {
            'commit' {
                # -Force so dotfiles such as .github are included; Copy-Item moves
                # bytes, so nothing here can introduce CRLF.
                $source = Join-Path $ContentDir $step.dir
                Get-ChildItem -LiteralPath $source -Force | ForEach-Object {
                    Copy-Item -LiteralPath $_.FullName -Destination $GitBasic -Recurse -Force
                }
                Invoke-Git add -A
                $author = if ($step.PSObject.Properties.Name -contains 'author') { $step.author } else { 'human' }
                if ($author -eq 'bot') {
                    Invoke-CommitAs -Index $step.index -Name $BotName -Email $BotEmail -Subject $step.subject
                } else {
                    Invoke-CommitAs -Index $step.index -Name $AuthorName -Email $AuthorEmail -Subject $step.subject
                }
                Add-ManifestEntry -Role $step.role -Subject $step.subject
            }

            'generate' {
                New-Item -ItemType Directory -Force -Path $step.into | Out-Null
                # Must match build.sh byte for byte -- see the comment there, and
                # `fixture_parity`. ASCII only, no locale-dependent formatting.
                for ($n = 1; $n -le [int]$step.count; $n++) {
                    $padded = '{0:d3}' -f $n
                    $sb = [System.Text.StringBuilder]::new()
                    [void]$sb.Append("/// Generated fixture module $padded.`n")
                    [void]$sb.Append("///`n")
                    [void]$sb.Append("/// Deliberately verbose: 200 of these must exceed max_total_diff_bytes`n")
                    [void]$sb.Append("/// (512 KB) so that SPEC 9.4 truncation runs at its DEFAULT settings.`n")
                    [void]$sb.Append("pub const ID_$padded" + ": u32 = $n;`n")
                    [void]$sb.Append("`n")
                    for ($k = 1; $k -le 40; $k++) {
                        $kp = '{0:d2}' -f $k
                        [void]$sb.Append("pub fn value_${padded}_${kp}(input: u32) -> u32 { input.wrapping_add($k).wrapping_mul(3) }`n")
                    }
                    Write-LfFile -Path (Join-Path $step.into "mod_$padded.rs") -Text $sb.ToString()
                }
                Invoke-Git add -A
                Invoke-CommitAs -Index $step.index -Name $AuthorName -Email $AuthorEmail -Subject $step.subject
                Add-ManifestEntry -Role $step.role -Subject $step.subject
            }

            'branch' { Invoke-Git checkout --quiet -b $step.name }

            'checkout' { Invoke-Git checkout --quiet $step.name }

            'merge' {
                Set-CommitTime -Index $step.index
                $env:GIT_AUTHOR_NAME = $AuthorName
                $env:GIT_AUTHOR_EMAIL = $AuthorEmail
                $env:GIT_COMMITTER_NAME = $AuthorName
                $env:GIT_COMMITTER_EMAIL = $AuthorEmail
                Invoke-Git merge --quiet --no-ff --no-gpg-sign -m $step.subject $step.branch
                Add-ManifestEntry -Role $step.role -Subject $step.subject
            }

            default { throw "fixtures: unknown step kind $($step.kind)" }
        }
    }

    Write-Manifest -Path (Join-Path $GitBasic '.manifest.json')
    Write-LfFile -Path (Join-Path $GitBasic '.git/info/exclude') -Text ".manifest.json`n"

    $commitCount = [int](& git rev-list --count HEAD).Trim()
    if ($commitCount -ne 12) {
        throw "fixtures: expected 12 commits on $DefaultBranch, got $commitCount"
    }
} finally {
    Pop-Location
}

Write-Host "fixtures: building $GitBare"
Invoke-Git clone --quiet --mirror $GitBasic $GitBare
Write-Host "fixtures: git-basic has 12 commits; manifest at $GitBasic/.manifest.json"

# --- svn-basic --------------------------------------------------------------
#
# Same policy as build.sh: skip cleanly when svn is absent, and write a manifest
# that SAYS it skipped, so an svn suite that never ran does not look like one that
# passed (SPEC §18).

$svnBasic = Join-Path $Out 'svn-basic'
New-Item -ItemType Directory -Force -Path $svnBasic | Out-Null

if ((Get-Command svn -ErrorAction SilentlyContinue) -and (Get-Command svnadmin -ErrorAction SilentlyContinue)) {
    Write-Host "fixtures: building $svnBasic"
    & bash (Join-Path $FixtureRoot 'svn-driver.sh') $Out
    if ($LASTEXITCODE -ne 0) { throw "fixtures: the svn fixture failed to build" }
} else {
    Write-Host "fixtures: SKIPPING svn-basic - svn/svnadmin not on PATH."
    Write-Host "fixtures:   SVN tests will skip. Install Subversion and re-run to enable them."
    $skip = @(
        '{',
        '  "fixture": "svn-basic",',
        '  "generator": "fixtures/svn.sh",',
        '  "skipped": true,',
        '  "reason": "svn and svnadmin are not on PATH",',
        '  "revisions": [],',
        '  "remediation": "install Subversion (apt-get install subversion / brew install subversion / choco install svn) and re-run fixtures/build.sh"',
        '}'
    ) -join "`n"
    Write-LfFile -Path (Join-Path $svnBasic '.manifest.json') -Text ($skip + "`n")
}
