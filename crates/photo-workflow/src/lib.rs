//! Photo-Cake's reference-driven, per-photo adjustment boundary inside LightCraft.
//! The engine and native DevelopSettings remain the only rendering/editing authority.
//! This initial bridge deliberately does not copy Photo-Cake's catalog or XMP writer.
#![forbid(unsafe_code)]

use lightcraft_develop::{controls, DevelopSettings, WbMode, WbScale};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AdaptiveAdjustments {
    pub exposure: Option<f64>,
    pub contrast: Option<f64>,
    pub highlights: Option<f64>,
    pub shadows: Option<f64>,
    pub whites: Option<f64>,
    pub blacks: Option<f64>,
    pub saturation: Option<f64>,
    pub vibrance: Option<f64>,
    pub temperature: Option<f64>,
    pub tint: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptiveRecipe {
    /// Photo-Cake provenance, not a LightCraft develop process version.
    pub recipe_id: String,
    pub source_reference_ids: Vec<String>,
    pub adjustments: AdaptiveAdjustments,
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum RecipeError {
    #[error("recipe requires a non-empty recipe_id and at least one source reference")]
    MissingProvenance,
    #[error("temperature and tint must be supplied together")]
    IncompleteWhiteBalance,
    #[error("white balance requires explicitly verified Kelvin calibration")]
    UncalibratedWhiteBalance,
    #[error("{0} is not finite or outside the native LightCraft control range")]
    InvalidControl(&'static str),
}

/// Convert one photographer-approved adaptive recipe into native LightCraft settings.
///
/// None means "leave this target photo's existing value untouched". Reject unsupported
/// exposure/WB values instead of silently clamping or guessing camera calibration.
/// Preserves target-specific masks, crops, optics, process and all unrelated edits.
pub fn resolve_recipe(
    current: &DevelopSettings,
    recipe: &AdaptiveRecipe,
    verified_kelvin: bool,
) -> Result<DevelopSettings, RecipeError> {
    if recipe.recipe_id.trim().is_empty()
        || recipe.source_reference_ids.is_empty()
        || recipe.source_reference_ids.iter().any(|id| id.trim().is_empty())
    {
        return Err(RecipeError::MissingProvenance);
    }
    let edits = &recipe.adjustments;
    let mut result = current.clone();
    let controls_to_apply = [
        ("light.exposure", edits.exposure),
        ("light.contrast", edits.contrast),
        ("light.highlights", edits.highlights),
        ("light.shadows", edits.shadows),
        ("light.whites", edits.whites),
        ("light.blacks", edits.blacks),
        ("color.saturation", edits.saturation),
        ("color.vibrance", edits.vibrance),
    ];
    for (id, value) in controls_to_apply {
        if let Some(v) = value {
            let Some(spec) = controls::find(id) else { return Err(RecipeError::InvalidControl(id)) };
            if !v.is_finite() || !(spec.min..=spec.max).contains(&v) || !controls::set(&mut result, id, v) {
                return Err(RecipeError::InvalidControl(id));
            }
        }
    }
    match (edits.temperature, edits.tint) {
        (None, None) => {}
        (Some(temp), Some(tint)) => {
            if !verified_kelvin || current.wb.scale != WbScale::Kelvin {
                return Err(RecipeError::UncalibratedWhiteBalance);
            }
            for (id, val) in [("wb.temp", temp), ("wb.tint", tint)] {
                let Some(spec) = controls::find(id) else { return Err(RecipeError::InvalidControl(id)) };
                if !val.is_finite() || !(spec.min..=spec.max).contains(&val) {
                    return Err(RecipeError::InvalidControl(id));
                }
            }
            controls::set(&mut result, "wb.temp", temp);
            controls::set(&mut result, "wb.tint", tint);
            result.wb.mode = WbMode::Custom;
        }
        _ => return Err(RecipeError::IncompleteWhiteBalance),
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn recipe() -> AdaptiveRecipe {
        AdaptiveRecipe {
            recipe_id: "photo-cake-recipe-v1".into(),
            source_reference_ids: vec!["reference-1".into()],
            adjustments: AdaptiveAdjustments {
                exposure: Some(0.7), highlights: Some(-40.0), ..Default::default()
            },
        }
    }

    #[test]
    fn applies_selective_adjustments_without_destroying_target_edits() {
        let mut existing = DevelopSettings::default();
        existing.light.shadows = 33.0;
        existing.color.saturation = 8.0;
        let updated = resolve_recipe(&existing, &recipe(), false).unwrap();
        assert_eq!(updated.light.exposure, 0.7);
        assert_eq!(updated.light.highlights, -40.0);
        assert_eq!(updated.light.shadows, 33.0);
        assert_eq!(updated.color.saturation, 8.0);
        assert_eq!(existing.light.exposure, 0.0);
    }

    #[test]
    fn rejects_out_of_range_and_non_finite_values() {
        for exposure in [12.0, f64::NAN, f64::INFINITY] {
            let mut r = recipe();
            r.adjustments.exposure = Some(exposure);
            assert_eq!(resolve_recipe(&DevelopSettings::default(), &r, false), Err(RecipeError::InvalidControl("light.exposure")));
        }
    }

    #[test]
    fn never_guesses_white_balance_or_accepts_half_a_pair() {
        let mut r = recipe();
        r.adjustments.temperature = Some(5300.0);
        assert_eq!(resolve_recipe(&DevelopSettings::default(), &r, false), Err(RecipeError::IncompleteWhiteBalance));
        r.adjustments.tint = Some(3.0);
        assert_eq!(resolve_recipe(&DevelopSettings::default(), &r, false), Err(RecipeError::UncalibratedWhiteBalance));
        let mut d = DevelopSettings::default();
        d.wb.scale = WbScale::Kelvin;
        let result = resolve_recipe(&d, &r, true).unwrap();
        assert_eq!(result.wb.temp, 5300.0);
        assert_eq!(result.wb.tint, 3.0);
        assert_eq!(result.wb.mode, WbMode::Custom);
    }

    #[test]
    fn rejects_untraceable_recipe() {
        let mut r = recipe();
        r.source_reference_ids.clear();
        assert_eq!(resolve_recipe(&DevelopSettings::default(), &r, false), Err(RecipeError::MissingProvenance));
    }
}
