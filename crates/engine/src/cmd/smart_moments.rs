//! Read-only Photo-Cake Moment discovery using native LightCraft catalog evidence.
//! This does not claim embeddings, persistent Moment UUIDs or AI scene classification.
use lightcraft_catalog::{Flag, MediaKind, Photo, PhotoId, Source};
use lightcraft_photo_workflow::moment::{initial_moments, MomentAsset, MomentConfig};
use serde_json::{json, Value};
use super::{always, bad, cmd, CommandSpec};
use crate::{Result, Session};

const CMD: &str = "smart.moments.preview";
const MAX_VISIBLE: usize = 5_000;

/// Photographer Pick overrides quality hints. Analysed "best" follows,
/// then high ratings and measured sharpness/clipping; no invented AI score.
fn candidate_key(photo: &Photo) -> (u8, u8, i32, i32, u64) {
    let rank = if photo.flag == Flag::Pick { 0 }
        else if photo.analysis.is_some_and(|a| a.best) { 1 }
        else if photo.rating >= 4 { 2 } else { 3 };
    let sharpness = photo.analysis.map_or(-1, |a| (a.sharpness * 100.0).round() as i32);
    let clipping = photo.analysis.map_or(100_000, |a| (a.clipped * 100_000.0).round() as i32);
    (rank, 5u8.saturating_sub(photo.rating), -sharpness, clipping, photo.id.0)
}

fn eligible(photo: &Photo) -> bool {
    !photo.deleted && !photo.local && photo.kind == MediaKind::Raw
        && photo.preview_only.is_none() && photo.flag != Flag::Reject
}

fn preview(session: &mut Session, _: &Value) -> Result<Value> {
    let Some(active) = session.active() else {
        return Err(bad(CMD, "select the RAW photo whose Moment should be found"));
    };
    let visible = session.visible_cloned();
    if visible.len() > MAX_VISIBLE {
        return Err(bad(CMD, format!("{} visible photos; narrow the library filter to {} or fewer", visible.len(), MAX_VISIBLE)));
    }
    let mut evidence = Vec::with_capacity(visible.len());
    for id in visible {
        let Some(photo) = session.catalog.photo(id) else { continue };
        if photo.deleted || photo.local || photo.kind != MediaKind::Raw { continue }
        let capture_ms = photo.captured.as_deref()
            .and_then(lightcraft_meta::DateTime::parse_iso)
            .map(|d| d.unix_seconds().saturating_mul(1000) + i64::from(d.millis));
        evidence.push(MomentAsset {
            id: id.0,
            file_name: photo.file_name.clone(),
            camera: (!photo.meta.camera.is_empty()).then(|| photo.meta.camera.clone()),
            folder: match &photo.source {
                Source::File { path } => std::path::Path::new(path).parent().map(|folder| folder.to_string_lossy().into_owned()),
                Source::Demo { .. } => None,
            },
            capture_ms,
        });
    }
    // If the active photo is outside the current filter, never suggest a different
    // photo's group as its moment.
    let groups = initial_moments(&evidence, MomentConfig::default());
    let active_group = groups.iter().find(|g| g.asset_ids.contains(&active.0));
    let mut details = Value::Null;
    if let Some(group) = active_group {
        let mut eligible_ids: Vec<PhotoId> = group.asset_ids.iter().copied().map(PhotoId)
            .filter(|id| session.catalog.photo(*id).is_some_and(|photo| eligible(photo.as_ref()))).collect();
        eligible_ids.sort_by_key(|id| session.catalog.photo(*id).map(|photo| candidate_key(photo.as_ref())));
        let suggested = eligible_ids.first().map(|id| id.0);
        let peers: Vec<u64> = eligible_ids.iter().filter(|id| **id != active)
            .filter(|id| session.catalog.photo(**id).is_some_and(|photo| !photo.is_edited()))
            .map(|id| id.0).collect();
        details = json!({
            "key": group.key,
            "basis": group.basis,
            "photoIds": group.asset_ids,
            "suggestedReference": suggested,
            "eligiblePeerIds": peers,
            "reference": active.0,
            "total": group.asset_ids.len(),
            "blocked": group.asset_ids.len().saturating_sub(eligible_ids.len()),
            "alreadyEdited": eligible_ids.iter().filter(|id| **id != active && session.catalog.photo(**id).is_some_and(|photo| photo.is_edited())).count(),
        });
    }
    Ok(json!({"active": active.0,"scope": "visible_RAW_only","inspected": evidence.len(),"momentCount":groups.len(),"activeMoment":details}))
}
pub fn specs() -> Vec<CommandSpec> {
    vec![cmd!(query "smart.moments.preview", "Preview Photo-Cake Moment", [], None,
        "{} -> current active RAW's conservative time/sequence Moment, candidate/reference and guard counts; no writes",
        always, preview)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_catalog::{MediaKind, Op, Source};
    fn session() -> Session {
        let mut s=Session::new();
        for (id,name,camera,ts,sharp) in [
            (1,"IMG_0101.CR3","Canon","2026-10-13T13:00:00",Some(40.0)),
            (2,"IMG_0102.CR3","Canon","2026-10-13T13:00:01",Some(90.0)),
            (3,"IMG_0103.CR3","Sony","2026-10-13T13:00:02",Some(99.0)),
        ] {
            let mut p=Photo::new(PhotoId(id), Source::Demo {scene:id as u32}, name, "CR3",800,600,"2026-10-13");
            p.kind=MediaKind::Raw;
            p.meta.camera=camera.into();
            p.captured=Some(ts.into());
            p.analysis=sharp.map(|sharpness| lightcraft_catalog::Analysis {sharpness,clipped:0.03,group:None,best:false});
            s.catalog.apply(Op::AddPhoto{photo:Box::new(p)}).expect("test fixture");
        }
        s.selection.active=Some(PhotoId(1));
        s
    }
    #[test]
    fn finds_active_moment_without_writing_and_suggests_measured_candidate() {
        let mut s=session();let before=s.undo.len();
        let result=preview(&mut s,&json!({})).expect("plan");
        assert_eq!(result["activeMoment"]["photoIds"],json!([1,2]));
        assert_eq!(result["activeMoment"]["suggestedReference"],2);
        assert_eq!(result["activeMoment"]["eligiblePeerIds"],json!([2]));
        assert_eq!(s.undo.len(),before);
    }
    #[test]
    fn manual_pick_takes_priority_and_reject_is_blocked() {
        let mut s=session();
        s.catalog.apply(Op::SetFlag{id:PhotoId(1),flag:Flag::Pick}).expect("pick");
        assert_eq!(preview(&mut s,&json!({})).expect("preview")["activeMoment"]["suggestedReference"],1);
        s.catalog.apply(Op::SetFlag{id:PhotoId(2),flag:Flag::Reject}).expect("reject");
        let r=preview(&mut s,&json!({})).expect("preview");
        assert_eq!(r["activeMoment"]["eligiblePeerIds"],json!([]));
        assert_eq!(r["activeMoment"]["blocked"],1);
    }
}
