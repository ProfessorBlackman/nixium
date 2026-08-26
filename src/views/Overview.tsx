/**
 * Overview. A placeholder until MON-2 lands in Phase 3.
 *
 * Deliberately says what it is rather than faking data: the spec's honest-numbers principle applies
 * to the UI too, and an empty state that explains itself is better than an invented chart.
 */
export default function Overview() {
  return (
    <section className="stack">
      <div className="card">
        <h2>Not built yet</h2>
        <p>
          The overview lands in Phase 3 (MON-2), and will lead with reclaimable space rather than
          burying it — nix is a storage-first tool, so the headline figure belongs here.
        </p>
        <p className="muted">
          Phase 0 is foundation only: no user-facing features. What exists today is the error
          surface, the privileged helper, the settings store, capability probing, and the
          long-operation primitive — all visible from the About view.
        </p>
      </div>
    </section>
  );
}
