//! Guarded Photo-Cake Recipe commands over the existing LightCraft engine.
//! Uses lightcraft-photo-workflow as the *only* Recipe -> DevelopSettings mapper.
//! Stage 1: explicit, previously resolved per-photo Recipes. AI planning comes later.

use std::collections::HashSet;
use lightcraft_catalog::{Flag, MediaKind, Op, PhotoId};
use lightcraft_develop::DevelopSettings;
use lightcraft_photo_workflow::{AdaptiveRecipe, resolve_recipe};
use serde::Deserialize;
use serde_json::{Value, json};
use super::{CommandSpec, always, bad, cmd};
use crate::{Result, Session};

const PLAN: &str = "smart.recipe.preflight";
const APPLY: &str = "smart.recipe.apply";

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Batch {
    reference: u64,
    process_version: u16,
    #[serde(default)]
    overwrite_edited: bool,
    #[serde(default)]
    reference_hash: Option<String>,
    targets: Vec<Target>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Target {
    id: u64,
    #[serde(default)]
    expected_hash: Option<String>,
    /// Photo-Cake's HDR/approved/manual restrictions. Always honored.
    #[serde(default)]
    protected: bool,
    #[serde(default)]
    verified_kelvin: bool,
    recipe: AdaptiveRecipe,
}

struct Planned {
    id: PhotoId,
    recipe_id: String,
    before: String,
    after: DevelopSettings,
    reason: Option<&'static str>,
    changed: bool,
}

fn hash(settings: &DevelopSettings) -> String {
    format!("{:016x}", settings.hash64())
}

fn parse(p: &Value, command: &str) -> Result<Batch> {
    let batch: Batch = serde_json::from_value(p.clone())
        .map_err(|e| bad(command, e.to_string()))?;
    if batch.process_version != 1 {
        return Err(bad(command, "only Photo-Cake Recipe ProcessVersion 1 is supported"));
    }
    if batch.targets.is_empty() || batch.targets.len() > 1000 {
        return Err(bad(command, "expected 1..1000 target photos"));
    }
    let mut seen = HashSet::new();
    for t in &batch.targets {
        if t.id == batch.reference || !seen.insert(t.id) {
            return Err(bad(command, "targets must be unique and different from reference"));
        }
    }
    Ok(batch)
}

fn plan(s: &Session, batch: &Batch, command: &str) -> Result<Vec<Planned>> {
    let source = s.catalog.photo(PhotoId(batch.reference))
        .ok_or_else(|| bad(command, "reference photo not found"))?;
    if source.deleted || source.local || source.flag == Flag::Reject {
        return Err(bad(command, "reference photo is not deliverable"));
    }
    if command == APPLY && batch.reference_hash.as_deref() != Some(hash(&source.develop).as_str()) {
        return Err(bad(command, "reference changed since preflight"));
    }
    let mut output = Vec::with_capacity(batch.targets.len());
    for t in &batch.targets {
        let photo = s.catalog.photo(PhotoId(t.id))
            .ok_or_else(|| bad(command, format!("photo {} not found in library", t.id)))?;
        let before = hash(&photo.develop);
        let after = resolve_recipe(&photo.develop, &t.recipe, t.verified_kelvin)
            .map_err(|e| bad(command, format!("photo {}: {e}", t.id)))?;
        let reason = if t.protected {
            Some("CALLER_PROTECTED")
        } else if photo.deleted || photo.local || photo.flag == Flag::Reject {
            Some("NOT_DELIVERABLE")
        } else if photo.kind != MediaKind::Raw || photo.preview_only.is_some() {
            Some("NOT_DECODED_RAW")
        } else if !batch.overwrite_edited && photo.is_edited() {
            Some("INDIVIDUAL_EDITS")
        } else {
            None
        };
        if command == APPLY && t.expected_hash.as_deref() != Some(before.as_str()) {
            return Err(bad(command, format!(
                "photo {} changed since preflight or is missing expectedHash", t.id
            )));
        }
        output.push(Planned {
            id: PhotoId(t.id),
            recipe_id: t.recipe.recipe_id.clone(),
            before,
            changed: after != *photo.develop,
            after,
            reason,
        });
    }
    Ok(output)
}

fn preflight(s: &mut Session, p: &Value) -> Result<Value> {
    let batch = parse(p, PLAN)?;
    let rows = plan(s, &batch, PLAN)?;
    let reference_hash = s.catalog.photo(PhotoId(batch.reference))
        .map(|p| hash(&p.develop)).unwrap_or_default();
    Ok(json!({
        "reference": batch.reference,
        "referenceHash": reference_hash,
        "processVersion": 1,
        "ready": rows.iter().filter(|r| r.reason.is_none() && r.changed).count(),
        "blocked": rows.iter().filter(|r| r.reason.is_some()).count(),
        "targets": rows.iter().map(|r| json!({
            "id": r.id.0,
            "recipeId": r.recipe_id,
            "status": if r.reason.is_some() { "BLOCKED" } else if r.changed { "READY" } else { "UNCHANGED" },
            "reason": r.reason,
            "expectedHash": r.before,
            "afterHash": hash(&r.after),
        })).collect::<Vec<_>>(),
    }))
}

fn apply(s: &mut Session, p: &Value) -> Result<Value> {
    let batch = parse(p, APPLY)?;
    let planned = plan(s, &batch, APPLY)?;
    if let Some(r) = planned.iter().find(|r| r.reason.is_some()) {
        return Err(bad(APPLY, format!(
            "photo {} protected ({}); nothing changed",
            r.id.0, r.reason.unwrap_or("unknown")
        )));
    }
    let mut ops = Vec::new();
    let mut ids = Vec::new();
    for r in planned.iter().filter(|r| r.changed) {
        let op = s.develop_op(r.id, r.after.clone(), "Photo-Cake Adaptive Recipe")
            .ok_or_else(|| bad(APPLY, format!("photo {} disappeared", r.id.0)))?;
        ops.push(op);
        ids.push(r.id.0);
    }
    if !ops.is_empty() {
        // LightCraft's lc:settings is not Adobe Camera Raw's crs: settings.
        // During the comparison period keep all sidecars untouched by the bridge.
        s.skip_auto_write = true;
        s.commit("Photo-Cake Adaptive Batch", Op::Batch { ops })?;
    }
    Ok(json!({
        "reference": batch.reference, "changed": ids.len(),
        "unchanged": planned.len() - ids.len(), "ids": ids,
        "undoCommand": "edit.undo", "engine": "lightcraft",
    }))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(query "smart.recipe.preflight", "Preflight Photo-Cake Recipes", [], None,
            "{reference,processVersion:1,targets:[{id,recipe:{recipe_id,source_reference_ids,adjustments},protected?,verifiedKelvin?}],overwriteEdited?} -> hashes and dispositions; no writes",
            always, preflight),
        cmd!("smart.recipe.apply", "Apply Photo-Cake Adaptive Recipes", [], None,
            "{reference,referenceHash,processVersion:1,targets:[{id,expectedHash,recipe:{...},protected?}],overwriteEdited?}; one LightCraft undo step",
            always, apply),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_catalog::{Photo, Source};
    use serde_json::json;

    fn session() -> Session {
        let mut s = Session::new();
        for id in 1..=3 {
            let mut p = Photo::new(PhotoId(id), Source::Demo { scene: id as u32 },
                &format!("{id}.dng"), "DNG", 320, 240, "2026-10-13");
            p.kind = MediaKind::Raw;
            p.develop = std::sync::Arc::new(p.camera_defaults());
            s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).expect("test fixture");
        }
        s
    }

    fn recipe(id: &str, exposure: f64) -> Value {
        json!({"recipe_id": id, "source_reference_ids": ["source-ref-1"],
            "adjustments": {"exposure": exposure}})
    }

    fn batch() -> Value {
        json!({"reference":1,"processVersion":1,"targets":[
            {"id":2,"recipe":recipe("r2",0.4)},
            {"id":3,"recipe":recipe("r3",-0.2)}
        ]})
    }

    fn approved(s: &mut Session, mut p: Value) -> Value {
        let response = s.execute(PLAN, &p).expect("preflight");
        p["referenceHash"] = response["referenceHash"].clone();
        for t in p["targets"].as_array_mut().expect("targets") {
            let id = t["id"].clone();
            let planned = response["targets"].as_array().expect("rows").iter()
                .find(|x| x["id"] == id).expect("row");
            t["expectedHash"] = planned["expectedHash"].clone();
        }
        p
    }

    #[test]
    fn read_only_plan_atomic_apply_and_native_undo() {
        let mut s = session();
        let old = (*s.develop_of(PhotoId(2)).expect("photo")).clone();
        let count = s.undo.len();
        let p = approved(&mut s, batch());
        assert_eq!(s.undo.len(), count);
        assert_eq!(s.execute(APPLY, &p).expect("apply")["changed"], 2);
        assert_eq!(s.undo.len(), count + 1);
        assert_eq!(s.develop_of(PhotoId(2)).expect("photo").light.exposure, 0.4);
        s.execute("edit.undo", &json!({})).expect("undo");
        assert_eq!(*s.develop_of(PhotoId(2)).expect("photo"), old);
    }

    #[test]
    fn stale_batch_is_rejected_without_partial_edits() {
        let mut s = session();
        let p = approved(&mut s, batch());
        let mut d = (*s.develop_of(PhotoId(3)).expect("photo")).clone();
        d.light.exposure = 0.8;
        s.set_develop(PhotoId(3), d, "Human edit").expect("human edit");
        assert!(s.execute(APPLY, &p).is_err());
        assert_eq!(s.develop_of(PhotoId(2)).expect("photo").light.exposure, 0.0);
    }

    #[test]
    fn editing_existing_photos_requires_explicit_overwrite() {
        let mut s = session();
        let mut d = (*s.develop_of(PhotoId(2)).expect("photo")).clone();
        d.light.contrast = 18.0;
        s.set_develop(PhotoId(2), d, "Human edit").expect("human edit");
        let p = json!({"reference":1,"processVersion":1,"targets":[
            {"id":2,"recipe":recipe("r2",0.8)}
        ]});
        let check = s.execute(PLAN, &p).expect("preflight");
        assert_eq!(check["targets"][0]["reason"], "INDIVIDUAL_EDITS");
        let p = approved(&mut s, p);
        assert!(s.execute(APPLY, &p).is_err());

        let mut override_request = p;
        override_request["overwriteEdited"] = json!(true);
        let applied = s.execute(APPLY, &override_request).expect("explicit override");
        assert_eq!(applied["changed"], 1);
        assert_eq!(s.develop_of(PhotoId(2)).expect("photo").light.contrast, 18.0);
    }

    #[test]
    fn protected_and_rejected_raws_stay_untouched() {
        let mut s = session();
        let p = json!({"reference":1,"processVersion":1,"overwriteEdited":true,
            "targets":[{"id":2,"protected":true,"recipe":recipe("r2",0.5)}]});
        assert_eq!(s.execute(PLAN, &p).expect("plan")["blocked"], 1);
        let p = approved(&mut s, p);
        assert!(s.execute(APPLY, &p).is_err());
        s.catalog.apply(Op::SetFlag { id: PhotoId(3), flag: Flag::Reject }).expect("reject");
        let p = json!({"reference":1,"processVersion":1,"overwriteEdited":true,
            "targets":[{"id":3,"recipe":recipe("r3",1.0)}]});
        assert_eq!(s.execute(PLAN, &p).expect("plan")["targets"][0]["reason"], "NOT_DELIVERABLE");
    }

    #[test]
    fn uncalibrated_white_balance_and_unknown_fields_fail_closed() {
        let mut s = session();
        for adjustments in [
            json!({"temperature":5200.0,"tint":4.0}),
            json!({"exposure":99.0}),
            json!({"unmapped_mask":42.0})
        ] {
            let p = json!({"reference":1,"processVersion":1,"targets":[
                {"id":2,"recipe":{"recipe_id":"r2",
                "source_reference_ids":["reference"],"adjustments":adjustments}}
            ]});
            assert!(s.execute(PLAN, &p).is_err());
        }
    }
}
