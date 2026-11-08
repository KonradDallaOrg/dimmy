# Self-test for job-started-guard.ps1. No Pester: runs the guard in a child
# Windows PowerShell with a fake job environment per case and checks the exit
# code and the log line.
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts\signing-runner\test-guard.ps1
$ErrorActionPreference = 'Stop'

$guard = Join-Path $PSScriptRoot 'job-started-guard.ps1'
$log = Join-Path ([IO.Path]::GetTempPath()) ("dimmy-guard-test-{0}.log" -f [guid]::NewGuid())
$vars = 'GITHUB_REPOSITORY', 'GITHUB_EVENT_NAME', 'GITHUB_REF', 'GITHUB_REF_TYPE',
        'GITHUB_WORKFLOW_REF', 'GITHUB_HEAD_REF', 'GITHUB_ACTOR', 'GITHUB_RUN_ID', 'GITHUB_JOB'

$release = @{
    GITHUB_REPOSITORY   = 'KonradDallaOrg/dimmy'
    GITHUB_EVENT_NAME   = 'push'
    GITHUB_REF          = 'refs/tags/v0.7.5'
    GITHUB_REF_TYPE     = 'tag'
    GITHUB_WORKFLOW_REF = 'KonradDallaOrg/dimmy/.github/workflows/release.yml@refs/tags/v0.7.5'
    GITHUB_ACTOR        = 'KonradDalla'
    GITHUB_RUN_ID       = '1'
    GITHUB_JOB          = 'sign-windows'
}
function With([hashtable]$base, [hashtable]$change) {
    $h = $base.Clone()
    foreach ($k in $change.Keys) { $h[$k] = $change[$k] }
    $h
}

$cases = @(
    @{ Name = 'release.yml rc tag';          Expect = 0; Env = (With $release @{ GITHUB_REF = 'refs/tags/v0.7.5-rc.2'; GITHUB_WORKFLOW_REF = 'KonradDallaOrg/dimmy/.github/workflows/release.yml@refs/tags/v0.7.5-rc.2' }) }
    @{ Name = 'release.yml stable tag';      Expect = 0; Env = $release }
    @{ Name = 'staging-tester.yml tag';      Expect = 0; Env = (With $release @{ GITHUB_REF = 'refs/tags/v0.7.5-staging.4'; GITHUB_WORKFLOW_REF = 'KonradDallaOrg/dimmy/.github/workflows/staging-tester.yml@refs/tags/v0.7.5-staging.4' }) }
    @{ Name = 'pull_request from a fork';    Expect = 1; Env = (With $release @{ GITHUB_EVENT_NAME = 'pull_request'; GITHUB_REF = 'refs/pull/9/merge'; GITHUB_REF_TYPE = 'branch'; GITHUB_HEAD_REF = 'evil'; GITHUB_WORKFLOW_REF = 'KonradDallaOrg/dimmy/.github/workflows/ci.yml@refs/pull/9/merge' }) }
    @{ Name = 'pull_request_target';         Expect = 1; Env = (With $release @{ GITHUB_EVENT_NAME = 'pull_request_target'; GITHUB_HEAD_REF = 'evil' }) }
    @{ Name = 'push event, head ref set';    Expect = 1; Env = (With $release @{ GITHUB_HEAD_REF = 'evil' }) }
    @{ Name = 'workflow_dispatch';           Expect = 1; Env = (With $release @{ GITHUB_EVENT_NAME = 'workflow_dispatch' }) }
    @{ Name = 'branch push';                 Expect = 1; Env = (With $release @{ GITHUB_REF = 'refs/heads/staging'; GITHUB_REF_TYPE = 'branch'; GITHUB_WORKFLOW_REF = 'KonradDallaOrg/dimmy/.github/workflows/release.yml@refs/heads/staging' }) }
    @{ Name = 'non-v tag';                   Expect = 1; Env = (With $release @{ GITHUB_REF = 'refs/tags/engine-nemotron-1'; GITHUB_WORKFLOW_REF = 'KonradDallaOrg/dimmy/.github/workflows/release.yml@refs/tags/engine-nemotron-1' }) }
    @{ Name = 'other workflow at v tag';     Expect = 1; Env = (With $release @{ GITHUB_WORKFLOW_REF = 'KonradDallaOrg/dimmy/.github/workflows/ci.yml@refs/tags/v0.7.5' }) }
    @{ Name = 'look-alike workflow name';    Expect = 1; Env = (With $release @{ GITHUB_WORKFLOW_REF = 'KonradDallaOrg/dimmy/.github/workflows/release.yml.evil.yml@refs/tags/v0.7.5' }) }
    @{ Name = 'workflow from a fork repo';   Expect = 1; Env = (With $release @{ GITHUB_WORKFLOW_REF = 'attacker/dimmy/.github/workflows/release.yml@refs/tags/v0.7.5' }) }
    @{ Name = 'workflow ref != job ref';     Expect = 1; Env = (With $release @{ GITHUB_WORKFLOW_REF = 'KonradDallaOrg/dimmy/.github/workflows/release.yml@refs/tags/v0.0.1' }) }
    @{ Name = 'wrong repository';            Expect = 1; Env = (With $release @{ GITHUB_REPOSITORY = 'attacker/dimmy' }) }
    @{ Name = 'repository wrong case';       Expect = 1; Env = (With $release @{ GITHUB_REPOSITORY = 'konraddallaorg/dimmy' }) }
    @{ Name = 'ref type branch';             Expect = 1; Env = (With $release @{ GITHUB_REF_TYPE = 'branch' }) }
    @{ Name = 'empty environment';           Expect = 1; Env = @{} }
)

$saved = @{}
foreach ($v in $vars) { $saved[$v] = [Environment]::GetEnvironmentVariable($v, 'Process') }
$failed = 0
try {
    foreach ($c in $cases) {
        foreach ($v in $vars) { [Environment]::SetEnvironmentVariable($v, $null, 'Process') }
        foreach ($k in $c.Env.Keys) { [Environment]::SetEnvironmentVariable($k, $c.Env[$k], 'Process') }
        $before = if (Test-Path $log) { @(Get-Content $log).Count } else { 0 }
        $null = & powershell.exe -NoProfile -ExecutionPolicy Bypass -File $guard -LogPath $log
        $code = $LASTEXITCODE
        $lines = @(Get-Content $log)
        $logged = ($lines.Count -eq $before + 1)
        $want = if ($c.Expect -eq 0) { ' ALLOW ' } else { ' DENY ' }
        $ok = ($code -eq $c.Expect) -and $logged -and ($lines[-1].Contains($want))
        if ($ok) { Write-Host ("PASS  {0,-30} exit {1}" -f $c.Name, $code) }
        else { $failed++; Write-Host ("FAIL  {0,-30} exit {1}, expected {2}, logged={3}" -f $c.Name, $code, $c.Expect, $logged) }
    }
} finally {
    foreach ($v in $vars) { [Environment]::SetEnvironmentVariable($v, $saved[$v], 'Process') }
    Remove-Item $log -ErrorAction SilentlyContinue
}

Write-Host ("{0} cases, {1} failed" -f $cases.Count, $failed)
if ($failed) { exit 1 }
exit 0
