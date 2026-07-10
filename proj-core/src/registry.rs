use crate::crs::{CrsDef, VerticalCrsDef};
use crate::datum::Datum;
use crate::epsg_db;
use crate::error::{Error, Result};
use crate::grid::GridDefinition;
use crate::operation::{
    CoordinateOperation, CoordinateOperationId, CoordinateOperationMetadata, SelectionOptions,
    VerticalGridOperation,
};
use crate::selector;

/// Look up a CRS definition by EPSG code.
///
/// Searches the embedded EPSG database (~5,600 CRS definitions) covering all
/// geographic 2D CRS and all projected CRS that use supported projection methods.
///
/// Returns `None` if the EPSG code is not in the database.
pub fn lookup_epsg(code: u32) -> Option<CrsDef> {
    epsg_db::lookup(code)
}

/// Look up a datum definition by EPSG code.
pub fn lookup_datum_epsg(code: u32) -> Option<Datum> {
    epsg_db::lookup_datum(code)
}

/// Look up the horizontal datum EPSG code for a CRS definition by CRS EPSG code.
pub fn lookup_datum_code_for_crs(code: u32) -> Option<u32> {
    epsg_db::lookup_datum_code_for_crs(code)
}

/// Look up the ellipsoid EPSG code for a datum by datum EPSG code.
pub fn lookup_ellipsoid_code_for_datum(code: u32) -> Option<u32> {
    epsg_db::lookup_ellipsoid_code_for_datum(code)
}

/// Look up a supported vertical CRS definition by EPSG code.
///
/// Standalone vertical CRS values are not valid horizontal transform inputs,
/// but parsers use this registry to canonicalize vertical components inside
/// compound CRS definitions.
pub fn lookup_vertical_epsg(code: u32) -> Option<VerticalCrsDef> {
    epsg_db::lookup_vertical(code)
}

/// Return deterministic provenance for the embedded EPSG registry artifact.
///
/// The JSON documents the generator, binary registry format, source PROJ
/// database metadata and normalized content checksum, registry counts, and
/// generated `epsg.bin` checksum.
pub fn embedded_registry_provenance_json() -> &'static str {
    epsg_db::PROVENANCE_JSON
}

/// Look up a coordinate operation by its identifier.
pub fn lookup_operation(id: CoordinateOperationId) -> Option<CoordinateOperation> {
    epsg_db::lookup_operation(id.0)
}

/// Look up a registry-backed vertical grid operation by EPSG operation code.
pub fn lookup_vertical_grid_operation(id: CoordinateOperationId) -> Option<VerticalGridOperation> {
    epsg_db::lookup_vertical_grid_operation(id.0)
}

/// Look up a grid definition by its identifier.
pub(crate) fn lookup_grid_definition(id: u32) -> Option<GridDefinition> {
    epsg_db::lookup_grid(id)
}

pub(crate) fn related_operations(
    source: &CrsDef,
    target: &CrsDef,
) -> Vec<&'static CoordinateOperation> {
    epsg_db::related_operations(
        source.base_geographic_crs_epsg(),
        target.base_geographic_crs_epsg(),
    )
}

/// Return all registry operations compatible with the source and target CRS.
pub fn operations_between(source: &CrsDef, target: &CrsDef) -> Vec<CoordinateOperation> {
    epsg_db::forward_operations(
        source.base_geographic_crs_epsg(),
        target.base_geographic_crs_epsg(),
    )
    .into_iter()
    .cloned()
    .collect()
}

/// Return registry-backed vertical grid operations compatible with the CRS pair.
///
/// This includes reverse-compatible operations when transforming from a
/// gravity-related vertical CRS back to ellipsoidal height.
pub fn vertical_grid_operations_between(
    source: &CrsDef,
    target: &CrsDef,
) -> Vec<VerticalGridOperation> {
    epsg_db::vertical_grid_operations_between(source, target)
}

/// Return selectable operation metadata for the source and target CRS.
///
/// Unlike [`operations_between`], this discovery API reports the direction each
/// operation would run for this CRS pair and includes reverse-compatible
/// operations.
pub fn operation_candidates_between(
    source: &CrsDef,
    target: &CrsDef,
) -> Result<Vec<CoordinateOperationMetadata>> {
    operation_candidates_between_with_selection_options(
        source,
        target,
        &SelectionOptions::default(),
    )
}

