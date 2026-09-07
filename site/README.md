<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# The landing page

One page, no build step, no JavaScript. `index.html`, `styles.css`, and the mascot. Deployed to GitHub
Pages by `.github/workflows/pages.yml` when anything in this directory changes on `master`.

To work on it: open `index.html` in a browser. There is nothing to install.

## The URL

**The address you get depends on the repository name, and this repository is `nixium`.**

GitHub serves a project site at `<user>.github.io/<repo>`, so as things stand this publishes to
`professorblackman.github.io/nixium`, not `professorblackman.github.io/nix`. Two ways to get the
shorter one:

- rename the repository to `nix`, which changes the clone URL and the links in this page; or
- put this directory in a separate repository named `nix` and deploy from there.

Every asset reference here is **relative**, so the page works at either path without editing — and the
Pages workflow asserts that, because an absolute `/mascot.png` would work locally and 404 once served
from a subdirectory.

## Why there are no screenshots

Because there were none to use, and a mockup drawn to look like a screenshot would be the first lie on
a page whose argument is that this tool does not lie to you about numbers.

What stands in for them is evidence: the hero is one real package measured three ways, and the numbers
table is the performance budgets that CI holds the build to. Every figure on the page is in
`docs/SPEC.md` and was checked against it before being published here.

When there are screenshots, they belong in the "What it does" section, and the four descriptions there
are already written to sit beside one each.

## The palette

From the mascot rather than from a scheme: the aubergine ground is what makes her cream circle read as
a coin, the hibiscus is the flower in her hat, the straw is the hat.

Every text pair was checked against WCAG AA before it went in, the same way the app's own palette is
checked by `scripts/check-contrast.mjs`. That found one real failure: `.button-quiet`'s border at
1.38:1, where AA asks 3:1 for the edge of a control. Section dividers and table rules stay quiet at
1.38:1 on purpose — those are decoration, which the standard asks nothing of, and a divider loud
enough to pass would compete with what it separates.
