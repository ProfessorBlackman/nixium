<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# The landing page

Two pages, no build step, no JavaScript. `index.html` and `styles.css` are the product; `gallery.html`
and `gallery.css` are the mascot's room. Deployed to GitHub Pages by `.github/workflows/pages.yml`
when anything in this directory changes on `master`.

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

## The gallery

`gallery.html` is the one unserious thing here, and it is unserious on purpose. A page whose whole
argument is *this tool does not exaggerate to you* is easier to believe from a project that is
visibly not precious about itself, so the mascot gets twelve portraits, a catalogue note, and no
mention of features. It is reached from the masthead and from under the mascot on the front page,
because a joke nobody finds is not doing any work.

It loads `styles.css` first and `gallery.css` second: same palette, same masthead, same buttons, same
footer. The room is new, the building is not, and nobody should have to wonder whether they have left
the site.

**The gallery says *he*, this file and `styles.css` say *she*, and that stays as it is.** Nobody knows
what Nix is; the catalogue note on the gallery page admits as much in writing. It looks exactly like
an inconsistency somebody forgot to clean up, which is why it is written down here: a pull request
that unifies the pronoun is the one tidy-up this site will not take.

Sixteen plates, all of them drawn. `PORTRAITS.md` is the character sheet: what is fixed across every
plate, the house style the renders established, the prompt blocks to reuse, all sixteen scenes, and
how to replace one. It also records the one plate that breaks the costume and what to do if it is
ever re-cut.

Each plate links to its own image file, because the jokes in these are in the set dressing — book
spines, sticky notes, what is printed on the mug — and a 340px grid cell shows only some of it. A plain
link is the entire lightbox this page gets; there is still no JavaScript here.

**The artwork is deployed as WebP and kept as PNG, in two different places.** `assets/portraits/`
holds the renders as delivered, ~2 MB each, and is *not* published — `site/` is what the Pages
workflow uploads. `site/portraits/` holds 900px WebP cuts at about 100 kB each. The difference is
29 MB against 1.4 MB, which is the whole rest of the site twenty times over, so the full-size PNGs
must not be put back into `site/`.

## The palette

From the mascot rather than from a scheme: the aubergine ground is what makes her cream circle read as
a coin, the hibiscus is the flower in her hat, the straw is the hat.

Every text pair was checked against WCAG AA before it went in, the same way the app's own palette is
checked by `scripts/check-contrast.mjs`. That found one real failure: `.button-quiet`'s border at
1.38:1, where AA asks 3:1 for the edge of a control. Section dividers and table rules stay quiet at
1.38:1 on purpose — those are decoration, which the standard asks nothing of, and a divider loud
enough to pass would compete with what it separates.
