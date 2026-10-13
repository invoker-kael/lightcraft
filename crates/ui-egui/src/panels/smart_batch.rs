//! Smart Batch workstation panel: a photographer selects one edited RAW as Reference,
//! selects the peer photos in the library, previews a guarded adaptive batch, and
//! commits only ready targets through LightCraft's existing journal/undo engine.
//! No parallel editor, copied catalog or sidecar writer.
use egui::RichText;
use lightcraft_catalog::{Photo, PhotoId, parse_shutter_seconds};
use lightcraft_engine::Session;
use serde_json::{Map, Value, json};

use crate::LightcraftApp;

const PLAN: &str = "smart.recipe.preflight";
const APPLY: &str = "smart.recipe.apply";
const MOMENT: &str = "smart.moments.preview";
const FIELDS: [(&str, &str); 8] = [
    ("Exposure", "light.exposure"),
    ("Contrast", "light.contrast"),
    ("Highlights", "light.highlights"),
    ("Shadows", "light.shadows"),
    ("Whites", "light.whites"),
    ("Blacks", "light.blacks"),
    ("Saturation", "color.saturation"),
    ("Vibrance", "color.vibrance"),
];

#[derive(Clone)]
struct SmartBatchUi {
    fields: [bool; 8],
    exif_exposure: bool,
    plan: Option<(Value, Value)>,
    moment_preview: Option<Value>,
    notice: Option<String>,
}
impl Default for SmartBatchUi {
    fn default() -> Self {
        Self { fields: [true; 8], exif_exposure: true, plan: None, moment_preview: None, notice: None }
    }
}

/// Exposure proxy in EV: exposure time × ISO / aperture², not an estimate of scene brightness.
/// Only used for a photographer-selected same-lighting cohort.
fn capture_ev(photo: &Photo) -> Option<f64> {
    let t = parse_shutter_seconds(&photo.meta.shutter)?;
    let aperture = f64::from(photo.meta.aperture?);
    let iso = f64::from(photo.meta.iso?);
    if !aperture.is_finite() || aperture <= 0.0 || iso <= 0.0 { return None; }
    let ev = (t * iso / (aperture * aperture)).log2();
    ev.is_finite().then_some(ev)
}

/// Build one per-photo plan using the active photo's LOOK, preserving target-specific
/// adjustments where the photographer has disabled a field. Protect all edits in the engine.
/// Exposure adaptation is explicitly based on EXIF capture settings, not semantic similarity.
fn request(session: &Session, fields: [bool; 8], exif_exposure: bool) -> Result<(Value, usize), String> {
    let reference = session.active().ok_or("Select an edited Reference RAW first")?;
    let source = session.catalog.photo(reference).ok_or("Reference is not in the catalog")?;
    if source.kind != lightcraft_catalog::MediaKind::Raw || source.preview_only.is_some() || source.local || source.deleted {
        return Err("Reference must be an available, decoded RAW photo".into());
    }
    let source_settings = session.develop_of(reference).ok_or("Reference has no develop settings")?;
    let mut ids = session.selection.ids.clone();
    ids.retain(|id| *id != reference);
    ids.sort();
    ids.dedup();
    if ids.is_empty() || ids.len() > 1000 {
        return Err("Select 1 to 1000 target photos together with the active Reference".into());
    }
    if !fields.iter().any(|on| *on) {
        return Err("Select at least one adjustment to synchronize".into());
    }
    let source_ev = capture_ev(source);
    let mut targets = Vec::with_capacity(ids.len());
    let mut exposure_skipped = 0usize;
    for id in ids {
        let target = session.catalog.photo(id).ok_or_else(|| format!("photo {} is not in this catalog", id.0))?;
        let mut changes = Map::new();
        for (i, (_, field)) in FIELDS.iter().enumerate() {
            if !fields[i] { continue; }
            let Some(v) = lightcraft_develop::controls::get(&source_settings, field) else {
                return Err(format!("unsupported native control {field}"));
            };
            let v = if i == 0 && exif_exposure {
                match (source_ev, capture_ev(target)) {
                    (Some(r), Some(t)) => {
                        let adapted = v + r - t;
                        if adapted.is_finite() && (-5.0..=5.0).contains(&adapted) {
                            Some(adapted)
                        } else { None }
                    }
                    _ => None,
                }
            } else { Some(v) };
            if let Some(v) = v {
                changes.insert(field.rsplit('.').next().unwrap_or(field).to_owned(), json!(v));
            } else if i == 0 {
                exposure_skipped += 1;
            }
        }
        // The source identity is a LightCraft PhotoId at this stage; Photo-Cake's
        // UUID-to-PhotoId reconciliation and semantic scene matching are future work.
        targets.push(json!({
            "id": id.0,
            "recipe": {
                "recipe_id": format!("lc-ref-{}-target-{}-v1", reference.0, id.0),
                "source_reference_ids": [format!("lightcraft-photo:{}", reference.0)],
                "adjustments": changes,
            }
        }));
    }
    Ok((json!({"reference": reference.0, "processVersion": 1, "targets": targets}), exposure_skipped))
}

