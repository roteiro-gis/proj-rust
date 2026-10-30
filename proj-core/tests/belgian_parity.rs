//! Belgian datum and height selection against C PROJ 9.8.1 (`cs2cs`) at
//! lon 3.12, lat 50.95, h 50.
//!
//! C PROJ takes the 15929 Helmert for a 2D pair with BD72, even with the grids
//! installed, and routes a compound through the ETRS89 of its geoid grid, which
//! lands on the BD72 to ETRS89 (3) grid.
//!
//! Horizontal tolerances are 1 mm. Reversed Helmerts sit up to 0.3 mm from C
//! PROJ: `helmert_inverse` solves the exact inverse, while C PROJ transposes
//! its small-angle rotation matrix. The wrong-sign deprecated scale is 5 cm off.

#[cfg(feature = "geotiff")]
#[path = "common/cdn_grids.rs"]
mod cdn_grids;

use proj_core::{
    registry, AreaOfInterest, Coord, CoordinateOperationId, SelectionOptions, Transform,
};

const INPUT_LON: f64 = 3.12;
const INPUT_LAT: f64 = 50.95;

const METRES: f64 = 1e-3;
const DEGREES: f64 = 1e-8;

// `echo "50.95 3.12" | cs2cs -f %.10f EPSG:4326 EPSG:4313`
const PROJ_BD72: (f64, f64) = (3.1187682820, 50.9505346220);
// `echo "50.95 3.12" | cs2cs -f %.4f EPSG:4326 EPSG:31370`
const PROJ_LB72: (f64, f64) = (62252.8708, 182927.9605);

const CURRENT_HELMERT: CoordinateOperationId = CoordinateOperationId(15929);

fn assert_near_2d(actual: (f64, f64), expected: (f64, f64), tolerance: f64) {
    assert!(
        (actual.0 - expected.0).abs() < tolerance && (actual.1 - expected.1).abs() < tolerance,
        "{actual:?} vs C PROJ {expected:?}"
    );
}

fn point_area() -> AreaOfInterest {
    AreaOfInterest::geographic_point(Coord::new(INPUT_LON, INPUT_LAT))
}

#[test]
fn wgs84_to_bd72_uses_the_current_helmert() {
    for options in [
        SelectionOptions::new(),
        SelectionOptions::new().with_area_of_interest(point_area()),
    ] {
        let transform =
            Transform::with_selection_options("EPSG:4326", "EPSG:4313", options).unwrap();
        assert_eq!(transform.selected_operation().id, Some(CURRENT_HELMERT));
        assert!(!transform.selected_operation().deprecated);
        assert_near_2d(
            transform.convert((INPUT_LON, INPUT_LAT)).unwrap(),
            PROJ_BD72,
            DEGREES,
        );
    }
}

#[test]
fn wgs84_to_belgian_lambert_72_uses_the_current_helmert() {
    let transform = Transform::new("EPSG:4326", "EPSG:31370").unwrap();
    assert_eq!(transform.selected_operation().id, Some(CURRENT_HELMERT));
    assert_near_2d(
        transform.convert((INPUT_LON, INPUT_LAT)).unwrap(),
        PROJ_LB72,
        METRES,
    );
}

#[test]
fn wgs84_3d_and_ostend_compound_have_a_vertical_operation_both_ways() {
    let wgs84 = registry::lookup_epsg(4979).unwrap();
    let lambert_2008 = registry::lookup_epsg(8370).unwrap();

    for (source, target) in [(&wgs84, &lambert_2008), (&lambert_2008, &wgs84)] {
        let operations = registry::vertical_grid_operations_between(source, target);
        assert!(operations
            .iter()
            .any(|operation| operation.name.contains("ETRS89 to Ostend height (1)")));
    }
}

#[cfg(feature = "geotiff")]
mod with_grids {
    use super::*;
    use std::sync::Arc;

    use proj_core::{GridDefinition, GridError, GridHandle, GridProvider};

    use crate::cdn_grids::{self, PinnedGrid};

    const INPUT_H: f64 = 50.0;

    const BD72_GRID: &str = "be_ign_bd72lb72_etrs89lb08.tif";
    const GEOID_GRID: &str = "be_ign_hBG18.tif";
    const GRIDS: &[PinnedGrid] = &[
        (
            BD72_GRID,
            "sha256:eb5cb1823e66971189dfd20c65f3a2c507d600403772e9cc01c04f88d7e68cfa",
        ),
        (
            GEOID_GRID,
            "sha256:882340af95eaf5dbd46f7988092fbb84cde8dcbb712ce9764c667e64ead6aa4c",
        ),
    ];

    // `echo "50.95 3.12 50" | cs2cs -f %.4f EPSG:4979 EPSG:6190`
    const PROJ_LB72_OSTEND: (f64, f64, f64) = (62252.8331, 182928.1135, 7.9573);
    // The same with only be_ign_hBG18.tif installed.
    const PROJ_LB72_OSTEND_NO_BD72_GRID: (f64, f64, f64) = (62252.8708, 182927.9603, 7.9573);
    // `echo "50.95 3.12 50" | cs2cs -f %.4f EPSG:4979 EPSG:8370`
    const PROJ_LB2008_OSTEND: (f64, f64, f64) = (562251.7176, 682917.8125, 7.9573);
    // `cs2cs -f %.10f` from the two compound results above to EPSG:4979. The
    // heights differ from 50 because those inputs are rounded to 0.1 mm.
    const PROJ_WGS84_FROM_LB72: (f64, f64, f64) = (3.1200000005, 50.9499999999, 49.9999661173);
    const PROJ_WGS84_FROM_LB2008: (f64, f64, f64) = (3.1199999995, 50.9500000003, 49.9999661172);

