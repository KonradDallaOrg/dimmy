# Signing runner hardening

The Windows binaries are Authenticode-signed on a self-hosted runner on
Konrad's PC (label `dimmy-sign`), because the Certum cloud key has no headless
login. The PC has two Windows installs, each with its own runner:

| Disk | Runner dir | Registered as |
|---|---|---|
| C: | `C:\actions-runner` | `PC-KONRAD` |
| D: | `D:\actions-runner` | `PC-KDALLA` |

Both run interactively through `run.cmd`, as the logged-in user, next to a
logged-in SimplySign Desktop session.

## The risk

The repo is public. `ci.yml` and `e2e-tests.yml` run on `pull_request`, and a
pull request can change any workflow file, so it can add a job with
`runs-on: [self-hosted, dimmy-sign]` (or just `self-hosted`). Once a
maintainer approves the run ("Require approval for all external
contributors" is set), that job runs on the PC as Konrad, with the
SimplySign session open: it could sign anything with our certificate, or read
anything on the disk.

A workflow-level `if:` cannot stop this: the attacker owns the workflow file
of their pull request. The check has to live on the runner.

## The guard

`scripts/signing-runner/job-started-guard.ps1` is a runner pre-job hook.
The runner reads `ACTIONS_RUNNER_HOOK_JOB_STARTED` from its `.env` file and
runs that script before every job, under "Set up runner" in the job log
(GitHub docs: *Running scripts before or after a job*). Any exit code other
than 0 marks the job failed and no step runs. The hook gets the default
`GITHUB_*` variables and `GITHUB_EVENT_PATH`; it runs with `pwsh` and falls
back to Windows PowerShell, so the script is 5.1-compatible. There is no
timeout, which is why the script does no I/O beyond its log.

A job is allowed only when ALL of these hold:

- `GITHUB_REPOSITORY` is exactly `KonradDallaOrg/dimmy`
- `GITHUB_EVENT_NAME` is `push` (no signing workflow uses `workflow_dispatch`)
- `GITHUB_HEAD_REF` is empty (it is set for every pull-request event)
- `GITHUB_REF` starts with `refs/tags/v` (and `GITHUB_REF_TYPE`, when set, is `tag`)
- `GITHUB_WORKFLOW_REF` is
  `KonradDallaOrg/dimmy/.github/workflows/(release|staging-tester).yml@<GITHUB_REF>`

Every decision is appended, with a timestamp and the job's identity, to
`<runner dir>\_diag\dimmy-guard.log`. A refusal also prints an `::error`
annotation that names the reason.

`scripts/signing-runner/test-guard.ps1` runs the guard against 17 fake
environments (3 allowed, 14 refused: pull requests, `pull_request_target`,
`workflow_dispatch`, branch pushes, non-`v` tags, other workflows, look-alike
names, forks, case tricks, an empty environment):

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\signing-runner\test-guard.ps1
```

### What it does not protect against

- **Anyone who can push a `v*` tag.** A tag runs the workflow file of the tagged
  commit, so write access to the repo is the trust boundary. Keep it small,
  and keep tag protection / rulesets on `v*` if the plan allows them.
- **A compromised action inside an allowed job.** That is what the SHA pins
  in `.github/workflows/` (kept current by `.github/dependabot.yml`) are for.
- **Code that already runs on the PC.** The guard is a gate in front of the
  runner, not a sandbox.
- **A runner without the hook.** The guard works only on the runners where it
  is installed. A new runner registered with the `dimmy-sign` (or
  `self-hosted`) label is unprotected until you install it there too.

The `if: startsWith(github.ref, 'refs/tags/v')` on the `sign-windows` jobs
in `release.yml` and `staging-tester.yml` protects against our own mistakes
(say, a future `workflow_dispatch` on a branch). It is not a security boundary.

## Install — do this on BOTH runners

Do it once per Windows install. The steps for D: are the same with `D:\`.

1. Stop the runner: press Ctrl+C in the `run.cmd` window.
2. Copy the script from a checkout of this repo:

   ```powershell
   New-Item -ItemType Directory -Force C:\actions-runner\guard | Out-Null
   Copy-Item scripts\signing-runner\job-started-guard.ps1 C:\actions-runner\guard\
   Unblock-File C:\actions-runner\guard\job-started-guard.ps1
   ```

   `Unblock-File` matters if the file came from a browser download: the
   execution policy here is `RemoteSigned`, which refuses a downloaded
   unsigned script, and that would fail every signing job.
3. Add one line to `C:\actions-runner\.env` (create the file if it does not
   exist; keep the lines already there):

   ```
   ACTIONS_RUNNER_HOOK_JOB_STARTED=C:\actions-runner\guard\job-started-guard.ps1
   ```

4. Start the runner again: `C:\actions-runner\run.cmd`, and wait for
   "Listening for Jobs". The runner reads `.env` only at start.

For D:, use `D:\actions-runner\guard\job-started-guard.ps1` in both the copy
and the `.env` line. Each runner logs to its own `_diag\dimmy-guard.log`.

When the script changes in the repo, copy it again to both runners. The
runner runs the copy in `guard\`, not the one in the repo.

## Verify

1. Run the self-test above on the PC. It must end with `17 cases, 0 failed`.
2. Re-run the `sign-windows` job of the last signed tag
   (`gh run rerun <run-id> --job <job-id>`, or "Re-run job" in the UI).
   In the job log, "Set up runner" shows a step for the hook with
   `dimmy-guard: ... ALLOW ...`, and the job continues as before.
3. Check the log: `Get-Content C:\actions-runner\_diag\dimmy-guard.log -Tail 5`.
4. Optional negative check: a throw-away branch with a workflow that has
   `on: push` and `runs-on: [self-hosted, dimmy-sign]`. The job fails in
   "Set up runner" with "Job refused on the signing runner: ref is not a v*
   tag", and no step runs. Delete the branch afterwards.

If a real signing job is refused, the log line says which check failed. Fix
the cause; do not loosen the guard.

## Further hardening (optional)

- **Dedicated low-privilege Windows account.** Run the runner as a standard
  user that has no access to Konrad's files. The catch: signtool reaches the
  Certum key through the SimplySign Desktop session of the logged-in user,
  so test that SimplySign works for that account before you switch.
- **`--ephemeral` registration.** The runner takes one job and unregisters,
  so nothing a job leaves behind survives to the next. It needs a fresh
  registration token for each job, which does not fit an interactive
  `run.cmd`; it pays off only with scripted re-registration.
- **Org runner group restricted to selected workflows.** `PC-KONRAD` is a
  repo-level runner today (`runner_group_name: null`; `PC-KDALLA` was not in
  the repo's runner list on 2026-11-08, so check where it is registered), and
  repo-level runners cannot be limited to workflows. Moving them to an org runner group allows "Restrict
  to selected workflows" (`release.yml`, `staging-tester.yml` at `refs/tags/v*`);
  the org is on the Team plan, check that the option is offered there. Note that
  org runner groups refuse public repos until "Allow public repositories" is
  on. Reading the groups needs `gh auth refresh -h github.com -s admin:org`.
- **Keep "Require approval for all external contributors"** (Settings →
  Actions → General). It is set today (`approval_policy:
  all_external_contributors`). Before approving a run, check that the PR
  changes no file under `.github/`.
- **Enforce SHA pinning.** The repo's Actions permissions report
  `sha_pinning_required: false`. With every action pinned, it can be turned on
  so an unpinned `uses:` fails instead of running.