/// Return selectable operation metadata using the same policy and AOI validation
/// rules as [`crate::Transform::with_selection_options`].
pub fn operation_candidates_between_with_selection_options(
    source: &CrsDef,
    target: &CrsDef,
    options: &SelectionOptions,
) -> Result<Vec<CoordinateOperationMetadata>> {
    let candidates = selector::rank_operation_candidates(source, target, options)?;
    Ok(candidates
        .ranked
        .into_iter()
        .map(|candidate| candidate.metadata(source, target))
        .collect())
}

/// Parse an authority:code string (e.g., "EPSG:4326") and look up the CRS definition.
///
/// Currently only supports the "EPSG" authority.
pub fn lookup_authority_code(code: &str) -> Result<CrsDef> {
    let Some((authority, code_str)) = code.split_once(':') else {
        return Err(Error::UnknownCrs(format!(
            "invalid authority:code format: {code}"
        )));
    };

    let authority = authority.trim();
    let code_str = code_str.trim();

    if !authority.eq_ignore_ascii_case("EPSG") {
        return Err(Error::UnknownCrs(format!(
            "unsupported authority: {authority} (only EPSG is supported)"
        )));
    }

    let epsg: u32 = code_str
        .parse()
        .map_err(|_| Error::UnknownCrs(format!("invalid EPSG code: {code_str}")))?;

    lookup_epsg(epsg).ok_or_else(|| Error::UnknownCrs(format!("unknown EPSG code: {epsg}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn lookup_wgs84() {
        let crs = lookup_epsg(4326).expect("should find 4326");
        assert!(crs.is_geographic());
        assert_eq!(crs.epsg(), 4326);
        assert_eq!(crs.name(), "WGS 84");
    }

    #[test]
    fn lookup_wgs84_3d() {
        let crs = lookup_epsg(4979).expect("should find 4979");
        assert!(crs.is_compound());
        assert!(crs.is_geographic());
        assert_eq!(crs.epsg(), 4979);
        assert_eq!(crs.base_geographic_crs_epsg(), Some(4326));
        assert!(crs.vertical_crs().is_some());
    }

    #[test]
    fn lookup_navd88_vertical_crs() {
        let crs = lookup_vertical_epsg(5703).expect("should find NAVD88 height");
        assert_eq!(crs.epsg(), 5703);
        assert_eq!(crs.vertical_datum_epsg(), Some(5103));
    }

    #[test]
    fn lookup_common_vertical_crs_codes() {
        let egm2008 = lookup_vertical_epsg(3855).expect("should find EGM2008 height");
        assert_eq!(egm2008.vertical_datum_epsg(), Some(1027));
        assert_eq!(egm2008.linear_unit_to_meter(), 1.0);

        let ngvd29_ft = lookup_vertical_epsg(5702).expect("should find NGVD29 ftUS height");
        assert_eq!(ngvd29_ft.vertical_datum_epsg(), Some(5102));
        assert!(
            (ngvd29_ft.linear_unit_to_meter()
                - crate::crs::LinearUnit::us_survey_foot().meters_per_unit())
            .abs()
                < 1e-14
        );

        let egm96 = lookup_vertical_epsg(5773).expect("should find EGM96 height");
        assert_eq!(egm96.vertical_datum_epsg(), Some(5171));

        let navd88_ft = lookup_vertical_epsg(6360).expect("should find NAVD88 ftUS height");
        assert_eq!(navd88_ft.vertical_datum_epsg(), Some(5103));
        assert!(
            (navd88_ft.linear_unit_to_meter()
                - crate::crs::LinearUnit::us_survey_foot().meters_per_unit())
            .abs()
                < 1e-14
        );
    }

    #[test]
    fn lookup_rdnap_compound_crs() {
        let crs = lookup_epsg(7415).expect("should find RD New + NAP height");
        assert!(crs.is_compound());
        assert!(crs.is_projected());
        assert_eq!(crs.epsg(), 7415);
        assert_eq!(crs.base_geographic_crs_epsg(), Some(4289));
        let vertical = crs.vertical_crs().expect("compound vertical CRS");
        assert_eq!(vertical.epsg(), 5709);
        assert_eq!(vertical.vertical_datum_epsg(), Some(5109));
    }

    #[test]
    fn rdnap_candidates_are_registry_generated() {
        let source = lookup_epsg(4979).expect("should find WGS 84 3D");
        let target = lookup_epsg(7415).expect("should find RD New + NAP height");

        let candidates = operation_candidates_between(&source, &target).unwrap();
        assert!(candidates.iter().any(|candidate| {
            candidate.name == "WGS 84 to Amersfoort (generated from EPSG:1149 + EPSG:9909282)"
                && candidate.uses_grids
        }));

        let vertical_operations = vertical_grid_operations_between(&source, &target);
        assert!(vertical_operations.iter().any(|operation| {
            operation.grid.format == crate::grid::GridFormat::GeoTiff
                && operation
                    .grid
                    .resource_names
                    .iter()
                    .any(|name| name == "nl_nsgi_nlgeo2018.tif")
        }));
    }

    #[test]
    fn generated_graph_composes_non_rdnap_horizontal_bridge() {
        let source = lookup_epsg(4326).expect("should find WGS 84");
        let target = lookup_epsg(4277).expect("should find OSGB36");

        let candidates = operation_candidates_between(&source, &target).unwrap();

        assert!(candidates.iter().any(|candidate| {
            candidate.name == "WGS 84 to OSGB36 (generated from EPSG:1149 + EPSG:9907709)"
                && candidate.source_crs_epsg == Some(4326)
                && candidate.target_crs_epsg == Some(4277)
                && candidate.uses_grids
        }));
    }

    #[test]
    fn generated_graph_composes_non_rdnap_vertical_bridge() {
        let source = lookup_epsg(4979).expect("should find WGS 84 3D");
        let target = lookup_epsg(7405).expect("should find OSGB36 + ODN height");

        let vertical_operations = vertical_grid_operations_between(&source, &target);

        assert!(vertical_operations.iter().any(|operation| {
            operation.name.contains("(generated horizontal CRS bridge)")
                && operation.target_vertical_crs_epsg == Some(5701)
                && operation.grid.format == crate::grid::GridFormat::GeoTiff
                && operation
                    .grid
                    .resource_names
                    .iter()
                    .any(|name| name == "uk_os_OSGM15_GB.tif")
        }));
    }

    #[test]
    fn lookup_registry_vertical_grid_operation() {
        let operation = lookup_vertical_grid_operation(CoordinateOperationId(10013))
            .expect("should find NAD83 to NAVD88 vertical grid operation");
        assert_eq!(operation.target_vertical_crs_epsg, Some(5703));
        assert_eq!(operation.target_vertical_datum_epsg, Some(5103));
        assert_eq!(operation.grid.format, crate::grid::GridFormat::Gtx);
        assert!(operation
            .grid
            .resource_names
            .iter()
            .any(|name| name == "g2003u01.bin"));
    }

    #[test]
    fn discovers_registry_vertical_grid_operations_for_crs_pair() {
        let source_horizontal = lookup_epsg(4269).expect("should find NAD83");
        let geographic = source_horizontal.as_geographic().unwrap().clone();
        let horizontal = crate::HorizontalCrsDef::Geographic(geographic.clone());
        let source_vertical = VerticalCrsDef::ellipsoidal_height(
            0,
            geographic.datum().clone(),
            crate::LinearUnit::metre(),
            "NAD83 ellipsoidal height",
        );
        let source = CrsDef::Compound(Box::new(crate::CompoundCrsDef::new(
            0,
            horizontal.clone(),
            source_vertical,
            "NAD83 + ellipsoidal height",
        )));
        let target = CrsDef::Compound(Box::new(crate::CompoundCrsDef::new(
            0,
            horizontal,
            lookup_vertical_epsg(5703).unwrap(),
            "NAD83 + NAVD88 height",
        )));

        let operations = vertical_grid_operations_between(&source, &target);
        assert!(!operations.is_empty());
        assert!(operations
            .iter()
            .any(|operation| operation.target_vertical_crs_epsg == Some(5703)
                && operation.grid.format == crate::grid::GridFormat::Gtx));
    }

    #[test]
    fn embedded_registry_provenance_reports_source_database() {
        let value: serde_json::Value =
            serde_json::from_str(embedded_registry_provenance_json()).unwrap();
        assert_eq!(value["schema_version"], 4);
        assert_eq!(value["registry_format"]["version"], 8);
        assert_eq!(
            value["source_database"]["metadata"]["PROJ.VERSION"],
            "9.6.2"
        );
        assert_eq!(
            value["source_database"]["metadata"]["EPSG.VERSION"],
            "v12.013"
        );
        assert!(value["source_database"]["normalized_content_sha256"]
            .as_str()
            .unwrap()
            .starts_with("sha256:"));
        assert_eq!(value["output"]["byte_len"], 1082565);
        assert_eq!(value["counts"]["vertical_crs"], 293);
        assert_eq!(value["counts"]["compound_crs"], 684);
        assert_eq!(value["counts"]["grid_resources"], 726);
        assert_eq!(value["counts"]["operations"], 2157);
        assert_eq!(value["counts"]["vertical_operations"], 716);
        assert!(value["output"]["sha256"]
            .as_str()
            .unwrap()
            .starts_with("sha256:"));
    }

    #[test]
    fn lookup_web_mercator() {
        let crs = lookup_epsg(3857).expect("should find 3857");
        assert!(crs.is_projected());
        assert_eq!(crs.epsg(), 3857);
        assert_eq!(crs.name(), "WGS 84 / Pseudo-Mercator");
    }

    #[test]
    fn lookup_polar_stereo_north() {
        let crs = lookup_epsg(3413).expect("should find 3413");
        assert!(crs.is_projected());
    }

    #[test]
    fn lookup_utm_zone_18n() {
        let crs = lookup_epsg(32618).expect("should find UTM 18N");
        assert!(crs.is_projected());
        assert_eq!(crs.epsg(), 32618);
    }

    #[test]
    fn lookup_utm_zone_1s() {
        let crs = lookup_epsg(32701).expect("should find UTM 1S");
        assert!(crs.is_projected());
        assert_eq!(crs.epsg(), 32701);
    }

    #[test]
    fn lookup_utm_zone_60n() {
        let crs = lookup_epsg(32660).expect("should find UTM 60N");
        assert!(crs.is_projected());
    }

    #[test]
    fn lookup_unknown_epsg() {
        assert!(lookup_epsg(99999).is_none());
    }

    #[test]
    fn authority_code_parse() {
        let crs = lookup_authority_code("EPSG:4326").expect("should parse");
        assert_eq!(crs.epsg(), 4326);
    }

    #[test]
    fn authority_code_case_insensitive() {
        let crs = lookup_authority_code("epsg:3857").expect("should parse");
        assert_eq!(crs.epsg(), 3857);
    }

    #[test]
    fn authority_code_invalid_format() {
        assert!(lookup_authority_code("NONSENSE").is_err());
    }

    #[test]
    fn authority_code_unknown() {
        assert!(lookup_authority_code("EPSG:99999").is_err());
    }

    #[test]
    fn authority_code_non_epsg() {
        assert!(lookup_authority_code("OGC:CRS84").is_err());
    }

    #[test]
    fn nad27_lookup() {
        let crs = lookup_epsg(4267).expect("should find NAD27");
        assert!(crs.is_geographic());
    }

    #[test]
    fn new_zealand_tm() {
        let crs = lookup_epsg(2193).expect("should find NZTM 2000");
        assert!(crs.is_projected());
    }

    #[test]
    fn lookup_new_projection_families() {
        for epsg in [3035, 3408, 9311, 28992, 3078, 2056, 30200, 32662] {
            let crs = lookup_epsg(epsg).unwrap_or_else(|| panic!("should find EPSG:{epsg}"));
            assert!(crs.is_projected(), "EPSG:{epsg} should be projected");
        }
    }

    #[test]
    fn readme_advertised_epsg_codes_resolve() {
        let readme_path = concat!(env!("CARGO_MANIFEST_DIR"), "/../README.md");
        let readme = std::fs::read_to_string(readme_path)
            .unwrap_or_else(|err| panic!("failed to read {readme_path}: {err}"));
        let epsg_codes = readme_advertised_epsg_codes(&readme);

        assert!(
            !epsg_codes.is_empty(),
            "README advertised EPSG code parser found no codes"
        );

        let missing = epsg_codes
            .iter()
            .copied()
            .filter(|code| lookup_epsg(*code).is_none() && lookup_vertical_epsg(*code).is_none())
            .map(|code| format!("EPSG:{code}"))
            .collect::<Vec<_>>();

        assert!(
            missing.is_empty(),
            "README advertises unsupported EPSG codes: {}",
            missing.join(", ")
        );
    }

    fn readme_advertised_epsg_codes(readme: &str) -> BTreeSet<u32> {
        let mut in_supported_crs = false;
        let mut codes = BTreeSet::new();

        for line in readme.lines() {
            let trimmed = line.trim();
            if trimmed == "## Supported CRS" {
                in_supported_crs = true;
                continue;
            }
            if in_supported_crs && trimmed.starts_with("## ") {
                break;
            }
            if !in_supported_crs {
                continue;
            }

            for token in trimmed.split(|value: char| {
                value.is_whitespace()
                    || matches!(value, ',' | '|' | '`' | '[' | ']' | '(' | ')' | '{' | '}')
            }) {
                add_readme_epsg_token(token, &mut codes);
            }
        }

        codes
    }

    fn add_readme_epsg_token(token: &str, codes: &mut BTreeSet<u32>) {
        let token = token.trim().trim_matches(|value: char| {
            !value.is_ascii_alphanumeric() && value != ':' && value != '-'
        });
        let token = token
            .strip_prefix("EPSG:")
            .or_else(|| token.strip_prefix("epsg:"))
            .unwrap_or(token);
        if token.is_empty() || token.contains("...") {
            return;
        }

        if let Some((start, end)) = token.split_once('-') {
            let start = start
                .trim()
                .strip_prefix("EPSG:")
                .or_else(|| start.trim().strip_prefix("epsg:"))
                .unwrap_or_else(|| start.trim());
            let end = end
                .trim()
                .strip_prefix("EPSG:")
                .or_else(|| end.trim().strip_prefix("epsg:"))
                .unwrap_or_else(|| end.trim());
            if start.chars().all(|value| value.is_ascii_digit())
                && end.chars().all(|value| value.is_ascii_digit())
            {
                let start = start.parse::<u32>().expect("validated numeric range start");
                let end = end.parse::<u32>().expect("validated numeric range end");
                codes.extend(start..=end);
            }
            return;
        }

        if token.chars().all(|value| value.is_ascii_digit()) {
            codes.insert(token.parse().expect("validated numeric EPSG token"));
        }
    }

    #[test]
    fn nc_state_plane() {
        let crs = lookup_epsg(32119).expect("should find NC State Plane");
        assert!(crs.is_projected());
        assert!(!crs.name().is_empty());
    }

    #[test]
    fn operations_between_returns_forward_compatible_operations() {
        let source = lookup_epsg(4267).expect("should find NAD27");
        let target = lookup_epsg(4326).expect("should find WGS84");
        let operations = operations_between(&source, &target);
        let source_datum = crate::epsg_db::lookup_datum_code_for_crs(4267);
        let target_datum = crate::epsg_db::lookup_datum_code_for_crs(4326);

        assert!(!operations.is_empty());
        assert!(operations.iter().all(|operation| {
            (operation.source_crs_epsg == Some(4267) && operation.target_crs_epsg == Some(4326))
                || (operation.source_datum_epsg == source_datum
                    && operation.target_datum_epsg == target_datum)
        }));
    }

    #[test]
    fn grid_alternative_candidate_preserves_epsg_metadata_and_ranks_first() {
        let source = lookup_epsg(4267).expect("should find NAD27");
        let target = lookup_epsg(4269).expect("should find NAD83");
        let original = lookup_operation(CoordinateOperationId(1313)).expect("operation 1313");
        let alternative =
            lookup_operation(CoordinateOperationId(9_901_313)).expect("operation 9901313");

        assert_eq!(alternative.source_crs_epsg, original.source_crs_epsg);
        assert_eq!(alternative.target_crs_epsg, original.target_crs_epsg);
        assert_eq!(alternative.source_datum_epsg, original.source_datum_epsg);
        assert_eq!(alternative.target_datum_epsg, original.target_datum_epsg);
        assert_eq!(alternative.accuracy, original.accuracy);
        assert_eq!(alternative.areas_of_use, original.areas_of_use);

        let crate::operation::OperationMethod::GridShift { grid_id, .. } = alternative.method
        else {
            panic!("expected grid shift");
        };
        let grid = lookup_grid_definition(grid_id.0).expect("alternative grid");
        assert_eq!(grid.format, crate::grid::GridFormat::GeoTiff);
        assert!(grid
            .resource_names
            .iter()
            .any(|name| name == "ca_nrc_ntv2_0.tif"));

        let candidates = operation_candidates_between(&source, &target).unwrap();
        let original_position = candidates
            .iter()
            .position(|candidate| candidate.id == Some(CoordinateOperationId(1313)))
            .expect("original candidate");
        let alternative_position = candidates
            .iter()
            .position(|candidate| candidate.id == Some(CoordinateOperationId(9_901_313)))
            .expect("alternative candidate");
        assert!(alternative_position < original_position);
    }

    #[test]
    fn operation_candidates_between_reports_direction() {
        let source = lookup_epsg(4326).expect("should find WGS84");
        let target = lookup_epsg(4267).expect("should find NAD27");

        let candidates = operation_candidates_between(&source, &target).unwrap();

        assert!(candidates.iter().any(|candidate| {
            candidate.direction == crate::operation::OperationStepDirection::Reverse
                && candidate.source_crs_epsg == Some(4326)
                && candidate.target_crs_epsg == Some(4267)
        }));
    }

    #[test]
    fn operation_candidate_discovery_includes_custom_operations() {
        let source = lookup_epsg(4267).expect("should find NAD27");
        let target = lookup_epsg(4326).expect("should find WGS84");
        let custom = CoordinateOperation {
            id: None,
            name: "Custom candidate".into(),
            source_crs_epsg: Some(4267),
            target_crs_epsg: Some(4326),
            source_datum_epsg: None,
            target_datum_epsg: None,
            accuracy: None,
            areas_of_use: smallvec::SmallVec::new(),
            deprecated: false,
            preferred: true,
            approximate: false,
            superseded: false,
            method: crate::operation::OperationMethod::Identity,
        };
        let options = SelectionOptions::new().with_coordinate_operation(custom);

        let candidates =
            operation_candidates_between_with_selection_options(&source, &target, &options)
                .unwrap();

        assert_eq!(candidates[0].name, "Custom candidate");
        assert_eq!(
            candidates[0].direction,
            crate::operation::OperationStepDirection::Forward
        );
    }

    #[test]
    fn operation_candidate_discovery_validates_aoi_bounds() {
        let source = lookup_epsg(4267).expect("should find NAD27");
        let target = lookup_epsg(4326).expect("should find WGS84");
        let options = SelectionOptions {
            area_of_interest: Some(crate::operation::AreaOfInterest::geographic_bounds(
                crate::coord::Bounds::new(10.0, 0.0, -10.0, 1.0),
            )),
            ..SelectionOptions::default()
        };

        let err = operation_candidates_between_with_selection_options(&source, &target, &options)
            .unwrap_err();

        assert!(matches!(err, Error::OutOfRange(_)));
    }

    #[test]
    fn operation_candidate_discovery_accepts_wrapped_geographic_aoi_bounds() {
        let source = lookup_epsg(4267).expect("should find NAD27");
        let target = lookup_epsg(4326).expect("should find WGS84");
        let options = SelectionOptions {
            area_of_interest: Some(crate::operation::AreaOfInterest::geographic_wrapped_bounds(
                crate::coord::Bounds::new(170.0, -20.0, -170.0, -10.0),
            )),
            ..SelectionOptions::default()
        };

        operation_candidates_between_with_selection_options(&source, &target, &options).unwrap();
    }
}
