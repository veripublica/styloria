# Security policy

## Reporting a vulnerability

**Please do not open a public issue for a security problem.** Report it
privately through GitHub:
[**Report a vulnerability**](https://github.com/veripublica/styloria/security/advisories/new)
(the *Security* tab of this repository). Only the maintainer can see the
report.

A useful report includes the smallest CSS input that shows the problem, the
styloria version, the function you called with it, and what happened: the
panic message, or the memory or time used.

## What counts

styloria parses CSS that other people wrote. That makes these security
problems, on native targets and on `wasm32` alike:

- **a crash**: a panic, an abort or a stack overflow on any input. Nesting is
  bounded by `MAX_NESTING_DEPTH`, so no input should exhaust the stack;
- **resource exhaustion out of proportion to the input**: memory or time
  that a small stylesheet can drive far past what its size justifies;
- **any file read or written, and any network access**. styloria works on
  strings in memory and makes none by design, so any would be a defect;
- **the release pipeline**: anything that could put code into the published
  crate that is not in this repository.

A wrong parse or serialization (a token split in the wrong place, a missed
or false error) is not a security problem. Please report it as an ordinary
[issue](https://github.com/veripublica/styloria/issues), where it helps
everyone who meets it.

## Supported versions

styloria is before 1.0, so only the **latest release** receives fixes. A fix
ships as a new release, never as a patch to an old one.

## What happens next

styloria has one maintainer, so these are aims, not guarantees:

- an acknowledgement within a week;
- a fix released **before** the details are public. The release's
  `CHANGELOG.md` entry then describes the problem under **Security**;
- credit to the reporter in that entry, if they want it.

## Verifying what you download

The crate on crates.io is published by this repository's `publish-crate.yml`
workflow from the release tag. No token is stored anywhere: crates.io accepts
uploads only from that workflow, through trusted publishing (OIDC). Every
action the workflows use is pinned to a commit.

styloria has no dependencies, so the crate's code is exactly what is in this
repository at the release tag.
