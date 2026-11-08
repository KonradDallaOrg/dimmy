# Pre-job hook for the self-hosted signing runner (label `dimmy-sign`).
#
# The runner calls this before every job through ACTIONS_RUNNER_HOOK_JOB_STARTED
# in its .env file. A non-zero exit fails the job before any step runs. The
# repo is public, so a pull request can add a job that targets our runner
# label; a workflow-level `if:` cannot stop that, because the pull request
# owns the workflow file. This script lives on the runner, out of their reach.
#
# Allowed: a tag push of v* that runs release.yml or staging-tester.yml from
# KonradDallaOrg/dimmy. Everything else is refused.
#
# Windows PowerShell 5.1 compatible. See docs/dev/signing-runner-hardening.md.
param(
    # Tests only. The runner calls the script without arguments.
    [string]$LogPath
)

$ErrorActionPreference = 'Stop'

$ExpectedRepo = 'KonradDallaOrg/dimmy'
$AllowedEvents = @('push')
$WorkflowRefPattern = '^KonradDallaOrg/dimmy/\.github/workflows/(release|staging-tester)\.yml@(refs/tags/v[0-9A-Za-z._+-]+)$'

if (-not $LogPath) {
    $runnerRoot = Split-Path -Parent $PSScriptRoot
    $LogPath = Join-Path (Join-Path $runnerRoot '_diag') 'dimmy-guard.log'
}

function Write-GuardLog([string]$decision, [string]$reason) {
    $line = '{0} {1} reason="{2}" repo="{3}" event="{4}" ref="{5}" workflow_ref="{6}" head_ref="{7}" actor="{8}" run_id="{9}" job="{10}"' -f `
        (Get-Date).ToString('o'), $decision, $reason, $env:GITHUB_REPOSITORY, $env:GITHUB_EVENT_NAME,
        $env:GITHUB_REF, $env:GITHUB_WORKFLOW_REF, $env:GITHUB_HEAD_REF, $env:GITHUB_ACTOR,
        $env:GITHUB_RUN_ID, $env:GITHUB_JOB
    try {
        $dir = Split-Path -Parent $LogPath
        if ($dir -and -not (Test-Path $dir)) { New-Item -ItemType Directory -Force -Path $dir | Out-Null }
        Add-Content -Path $LogPath -Value $line -Encoding utf8
    } catch {
        # A log that cannot be written must not let a job through, and must
        # not block a good one: the decision below stands either way.
        Write-Host "dimmy-guard: cannot write log ${LogPath}: $($_.Exception.Message)"
    }
    Write-Host "dimmy-guard: $line"
}

function Deny([string]$reason) {
    Write-GuardLog 'DENY' $reason
    Write-Host "::error title=Signing runner guard::Job refused on the signing runner: $reason. Only tag pushes (v*) of release.yml / staging-tester.yml in $ExpectedRepo may run here."
    exit 1
}

if ($env:GITHUB_REPOSITORY -cne $ExpectedRepo) { Deny 'wrong repository' }
if ($AllowedEvents -cnotcontains $env:GITHUB_EVENT_NAME) { Deny 'event is not push' }
if ($env:GITHUB_HEAD_REF) { Deny 'GITHUB_HEAD_REF is set (pull request)' }
if (-not $env:GITHUB_REF -or -not $env:GITHUB_REF.StartsWith('refs/tags/v', [StringComparison]::Ordinal)) {
    Deny 'ref is not a v* tag'
}
if ($env:GITHUB_REF_TYPE -and $env:GITHUB_REF_TYPE -cne 'tag') { Deny 'ref type is not tag' }

$m = [regex]::Match([string]$env:GITHUB_WORKFLOW_REF, $WorkflowRefPattern)
if (-not $m.Success) { Deny 'workflow is not release.yml or staging-tester.yml at a v* tag' }
if ($m.Groups[2].Value -cne $env:GITHUB_REF) { Deny 'workflow ref and job ref differ' }

Write-GuardLog 'ALLOW' 'tag push of a signing workflow'
exit 0
