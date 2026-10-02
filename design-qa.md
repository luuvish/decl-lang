# Homepage paper model — design QA

Date: 2026-10-02. Scope: the approved paper tree and reference graph on the
existing Decl homepage. This report covers the website changes in this turn,
not the earlier evaluator or documentation changes already in the working tree.

final result: passed

## Visual evidence

- Source visual truth: `/Users/luuvish/.codex/generated_images/01a09075-88e5-72c3-b919-27694018ca00/exec-394d4402-9c84-46f9-9ab9-5cd99f0dbb91.png`.
- Implementation: `http://127.0.0.1:4321/`, light theme, top of page.
- Desktop capture: `/private/tmp/decl-paper-desktop-final.jpg`.
- Full-view comparison, source left / implementation right:
  `/private/tmp/decl-paper-compare-final.jpg`.
- Focused artwork comparison, source above / implementation below:
  `/private/tmp/decl-paper-details-final.jpg`.
- Responsive captures: `/private/tmp/decl-paper-tablet-final.jpg` and
  `/private/tmp/decl-paper-mobile-final.jpg`.
- Dark theme after background correction: `/private/tmp/decl-dark-art-desktop.jpg`.
- Dark mobile after background correction: `/private/tmp/decl-dark-art-mobile.jpg`.
- Mobile enlargement: `/private/tmp/decl-paper-mobile-dialog.jpg`.

The source is 1504 × 1046 pixels. Desktop CSS viewport was 1504 × 1046,
devicePixelRatio 1. The browser capture provider returned 1489 × 1036 pixels;
the comparison normalizes that small capture-size difference to the source
canvas. Additional CSS widths were 834, 390, and 320. The focused comparison
checks the labels and reference destinations, not pixel-exact physical folds.
Captures are temporary local evidence; the source asset and generation prompt
are retained in the repository.

## Findings and comparison history

No actionable P0, P1, or P2 findings remain.

1. **P2 — Intro wrapping and vertical rhythm.** The first implementation
   wrapped the description to three lines and pushed the model farther down
   than the selected design. Evidence: `/private/tmp/decl-paper-compare-v1.jpg`.
   Adjusted the intro columns, type size, and hero spacing. Final comparison
   shows the two-line description and the next section starting at CSS y=1012
   in the 1046-pixel viewport, preserving the reading path from the mockup.
2. **P2 — Transparent edge artifacts.** The first extracted artwork had bright
   floating flecks outside the paper silhouette. Targeted alpha edits did not
   remove all of them. An opaque warm background removed the flecks, but its
   dark-theme treatment was subsequently rejected by the user; see finding 4.
3. **P2 — Title descender and enlargement contrast.** A tight image gap
   covered part of the title descender, and the enlargement label inherited
   light ink against the photograph in dark mode. Added a safe title/image
   gap and gave the label dark ink over the warm photograph; on mobile it
   follows page text because it sits below the photograph. Rechecked the
   final title, dark label, and mobile placement visually.
4. **P2 — White image rectangle in dark mode (user feedback).** The previous
   QA accepted a warm rectangular image surface, but this did not integrate
   with the dark page. Added a separate background-only ImageGen edit with
   a charcoal surrounding surface. The paper, labels, tree, and reference
   destinations are preserved. Hero links and dialog images now follow
   `data-theme` with CSS, and enlargement text follows the theme's ink color.
   Verified both themes, both dialogs, persisted dark theme after reload,
   and 390-pixel mobile layout. Evidence:
   `/private/tmp/decl-dark-art-desktop.jpg`,
   `/private/tmp/decl-dark-art-mobile.jpg`, and
   `/private/tmp/decl-dark-art-comparison.jpg`. The preceding blanket
   acceptance of the warm backdrop in dark mode is superseded by this fix.

## Required fidelity surfaces

- **Fonts and typography:** existing self-hosted Literata, IBM Plex Sans,
  and IBM Plex Mono are retained. Title and two-line description reproduce
  the selected hierarchy as real text. Mobile title wraps into two lines.
  No cropped glyphs or collisions remain. The raster mockup's synthesized
  letterforms are approximated with the project's actual fonts.
- **Spacing and layout:** wide title band above one illustration, followed
  by the exact tree/graph statement, an in-page link, a rule, and the runnable
  model. The photographic asset includes slightly more outer margin than
  the mockup; its entire paper silhouette and shadow remain visible.
  Tablet and mobile stack the introduction without page overflow.
- **Colors and tokens:** existing Graphite tokens control the live UI.
  The artwork retains warm paper, gray tree branches, and blue references.
  The physical paper remains ivory, preserving annotation contrast, while
  its surrounding backdrop switches between warm white and charcoal.
- **Image quality and fidelity:** real generated raster artwork, no code-drawn
  replacement. Six nodes and two reference threads are intact: `source`
  points to `gateway`, `target` to `auth`; ports, endpoints, and the constraint
  match the runnable model. Responsive WebP assets are 9–99 KB; the separate
  high-quality enlargement is 150 KB. Image dimensions reserve layout space.
- **Copy and content:** headline and caption match the selected design.
  Guide and Playground are working links; the existing functional theme
  control is retained. The subtle enlargement action is an accessibility
  addition for a text-bearing illustration, rather than an extra conversion CTA.

## Interaction and accessibility verification

- Production site build passed: 51 pages, search index, and image variants.
- All 26 local homepage links/anchors resolve in the generated site.
- Clicking “See the model below” updates `#model`; the heading lands at y=88,
  below the fixed header. Verified again after final spacing changes.
- Guide opens “Why Decl”; documentation retains its search field.
- Playground opens the Service Graph example. Run returns gateway:8080,
  auth:9001, inbound counts 0/1, and the expected two reference paths.
- Selecting the illustration opens a native modal. Close and Escape work;
  Escape restores focus to the illustration link. At mobile width the image
  scrolls within the dialog, with a visible close control.
- The full-image link remains a normal fallback without JavaScript or when
  opened with a modifier key. Title, caption, and navigation are live text;
  the illustration has a detailed alternative description.
- Light/dark controls work. Browser console inspection found no errors or warnings.
- Width checks at 320, 390, and 834 found no horizontal page overflow.
- `git diff --check` passed. No language behavior changed in this turn, so
  the previously completed language parity suite was not rerun for this UI change.
- Temporary browser viewport overrides were reset before handoff.

## Follow-up polish

- P3: the physical folds and framing vary slightly from the approved generated
  mockup because the website uses a separately generated illustration asset.
  A future art revision can tighten its outer margins; semantic labels and
  destinations must continue to match `site/src/samples/hero.decl`.
- No separate browser-engine or device-hardware matrix was run. Responsive
  and keyboard checks used the Codex in-app browser.

## Implementation checklist

- [x] Save artwork and its exact built-in ImageGen prompt in the project.
- [x] Implement the opening and homepage navigation in Astro components.
- [x] Add responsive delivery, alternative text, and enlargement.
- [x] Compare source and rendered output together and repair P2 findings.
- [x] Verify navigation, example execution, themes, and mobile layout.
- [x] Record the design and asset conventions in the site documentation.

Asset: `site/src/assets/model-paper.png`.
Generation prompt and provenance: `site/brand/model-paper.prompt.txt`.
Dark asset: `site/src/assets/model-paper-dark.png` (1821 × 864).
Dark edit prompt and provenance: `site/brand/model-paper-dark.prompt.txt`.
Both assets were created with built-in ImageGen. The dark variant's corner
sample is RGB (21, 21, 22), close to the page token RGB (20, 20, 22).
The follow-up site build passed for all 51 pages; the browser console had no
errors or warnings. No language or example semantics changed.
