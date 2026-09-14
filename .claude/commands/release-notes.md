---
description: Write the user-facing release notes and the CHANGELOG block for a release, from the commit range the release pipeline will actually ship. Run before tagging any rcN / staging.N / final.
allowed-tools: Bash, Read, Grep, Edit, Write
---

You write the two pieces of prose a release needs, from the same commit range
`release.yml` will ship. Both are for the person USING Dimmy, not the person
who wrote the code.

**Why this exists.** `release.yml` builds the GitHub release body from
`git log --pretty=format:"- %s"` — a list of commit subjects, first ten
visible, the rest folded away. That is a changelog for committers. A user
reading "fix(win): the About page read the wrong version source" learns
nothing about what was wrong with their copy of Dimmy. And `CHANGELOG.md`,
which `docs/RELEASING.md` step 4 requires, fell four months behind because
nothing ever forced the question at tag time.

## The range — get this right first

Use the SAME shape-matched rule `release.yml` uses, or your notes will cover
a different set of commits than the release does:

```bash
PREV=$(git tag --sort=-creatordate --list 'v*' \
       | grep -vE -- '-staging\.|^<the tag you are about to cut>$' | head -n 1)
git log --oneline --no-merges "$PREV..HEAD"
git diff --stat "$PREV..HEAD"
```

A staging tag is never the baseline. That filter exists because a
`staging.N` cut minutes earlier once produced a 64 KB release body.

Also run the mandatory version check before anything else — `gh release list
--limit 5`, `git tag --sort=-version:refname | head -5`, `core/Cargo.toml`.
Notes for a version number that is already published are worse than no notes.

## Deciding what is news

**Most commits are not news.** Refactors, test additions, CI repairs, lockfile
bumps, comment rewrites: they change nothing the user can observe, and listing
them buries the three lines that matter.

A commit is news if a user could NOTICE it: something they can now do,
something that used to be wrong, something that got faster or slower, or
something that changed where they had learned to look.

When a commit's user-visible effect is not clear from its message, read the
diff. When it is still not clear, ASK. Do not invent an effect, and do not
pad the notes to make the release look bigger.

## The voice

Read the tag annotations on `v0.7.2`, `v0.7.2-rc.4` and `v0.7.3-rc.1`
(`git tag -l --format='%(contents)' <tag>`) — they are the reference, and the
rules below are what makes them work.

1. **Lead with what the reader experienced, name the cause second.** Not
   "fixed a null manager in the version resolver" but "About said 0.7.3 while
   the banner under it said v0.7.3-rc.1 — the same window contradicting
   itself." A person recognises the symptom; nobody recognises the cause.

2. **Say who it is for when it is not everyone.** A feature that needs a work
   account, a paid plan, or specific hardware says so in its own sentence,
   before someone spends ten minutes discovering it.

3. **Measured numbers earn their place when they change a decision.** "11%
   less text than a 15-second window, and slower as well" tells someone why a
   default moved. A benchmark nobody acts on does not belong here.

4. **Name a removal or a limit as plainly as a feature.** If something no
   longer works, or only works for some accounts, that sentence is the most
   valuable one in the notes.

5. **No em-dashes and no tildes** (`feedback_no_em_dashes_in_ui_copy`) — they
   break PowerShell 5.1, and tag messages are pushed from PowerShell.

6. **English**, plain, no internal vocabulary. Never a module name, a file
   path, a config key or an FFI symbol. If a sentence only parses for someone
   who has read the source, rewrite it.

7. **Do not thank yourself.** No "improved", "enhanced", "various fixes".

## Output — produce BOTH, do not write files without asking

### 1. The release body

Prose for the GitHub release and the tag annotation. Shape:

- **One opening paragraph** naming the single thing this release is about. If
  there is no single thing, say what the two or three are.
- Then the news, grouped by what it affects, not by commit type.
- **On any `-rc.N` tag, end with the Stripe warning**: that pipeline is
  `release.yml`, which talks to the LIVE licensing endpoint, so testers should
  exercise trial and magic-link activation and not the purchase button.
- If a known limitation ships with it, say so here rather than letting someone
  find it.

### 2. The CHANGELOG.md block

Keep a Changelog shape, per `docs/RELEASING.md` step 4: a
`## [X.Y.Z] - YYYY-MM-DD` heading and `### Added` / `### Changed` / `### Fixed`
sections, moving anything relevant out of `[Unreleased]`.

This half has a DIFFERENT job from the release body: it is archaeology. Write
**the root cause**, not the symptom, because the reader is a future maintainer
looking at a bug that feels familiar. Where the release body says "a long
dictation used to fail outright", the changelog says which limit was hit and
what it was replaced with.

Where the two overlap, that is fine. They are read by different people for
different reasons.

## Finally

Show both to the human and stop. Do not edit `CHANGELOG.md`, do not create the
tag, do not push. Say plainly which commits you judged to be non-news, so the
call can be overruled before it is baked into a tag nobody will rewrite.

If `CHANGELOG.md` is more than one release behind, say so and how far — do not
quietly write one block on top of a gap.
