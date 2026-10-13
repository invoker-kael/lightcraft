# Photo-Cake into LightCraft — integration track

## Product direction

This fork will become a unified local-first smart photography workstation, not a wrapper around two editing apps.
The photographer's path is **RAW import → Moment / semantic group → assisted culling → reference photo → adaptive per-photo recipes → exception-first batch review → native LightCraft export**.
Lightroom Classic comparison and conservative Adobe XMP handoff remain a transitional, optional deliverable. Native exports are the long-term default.

## Boundaries

- LightCraft's catalog, `DevelopSettings`, rendering process, GPU pipeline, undo journal and export path remain authoritative.
- Photo-Cake supplies *decisions* (Reference provenance, Moment membership, per-photo adaptive adjustments, protection/exceptions), not a second RAW renderer, catalog or general command registry.
- The first integration crate is `crates/photo-workflow`: a strictly validated, non-destructive mapper from Photo-Cake's core adjustments to a copy of native `DevelopSettings`.
- `None` does not reset an existing setting. Values outside native control ranges, NaNs/infinities and incomplete/uncalibrated white balance must fail closed.
- Never silently synchronize protected/manual-edited photos, HDR bracket sources or confirmed rejects. Selection/guard checks must be added before any batch application command is exposed.
- Existing XMP sidecars remain user-owned; do not silently overwrite them. LightCraft's `lc:settings` is not Lightroom's `crs:` representation.
- Preserve Photo-Cake's existing Windows/Android projects untouched during migration.

## Integration batches

1. **Recipe bridge (current)**: Rust crate with typed recipe/provenance, mapping, conservative WB/range checks and tests. No catalog mutation, UI changes or sidecar writes yet.
2. **Command and identity bridge**: map Photo-Cake stable asset IDs to LightCraft photo IDs; validate target/protection/review and commit verified per-photo changes as one undoable LightCraft catalog batch.
3. **Photographer workbench**: egui Moment / Cull / Reference / Batch Edit / Exception Review backed by existing LightCraft engine commands. Avoid a parallel editor state.
4. **Render verification and Lightroom comparison**: real-camera RAW corpus, color and process-version checks, one-way XMP adapter with existing-sidecar conflict detection, export parity evidence.
5. **Primary native delivery**: when verified, set LightCraft export as default, keep Lightroom optional, then retire old Photo-Cake Windows UI.

## Source and license

Original projects: `storytold/lightcraft` (MIT OR Apache-2.0) and `invoker-kael/Photo-Cake`.
Do not repurpose ArtCraft trademarks or branded assets when distributing a separately branded product; keep required license notices.