/// Turn the preview's approved READY rows into a transaction restricted to that cohort.
/// A blocked/unchanged target is not silently included in the apply payload.
fn approved_payload(request: &Value, report: &Value) -> Option<Value> {
    let mut payload = request.clone();
    payload["referenceHash"] = report["referenceHash"].clone();
    if !payload["referenceHash"].is_string() { return None; }
    let original = request["targets"].as_array()?;
    let rows = report["targets"].as_array()?;
    if rows.len() != original.len() { return None; }
    let mut ready = Vec::new();
    for row in rows {
        if row["status"] != "READY" { continue; }
        let id = row["id"].as_u64()?;
        let expected = row["expectedHash"].as_str()?;
        let mut item = original.iter().find(|t| t["id"].as_u64() == Some(id))?.clone();
        item["expectedHash"] = json!(expected);
        ready.push(item);
    }
    if ready.is_empty() { return None; }
    payload["targets"] = Value::Array(ready);
    Some(payload)
}

/// Select eligible non-edited peers only, retaining the photographer's current Reference.
fn moment_selection(preview: &Value, reference: PhotoId) -> Result<Vec<u64>, String> {
    let moment = &preview["activeMoment"];
    if preview["active"].as_u64() != Some(reference.0) || moment["reference"].as_u64() != Some(reference.0) {
        return Err("Moment preview belongs to a different Reference. Find Moment again.".into());
    }
    let Some(peers) = moment["eligiblePeerIds"].as_array() else {
        return Err("No moment candidates for the selected Reference".into());
    };
    if peers.len() > 1000 { return Err("Moment has over 1000 targets. Filter the library first.".into()); }
    let mut ids = vec![reference.0];
    for id in peers {
        let number = id.as_u64().ok_or("Moment contains an invalid photo ID")?;
        if !ids.contains(&number) { ids.push(number); }
    }
    if ids.len() < 2 { return Err("No unedited peer RAW photos in this Moment".into()); }
    Ok(ids)
}

