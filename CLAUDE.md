# styloria — Project Handoff / Bootstrap

> **For a fresh Claude Code session:** Read this file first. `styloria` was
> scoped during a session on the `epubveri` project (2026-07-01), after
> epubveri's own XHTML content-model work made clear that CSS validation was
> the next architectural gap — and that gap deserved its own standalone
> project rather than living inside epubveri. Treat the decisions below as
> **already made** (don't relitigate unless asked).

---

## What styloria is

A **pure-Rust, general-purpose CSS3 parser and serializer** — not tied to
epubveri or EPUB at all. Starts from the
[CSS Syntax Level 3](https://www.w3.org/TR/css-syntax-3/) spec: the
tokenizer + "core grammar" (stylesheets → rules → preludes/blocks) layer,
which is fully spec'd and property-agnostic (you don't need to know what
`color` or `font-size` mean to parse a stylesheet's structure correctly).
Deeper, property-specific semantic validation is a later layer built on top.

**Why standalone, not a module inside epubveri:** the owner's explicit
reasoning — some users may want to download and use *only* the CSS parser,
independent of EPUB validation entirely. This mirrors why `epubveri` itself
is a separate repo from `epublift` (see epubveri's own CLAUDE.md for that
precedent): a foundational capability shouldn't be trapped inside a single
consumer's repo.

**Relationship to epubveri:** epubveri will eventually depend on `styloria`
(as a normal crate dependency) for validating CSS in EPUB content documents
(currently the `CSS` family in epubveri's corpus measurement sits at 0%
coverage — this is the intended fix). During epubveri development, if
epubveri needs styloria before it's published to crates.io, use a temporary
`path`/`git` Cargo dependency, then switch once published — same pattern
epubveri itself used for epublift.

## Locked decisions (2026-07-01)

- **Name: `styloria`.** Coined (not descriptive) for trademark strength,
  same logic as `epubveri`'s naming: the "styl-" root reads naturally as
  "style" (CSS = Cascading **Style** Sheets), while the whole word is
  invented/ownable. Checked free on crates.io at decision time. (A sibling
  candidate, `cascada`, was rejected: already taken on crates.io.)
- **Org/repo: `github.com/veripublica/styloria`** — same house brand as
  `epubveri`, not a new org. Public visibility (explicitly confirmed by the
  owner) — the whole point is independent, standalone use.
- **License: dual `AGPL-3.0-only OR LicenseRef-veripublica-Commercial`** —
  same model and same reasoning as epubveri (protection + monetization; see
  epubveri's CLAUDE.md "Owner context" for the fuller backstory on *why* the
  owner insists on this model, not just AGPL). **CLA required before any
  external contribution** — same hard prerequisite as epubveri, for the same
  reason (commercial licensing requires the owner to hold all copyright).
- **Scope: general-purpose, not EPUB-specific.** This is the key difference
  from how epubveri's own schemas (`package.rng`, `xhtml.rng`) were scoped —
  those are deliberately EPUB-shaped. `styloria` should parse/serialize CSS
  the way any Rust project would want, with EPUB-specific *validation rules*
  (if any are ever needed) layered on as an *optional* feature or left to
  the consumer (epubveri) entirely — don't let EPUB-specific concerns leak
  into the core parser design.
- **Repo setup checklist — DONE:** `LICENSE` (official AGPL-3.0 text),
  `LICENSE-COMMERCIAL.md`, `README.md`, `CONTRIBUTING.md`, `Cargo.toml`
  (`license` field set, `publish = false` for now), `.gitignore` (note:
  `Cargo.lock` is gitignored here since this is a **library** — convention
  is apps commit their lock file, libraries don't; epubveri, a CLI app,
  does commit its own).
- **Trademark clearance: not done, matching epubveri's precedent.** The
  owner already decided (for epubveri) that a formal USPTO/EUIPO search
  isn't worth the cost/time right now; the same tradeoff applies here.
  Don't re-raise unless the owner brings it up.

## Owner context

Same as epubveri (same owner, same house brand) — see epubveri's CLAUDE.md
"Owner context" section for the full backstory (Turkish speaker, mirror his
language; never credit Claude/Anthropic anywhere; data-first/calibrated
experiments; accumulate locally, push only when explicitly asked; verifies
on real input rather than trusting synthetic tests alone).

## Status

**2026-07-01: repo scaffolding only.** No tokenizer/parser code yet — this
file was written immediately after repo creation, before the first real
implementation session. The natural first step is a CSS Syntax Level 3
tokenizer (the well-specified, stable foundation everything else builds on),
architected the same way epubveri's RNG engine was: a spike/measurement
mindset (small first slice, test against real CSS, iterate), and — learning
directly from epubveri's own hard-won lesson — **watch for the same class of
performance trap** epubveri hit with its derivative engine (naive recursive/
repeated-allocation patterns that are fine on toy input and blow up on real
input). Don't assume "it'll be fine at scale" — measure early.
