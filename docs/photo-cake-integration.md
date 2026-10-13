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


## Stage 1B: validated native batch commands

Following the typed recipe mapper in lightcraft-photo-workflow, the same fork now supplies command IDs smart.recipe.preflight and smart.recipe.apply through the **existing** engine registry (UI, CLI, control channel, MCP). These commands only accept explicit, already-resolved adaptive Recipes. They do not yet compute Moment/embedding similarity, Reference candidates or AI recommendations.

Input (as the params body of engine.execute):

    {
      "reference": 1,
      "processVersion": 1,
      "targets": [
        {
          "id": 2,
          "recipe": {
            "recipe_id": "photo-cake-recipe-2",
            "source_reference_ids": ["original-reference-uuid"],
            "adjustments": {"exposure": 0.4, "highlights": -20.0}
          }
        }
      ]
    }

- reference and target IDs are **numeric LightCraft catalog PhotoIds**. Recipe provenance IDs remain the original Photo-Cake strings/UUIDs. A persistent ID reconciliation layer is a future migration requirement; do not assume the two are the same.
- preflight is read-only and reports referenceHash plus each target's READY, UNCHANGED or BLOCKED status, reason, expectedHash and afterHash. It does not mutate original RAW, XMP or the catalog.
- apply requires the same body **plus** referenceHash and an expectedHash on every matching target from the previous preflight. Every input is revalidated against the current native catalog; any stale or blocked target aborts all changes before commit. One successful apply writes **one native catalog undo transaction**; edit.undo reverses that whole batch.
- LightCraft's own typed resolve_recipe is the only parameter mapping function. Eight global edits can be mapped today, and paired white balance is permitted only with explicit verifiedKelvin and a matching native Kelvin scale; unknown fields, invalid values and unsupported process versions fail closed.
- Individual edits are protected by default. overwriteEdited must be explicit and still cannot bypass caller protected=true, Reject, deleted/Local or unsupported/preview-only RAW. Photo-Cake must mark its HDR sources and previous approvals protected=true until those decisions are natively represented.
- During Lightroom comparison, smart.recipe.apply deliberately bypasses automatic XMP sidecar writes. User-owned Adobe crs: data is not modified by this command. Native DevelopSettings and catalog history remain the edit authority; intentional export or separate Lightroom sidecar handoff must be invoked later.

This stage completes the initial **execution bridge**, not the smart photography planner or native UI. Future work must reuse it rather than duplicate its validation or reintroduce a parallel SQLite edit state.

## Verification

The command module includes coverage for read-only preflight, native one-step undo, stale-state rejection, protecting edited and rejected photos, explicit override preserving unrelated edits, uncalibrated white balance and unknown Recipe fields. Production validation also requires real Windows RAW fixtures, Lightroom image comparisons and full fork CI. Never assert Lightroom/LightCraft rendering parity merely because numeric controls agree.