    fn grid_provider() -> Option<Arc<dyn GridProvider>> {
        cdn_grids::grid_provider("PROJ_BE_GRID_DIR", GRIDS)
    }

    /// Hides every grid except the geoid.
    struct GeoidOnly(Arc<dyn GridProvider>);

    impl GeoidOnly {
        fn is_geoid(grid: &GridDefinition) -> bool {
            grid.name == GEOID_GRID || grid.resource_names.iter().any(|name| name == GEOID_GRID)
        }
    }

    impl GridProvider for GeoidOnly {
        fn definition(&self, grid: &GridDefinition) -> Result<Option<GridDefinition>, GridError> {
            if Self::is_geoid(grid) {
                self.0.definition(grid)
            } else {
                Ok(None)
            }
        }

        fn load(&self, grid: &GridDefinition) -> Result<Option<GridHandle>, GridError> {
            if Self::is_geoid(grid) {
                self.0.load(grid)
            } else {
                Ok(None)
            }
        }
    }

    fn transform(from: &str, to: &str, provider: Arc<dyn GridProvider>) -> Transform {
        let options = SelectionOptions::new()
            .with_area_of_interest(point_area())
            .with_grid_provider(provider);
        Transform::with_selection_options(from, to, options)
            .unwrap_or_else(|error| panic!("{from} -> {to}: {error}"))
    }

    fn assert_near_3d(actual: (f64, f64, f64), expected: (f64, f64, f64), horizontal: f64) {
        assert!(
            (actual.0 - expected.0).abs() < horizontal
                && (actual.1 - expected.1).abs() < horizontal
                && (actual.2 - expected.2).abs() < METRES,
            "{actual:?} vs C PROJ {expected:?}"
        );
    }

    #[test]
    fn two_dimensional_pairs_keep_the_helmert_with_grids_installed() {
        let Some(provider) = grid_provider() else {
            eprintln!("skipping: set PROJ_BE_GRID_DIR or PROJ_NETWORK=ON");
            return;
        };

        let bd72 = transform("EPSG:4326", "EPSG:4313", provider.clone());
        assert_eq!(bd72.selected_operation().id, Some(CURRENT_HELMERT));
        assert_near_2d(
            bd72.convert((INPUT_LON, INPUT_LAT)).unwrap(),
            PROJ_BD72,
            DEGREES,
        );

        let lambert = transform("EPSG:4326", "EPSG:31370", provider);
        assert_eq!(lambert.selected_operation().id, Some(CURRENT_HELMERT));
        assert_near_2d(
            lambert.convert((INPUT_LON, INPUT_LAT)).unwrap(),
            PROJ_LB72,
            METRES,
        );
    }

    #[test]
    fn ostend_compound_routes_through_the_etrs89_of_its_geoid() {
        let Some(provider) = grid_provider() else {
            eprintln!("skipping: set PROJ_BE_GRID_DIR or PROJ_NETWORK=ON");
            return;
        };

        let forward = transform("EPSG:4979", "EPSG:6190", provider.clone());
        assert!(forward.selected_operation().uses_grids);
        assert_near_3d(
            forward.convert_3d((INPUT_LON, INPUT_LAT, INPUT_H)).unwrap(),
            PROJ_LB72_OSTEND,
            METRES,
        );

        let reverse = transform("EPSG:6190", "EPSG:4979", provider);
        assert!(reverse.selected_operation().uses_grids);
        assert_near_3d(
            reverse.convert_3d(PROJ_LB72_OSTEND).unwrap(),
            PROJ_WGS84_FROM_LB72,
            DEGREES,
        );
    }

    #[test]
    fn ostend_compound_falls_back_to_the_helmert_without_the_bd72_grid() {
        let Some(provider) = grid_provider() else {
            eprintln!("skipping: set PROJ_BE_GRID_DIR or PROJ_NETWORK=ON");
            return;
        };

        let forward = transform("EPSG:4979", "EPSG:6190", Arc::new(GeoidOnly(provider)));
        assert_eq!(forward.selected_operation().id, Some(CURRENT_HELMERT));
        assert_near_3d(
            forward.convert_3d((INPUT_LON, INPUT_LAT, INPUT_H)).unwrap(),
            PROJ_LB72_OSTEND_NO_BD72_GRID,
            METRES,
        );
    }

    #[test]
    fn wgs84_3d_reaches_the_ostend_compound_on_etrs89() {
        let Some(provider) = grid_provider() else {
            eprintln!("skipping: set PROJ_BE_GRID_DIR or PROJ_NETWORK=ON");
            return;
        };

        let forward = transform("EPSG:4979", "EPSG:8370", provider.clone());
        assert_near_3d(
            forward.convert_3d((INPUT_LON, INPUT_LAT, INPUT_H)).unwrap(),
            PROJ_LB2008_OSTEND,
            METRES,
        );

        let reverse = transform("EPSG:8370", "EPSG:4979", provider.clone());
        assert_near_3d(
            reverse.convert_3d(PROJ_LB2008_OSTEND).unwrap(),
            PROJ_WGS84_FROM_LB2008,
            DEGREES,
        );

        let etrs89 = transform("EPSG:4937", "EPSG:8370", provider);
        assert_near_3d(
            etrs89.convert_3d((INPUT_LON, INPUT_LAT, INPUT_H)).unwrap(),
            PROJ_LB2008_OSTEND,
            METRES,
        );
    }
}