pub fn show(app: &mut LightcraftApp, ui: &mut egui::Ui, id: PhotoId) {
    let state_id = egui::Id::new("photo-cake-smart-batch-ui");
    let mut state: SmartBatchUi = ui.ctx().data(|d| d.get_temp(state_id)).unwrap_or_default();
    if state.moment_preview.as_ref().is_some_and(|m| m["active"].as_u64() != Some(id.0)) {
        state.moment_preview = None;
        state.plan = None;
    }
    egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 8, bottom: 12 }).show(ui, |ui| {
        egui::CollapsingHeader::new("Smart Batch · Reference → selected photos")
            .default_open(true)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if ui.button("Find Current Moment").on_hover_text("Find nearby captures using Photo-Cake's conservative time, camera and sequence rules").clicked() {
                        match app.run(MOMENT, json!({})) {
                            Ok(result) => { state.moment_preview = Some(result); state.notice = None; }
                            Err(e) => { state.moment_preview = None; state.notice = Some(format!("Moment discovery failed: {e}")); }
                        }
                    }
                });
                if let Some(moment_data) = state.moment_preview.clone() {
                    let moment = &moment_data["activeMoment"];
                    if moment.is_null() {
                        ui.label(RichText::new("The active photo is not in a visible, eligible RAW Moment. Check the library filter.").small().weak());
                    } else {
                        let total = moment["total"].as_u64().unwrap_or(0);
                        let blocked = moment["blocked"].as_u64().unwrap_or(0);
                        let individually_edited = moment["alreadyEdited"].as_u64().unwrap_or(0);
                        let suggested = moment["suggestedReference"].as_u64();
                        ui.label(RichText::new(format!(
                            "Moment: {total} RAW · {blocked} excluded · {individually_edited} already edited · suggested Reference: {}",
                            suggested.map_or("none".into(), |id| format!("#{id}"))
                        )).small());
                        ui.label(RichText::new("This is time/sequence grouping, not AI visual matching. The current photo remains the Reference.").small().weak());
                        let selection = moment_selection(&moment_data, id);
                        if ui.add_enabled(selection.is_ok(), egui::Button::new("Select Moment Peers")).clicked() {
                            if let Ok(ids) = selection {
                                match app.run("library.select", json!({"ids": ids, "active": id.0, "mode": "replace"})) {
                                    Ok(_) => {
                                        state.plan = None;
                                        state.moment_preview = None;
                                        state.notice = Some("Moment peers selected. Review settings, then Check Batch.".into());
                                    }
                                    Err(e) => { state.notice = Some(format!("Moment selection failed: {e}")); }
                                }
                            }
                        }
                    }
                }
                ui.separator();
                ui.label(RichText::new("Current active RAW = Reference. Only selected peers are candidates.").small());
                ui.label(RichText::new("EXIF exposure matching is a starting estimate, not scene-aware AI. Review each result.").small().weak());
                ui.add_space(5.0);
                egui::Grid::new("smart-batch-fields").num_columns(2).show(ui, |ui| {
                    for (i, (label, _)) in FIELDS.iter().enumerate() {
                        ui.checkbox(&mut state.fields[i], *label);
                        if i % 2 == 1 { ui.end_row(); }
                    }
                });
                ui.checkbox(&mut state.exif_exposure, "Adapt exposure from shutter / aperture / ISO");
                let current = request(&app.session, state.fields, state.exif_exposure);
                match current {
                    Err(ref msg) => { ui.label(RichText::new(msg).weak()); state.plan = None; }
                    Ok((ref body, skipped)) => {
                        if state.plan.as_ref().is_some_and(|(previous, _)| previous != body) {
                            state.plan = None;
                            state.notice = Some("Selection or Reference settings changed — check batch again.".into());
                        }
                        let count = body["targets"].as_array().map_or(0, Vec::len);
                        ui.label(format!("{count} selected targets; {skipped} lack safe EXIF exposure adaptation"));
                        if ui.button("Check Batch").clicked() {
                            match app.run(PLAN, body.clone()) {
                                Ok(report) => { state.plan = Some((body.clone(), report)); state.notice = None; }
                                Err(e) => { state.plan = None; state.notice = Some(format!("Preflight failed: {e}")); }
                            }
                        }
                        if let Some((_, ref report)) = state.plan {
                            let ready = report["ready"].as_u64().unwrap_or(0);
                            let blocked = report["blocked"].as_u64().unwrap_or(0);
                            let unchanged = count.saturating_sub((ready + blocked) as usize);
                            ui.label(format!("Ready {ready} · Protected {blocked} · Unchanged {unchanged}"));
                            for row in report["targets"].as_array().into_iter().flatten().filter(|r| r["status"] == "BLOCKED").take(5) {
                                ui.label(RichText::new(format!("Photo {}: {}", row["id"], row["reason"].as_str().unwrap_or("blocked"))).small().weak());
                            }
                            let approved = approved_payload(body, report);
                            if ui.add_enabled(approved.is_some(), egui::Button::new(format!("Apply {ready} Ready · One Undo"))).clicked() {
                                if let Some(payload) = approved {
                                    match app.run(APPLY, payload) {
                                        Ok(response) => {
                                            state.notice = Some(format!("Applied to {} photos. Use Edit → Undo to revert the whole batch.", response["changed"]));
                                            state.plan = None;
                                        }
                                        Err(e) => {
                                            state.notice = Some(format!("No batch applied: {e}. Run Check Batch again."));
                                            state.plan = None;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                if let Some(msg) = &state.notice {
                    ui.label(RichText::new(msg).small());
                }
                ui.label(RichText::new("Existing individual edits, rejected and unsupported RAW files are blocked by default. HDR sources need explicit exclusion until Photo-Cake protection metadata is migrated.").small().weak());
            });
    });
    ui.ctx().data_mut(|d| d.insert_temp(state_id, state));
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_catalog::{MediaKind, Op, Source};
    use lightcraft_engine::Selection;

    fn fixture() -> Session {
        let mut s = Session::new();
        for id in 1..=3 {
            let mut p = Photo::new(PhotoId(id), Source::Demo { scene: id as u32 }, &format!("{id}.dng"), "DNG", 240, 160, "2026-10-13");
            p.kind = MediaKind::Raw;
            p.meta.iso = Some(100);
            p.meta.aperture = Some(2.8);
            p.meta.shutter = match id { 1 => "1/250".into(), 2 => "1/500".into(), _ => "unknown".into() };
            s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).expect("test fixture");
        }
        s.selection = Selection { ids: vec![PhotoId(1), PhotoId(2), PhotoId(3)], active: Some(PhotoId(1)) };
        s
    }

    #[test]
    fn moment_selection_keeps_reference_and_requires_same_active_photo() {
        let preview = json!({"active": 1, "activeMoment": {"reference": 1, "eligiblePeerIds": [2,3]}});
        assert_eq!(moment_selection(&preview, PhotoId(1)).expect("peers"), vec![1,2,3]);
        assert!(moment_selection(&preview, PhotoId(9)).is_err());
        assert!(moment_selection(&json!({"active":1,"activeMoment": {"reference":1,"eligiblePeerIds":[]}}), PhotoId(1)).is_err());
    }

    #[test]
    fn per_target_exposure_uses_capture_data_but_never_guesses_missing_ev() {
        let mut s = fixture();
        let mut d = (*s.develop_of(PhotoId(1)).expect("reference")).clone();
        d.light.exposure = 0.2;
        d.light.highlights = -30.0;
        s.set_develop(PhotoId(1), d, "Reference").expect("edit");
        let (body, skipped) = request(&s, [true; 8], true).expect("plan");
        assert_eq!(skipped, 1);
        assert!((body["targets"][0]["recipe"]["adjustments"]["exposure"].as_f64().expect("ev") - 1.2).abs() < 0.0001);
        assert_eq!(body["targets"][1]["recipe"]["adjustments"]["exposure"], Value::Null);
        assert_eq!(body["targets"][1]["recipe"]["adjustments"]["highlights"], -30.0);
    }

    #[test]
    fn ready_rows_only_are_approved_for_atomic_apply() {
        let (body, _) = request(&fixture(), [true; 8], false).expect("request");
        let report = json!({"referenceHash": "1234", "targets": [
            {"id":2,"status":"READY","expectedHash":"abc"},
            {"id":3,"status":"BLOCKED","expectedHash":"def"}
        ]});
        let payload = approved_payload(&body, &report).expect("ready");
        assert_eq!(payload["targets"].as_array().expect("targets").len(), 1);
        assert_eq!(payload["targets"][0]["expectedHash"], "abc");
        assert_eq!(payload["referenceHash"], "1234");
    }
}
