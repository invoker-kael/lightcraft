//! Conservative Photo-Cake Moment grouping, independent of LightCraft catalog storage.
//! This is the metadata/sequence parent group, NOT semantic image similarity.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MomentAsset {
    pub id: u64,
    pub file_name: String,
    pub camera: Option<String>,
    /// Keep sibling files from different source folders apart, especially without EXIF dates.
    pub folder: Option<String>,
    /// Capture time in milliseconds; never infer a timestamp from import time.
    pub capture_ms: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MomentBasis {
    Singleton,
    Time,
    TimeAndSequence,
    SequenceFallback,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MomentGroup {
    /// Deterministic for a fixed input: the first member's native catalog photo ID.
    /// This is a discovery key, not yet a persistent Photo-Cake Moment UUID.
    pub key: String,
    pub basis: MomentBasis,
    pub asset_ids: Vec<u64>,
}

#[derive(Clone, Copy, Debug)]
pub struct MomentConfig {
    pub short_gap_ms: u64,
    pub max_gap_ms: u64,
    pub max_sequence_gap: u64,
}
impl Default for MomentConfig {
    fn default() -> Self {
        Self { short_gap_ms: 1_500, max_gap_ms: 4_000, max_sequence_gap: 1 }
    }
}

fn stem_and_sequence(name: &str) -> (&str, Option<u64>) {
    let file = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let stem = file.rsplit_once('.').map_or(file, |(s, _)| s);
    let prefix = stem.trim_end_matches(|ch: char| ch.is_ascii_digit());
    let digits = &stem[prefix.len()..];
    (prefix, (!digits.is_empty()).then(|| digits.parse::<u64>().ok()).flatten())
}

fn joins(a: &MomentAsset, b: &MomentAsset, config: MomentConfig) -> Option<MomentBasis> {
    if let (Some(left), Some(right)) = (&a.folder, &b.folder) {
        if left != right { return None; }
    }
    if let (Some(left), Some(right)) = (&a.camera, &b.camera) {
        if !left.is_empty() && !right.is_empty() && !left.eq_ignore_ascii_case(right) { return None; }
    }
    let (a_prefix, a_seq) = stem_and_sequence(&a.file_name);
    let (b_prefix, b_seq) = stem_and_sequence(&b.file_name);
    let close_sequence = a_seq.zip(b_seq).is_some_and(|(a, b)| a.abs_diff(b) <= config.max_sequence_gap);
    let same_prefix = a_prefix.eq_ignore_ascii_case(b_prefix);
    match (a.capture_ms, b.capture_ms) {
        (Some(a), Some(b)) => {
            let gap = a.abs_diff(b);
            if gap <= config.short_gap_ms {
                Some(if close_sequence { MomentBasis::TimeAndSequence } else { MomentBasis::Time })
            } else if gap <= config.max_gap_ms && close_sequence && same_prefix {
                Some(MomentBasis::TimeAndSequence)
            } else { None }
        }
        _ if close_sequence && same_prefix => Some(MomentBasis::SequenceFallback),
        _ => None,
    }
}

fn merged(current: MomentBasis, next: MomentBasis) -> MomentBasis {
    match (current, next) {
        (_, MomentBasis::TimeAndSequence) => MomentBasis::TimeAndSequence,
        (MomentBasis::Singleton, kind) => kind,
        (MomentBasis::SequenceFallback, MomentBasis::Time) => MomentBasis::Time,
        (kind, _) => kind,
    }
}

/// Port of Photo-Cake's 1.5s/4s initial grouping rules. No files are moved
/// and no catalog groups are written. Caller is responsible for manual locks
/// and photographer overrides before promoting groups to persistent state.
pub fn initial_moments(assets: &[MomentAsset], config: MomentConfig) -> Vec<MomentGroup> {
    if assets.is_empty() { return Vec::new(); }
    let mut sorted = assets.to_vec();
    sorted.sort_by(|a,b| match (a.capture_ms, b.capture_ms) {
        (Some(x), Some(y)) => x.cmp(&y).then_with(|| a.file_name.cmp(&b.file_name)).then_with(|| a.id.cmp(&b.id)),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => {
            let (pa,sa) = stem_and_sequence(&a.file_name);
            let (pb,sb) = stem_and_sequence(&b.file_name);
            pa.cmp(pb).then_with(|| sa.cmp(&sb)).then_with(|| a.file_name.cmp(&b.file_name)).then_with(|| a.id.cmp(&b.id))
        }
    });
    let mut result: Vec<MomentGroup> = Vec::new();
    let mut current = vec![sorted[0].id];
    let mut basis = MomentBasis::Singleton;
    for pair in sorted.windows(2) {
        if let Some(reason) = joins(&pair[0], &pair[1], config) {
            current.push(pair[1].id);
            basis = merged(basis, reason);
        } else {
            result.push(MomentGroup { key: format!("moment:{}", current[0]), asset_ids: std::mem::take(&mut current), basis });
            current.push(pair[1].id);
            basis = MomentBasis::Singleton;
        }
    }
    result.push(MomentGroup { key: format!("moment:{}", current[0]), asset_ids: current, basis });
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn asset(id: u64, name: &str, camera: &str, time: Option<i64>) -> MomentAsset {
        MomentAsset { id, file_name: name.into(), camera: Some(camera.into()), folder: None, capture_ms: time }
    }
    #[test]
    fn groups_close_captures_and_splits_different_cameras() {
        let photos = [
            asset(3,"IMG_1003.CR3","Canon",Some(20_000)),
            asset(1,"IMG_1001.CR3","Canon",Some(10_000)),
            asset(2,"IMG_1002.CR3","Canon",Some(11_100)),
            asset(4,"IMG_1004.CR3","Sony",Some(20_200)),
        ];
        let groups = initial_moments(&photos, MomentConfig::default());
        assert_eq!(groups.iter().map(|g| g.asset_ids.as_slice()).collect::<Vec<_>>(), vec![&[1,2][..], &[3][..], &[4][..]]);
        assert_eq!(groups[0].basis, MomentBasis::TimeAndSequence);
        assert_eq!(groups[0].key, "moment:1");
    }
    #[test]
    fn sequence_joins_with_missing_time_but_not_distant_frames() {
        let photos = [
            asset(1,"DSC_4001.ARW","Sony",None),
            asset(2,"DSC_4002.ARW","Sony",None),
            asset(3,"DSC_4200.ARW","Sony",None),
        ];
        let groups = initial_moments(&photos, MomentConfig::default());
        assert_eq!(groups[0].asset_ids, vec![1,2]);
        assert_eq!(groups[0].basis, MomentBasis::SequenceFallback);
        assert_eq!(groups.len(),2);
    }
    #[test]
    fn four_second_rule_needs_prefix_and_sequence() {
        let photos=[
            asset(1,"IMG_0001.RAF","Fuji",Some(10_000)),
            asset(2,"IMG_0002.RAF","Fuji",Some(13_000)),
            asset(3,"ALT_0003.RAF","Fuji",Some(16_000)),
        ];
        let groups=initial_moments(&photos,MomentConfig::default());
        assert_eq!(groups[0].asset_ids, vec![1,2]);
        assert_eq!(groups.len(),2);
    }
    #[test]
    fn same_sequence_in_other_folder_is_not_one_moment() {
        let mut a = asset(1,"IMG_0001.CR3","Canon",None);
        let mut b = asset(2,"IMG_0002.CR3","Canon",None);
        a.folder = Some("trip-a".into());
        b.folder = Some("trip-b".into());
        assert_eq!(initial_moments(&[a,b],MomentConfig::default()).len(),2);
    }
    #[test]
    fn empty_and_input_order_are_stable() {
        assert!(initial_moments(&[],MomentConfig::default()).is_empty());
        let a=[asset(2,"IMG_0002.DNG","A",Some(101)),asset(1,"IMG_0001.DNG","A",Some(100))];
        let mut b=a.clone();b.reverse();
        assert_eq!(initial_moments(&a,MomentConfig::default()),initial_moments(&b,MomentConfig::default()));
    }
}
