// ---------------------------------------------------------------------------
// Feature flags for the Hub web frontend.
//
// v1.0 ships local-LLM-only, so cloud-LLM-cost surfaces (Cost route + nav
// item + Dashboard widget) are hidden. v2 (when cloud routing lands) will
// flip `v1_show_cost` to true and the surfaces reappear with zero code
// changes elsewhere.
//
// Canvas is hidden from the static nav per BRAND_SPEC_V1.0.md §5.1; the
// route is preserved so direct URL navigation still works, and the surface
// is intended to auto-reveal in v1.1 when an agent pushes a frame.
//
// Adding a new flag:
//   1. Add the entry below with a comment explaining when v2 flips it.
//   2. Import { featureFlags } from '@/config/features' at the call site.
//   3. Gate the render with `{featureFlags.<flag_name> && (...)}`.
//   4. Never delete the gated code path — flags are reversible, deletes are not.
// ---------------------------------------------------------------------------

export interface FeatureFlags {
  /**
   * Show the Cost Tracker page route in the sidebar and the Cost Overview
   * widget on the Dashboard.
   *
   * v1.0 — false (local-LLM only, no per-query metering, Cost is meaningless).
   * v2   — true  (cloud-LLM routing lands; per-query cost becomes visible).
   *
   * The Cost route remains registered in App.tsx so /cost still loads.
   */
  v1_show_cost: boolean;

  /**
   * Show the Canvas page in the static sidebar nav.
   *
   * v1.0 — false (Canvas surface is reserved for agent-pushed frames; the
   *               static nav entry confused early testers who saw a
   *               "Disconnected" badge on an empty page).
   * v1.1 — auto-reveal pattern (Canvas item appears in nav when a frame is
   *               pushed; clears when the canvas is cleared).
   * v2+  — true  (Canvas always-visible if the auto-reveal pattern proves
   *               unnecessary).
   *
   * The Canvas route remains registered in App.tsx so /canvas still loads.
   */
  v1_show_canvas_in_nav: boolean;
}

export const featureFlags: FeatureFlags = {
  v1_show_cost: false,
  v1_show_canvas_in_nav: false,
};
