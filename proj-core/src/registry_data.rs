use crate::crs::*;
use crate::datum;

// ---------------------------------------------------------------------------
// Geographic CRS definitions
// ---------------------------------------------------------------------------

pub(crate) const GEOGRAPHIC_CRS: &[(u32, GeographicCrsDef)] = &[
    (
        4267,
        GeographicCrsDef {
            epsg: 4267,
            datum: datum::NAD27,
            name: "NAD27",
        },
    ),
    (
        4269,
        GeographicCrsDef {
            epsg: 4269,
            datum: datum::NAD83,
            name: "NAD83",
        },
    ),
    (
        4326,
        GeographicCrsDef {
            epsg: 4326,
            datum: datum::WGS84,
            name: "WGS 84",
        },
    ),
    (
        4258,
        GeographicCrsDef {
            epsg: 4258,
            datum: datum::ETRS89,
            name: "ETRS89",
        },
    ),
    (
        4277,
        GeographicCrsDef {
            epsg: 4277,
            datum: datum::OSGB36,
            name: "OSGB 1936",
        },
    ),
    (
        4284,
        GeographicCrsDef {
            epsg: 4284,
            datum: datum::PULKOVO1942,
            name: "Pulkovo 1942",
        },
    ),
    (
        4230,
        GeographicCrsDef {
            epsg: 4230,
            datum: datum::ED50,
            name: "ED50",
        },
    ),
    (
        4301,
        GeographicCrsDef {
            epsg: 4301,
            datum: datum::TOKYO,
            name: "Tokyo",
        },
    ),
];

// ---------------------------------------------------------------------------
// Projected CRS definitions — manually curated
// ---------------------------------------------------------------------------

pub(crate) const PROJECTED_CRS: &[(u32, ProjectedCrsDef)] = &[
    // Web Mercator
    (
        3857,
        ProjectedCrsDef {
            epsg: 3857,
            datum: datum::WGS84,
            method: ProjectionMethod::WebMercator,
            name: "WGS 84 / Pseudo-Mercator",
        },
    ),
    // NSIDC Sea Ice Polar Stereographic North
    (
        3413,
        ProjectedCrsDef {
            epsg: 3413,
            datum: datum::WGS84,
            method: ProjectionMethod::PolarStereographic {
                lon0: -45.0,
                lat_ts: 70.0,
                k0: 1.0,
                false_easting: 0.0,
                false_northing: 0.0,
            },
            name: "WGS 84 / NSIDC Sea Ice Polar Stereographic North",
        },
    ),
    // Antarctic Polar Stereographic
    (
        3031,
        ProjectedCrsDef {
            epsg: 3031,
            datum: datum::WGS84,
            method: ProjectionMethod::PolarStereographic {
                lon0: 0.0,
                lat_ts: -71.0,
                k0: 1.0,
                false_easting: 0.0,
                false_northing: 0.0,
            },
            name: "WGS 84 / Antarctic Polar Stereographic",
        },
    ),
    // Arctic Polar Stereographic
    (
        3995,
        ProjectedCrsDef {
            epsg: 3995,
            datum: datum::WGS84,
            method: ProjectionMethod::PolarStereographic {
                lon0: 0.0,
                lat_ts: 71.0,
                k0: 1.0,
                false_easting: 0.0,
                false_northing: 0.0,
            },
            name: "WGS 84 / Arctic Polar Stereographic",
        },
    ),
    // UPS North
    (
        32661,
        ProjectedCrsDef {
            epsg: 32661,
            datum: datum::WGS84,
            method: ProjectionMethod::PolarStereographic {
                lon0: 0.0,
                lat_ts: 90.0,
                k0: 0.994,
                false_easting: 2_000_000.0,
                false_northing: 2_000_000.0,
            },
            name: "WGS 84 / UPS North",
        },
    ),
    // UPS South
    (
        32761,
        ProjectedCrsDef {
            epsg: 32761,
            datum: datum::WGS84,
            method: ProjectionMethod::PolarStereographic {
                lon0: 0.0,
                lat_ts: -90.0,
                k0: 0.994,
                false_easting: 2_000_000.0,
                false_northing: 2_000_000.0,
            },
            name: "WGS 84 / UPS South",
        },
    ),
    // --- World Mercator ---
    (
        3395,
        ProjectedCrsDef {
            epsg: 3395,
            datum: datum::WGS84,
            method: ProjectionMethod::Mercator {
                lon0: 0.0,
                lat_ts: 0.0,
                k0: 1.0,
                false_easting: 0.0,
                false_northing: 0.0,
            },
            name: "WGS 84 / World Mercator",
        },
    ),
    // --- CONUS Albers Equal Area ---
    (
        5070,
        ProjectedCrsDef {
            epsg: 5070,
            datum: datum::NAD83,
            method: ProjectionMethod::AlbersEqualArea {
                lon0: -96.0,
                lat0: 23.0,
                lat1: 29.5,
                lat2: 45.5,
                false_easting: 0.0,
                false_northing: 0.0,
            },
            name: "NAD83 / Conus Albers",
        },
    ),
    // --- France Lambert-93 ---
    (
        2154,
        ProjectedCrsDef {
            epsg: 2154,
            datum: datum::ETRS89,
            method: ProjectionMethod::LambertConformalConic {
                lon0: 3.0,
                lat0: 46.5,
                lat1: 44.0,
                lat2: 49.0,
                false_easting: 700_000.0,
                false_northing: 6_600_000.0,
            },
            name: "RGF93 v1 / Lambert-93",
        },
    ),
    // --- British National Grid ---
    (
        27700,
        ProjectedCrsDef {
            epsg: 27700,
            datum: datum::OSGB36,
            method: ProjectionMethod::TransverseMercator {
                lon0: -2.0,
                lat0: 49.0,
                k0: 0.9996012717,
                false_easting: 400_000.0,
                false_northing: -100_000.0,
            },
            name: "OSGB 1936 / British National Grid",
        },
    ),
    // --- Plate Carree ---
    (
        32662,
        ProjectedCrsDef {
            epsg: 32662,
            datum: datum::WGS84,
            method: ProjectionMethod::EquidistantCylindrical {
                lon0: 0.0,
                lat_ts: 0.0,
                false_easting: 0.0,
                false_northing: 0.0,
            },
            name: "WGS 84 / Plate Carree",
        },
    ),
    // --- BC Albers ---
    (
        3005,
        ProjectedCrsDef {
            epsg: 3005,
            datum: datum::NAD83,
            method: ProjectionMethod::AlbersEqualArea {
                lon0: -126.0,
                lat0: 45.0,
                lat1: 50.0,
                lat2: 58.5,
                false_easting: 1_000_000.0,
                false_northing: 0.0,
            },
            name: "NAD83 / BC Albers",
        },
    ),
    // --- Canada Lambert ---
    (
        3347,
        ProjectedCrsDef {
            epsg: 3347,
            datum: datum::NAD83,
            method: ProjectionMethod::LambertConformalConic {
                lon0: -91.866667,
                lat0: 63.390675,
                lat1: 49.0,
                lat2: 77.0,
                false_easting: 6_200_000.0,
                false_northing: 3_000_000.0,
            },
            name: "NAD83 / Statistics Canada Lambert",
        },
    ),
];

// ---------------------------------------------------------------------------
// UTM zone generation
// ---------------------------------------------------------------------------

/// Generate a UTM zone definition.
const fn utm_def(zone: u8, north: bool) -> ProjectedCrsDef {
    let epsg = if north {
        32600 + zone as u32
    } else {
        32700 + zone as u32
    };
    let lon0 = (zone as f64 - 1.0) * 6.0 - 180.0 + 3.0;
    let false_northing = if north { 0.0 } else { 10_000_000.0 };

    ProjectedCrsDef {
        epsg,
        datum: datum::WGS84,
        method: ProjectionMethod::TransverseMercator {
            lon0,
            lat0: 0.0,
            k0: 0.9996,
            false_easting: 500_000.0,
            false_northing,
        },
        // name will be set via the lookup function since we can't format in const
        name: "", // filled in by registry lookup
    }
}

/// Look up a UTM zone by EPSG code. Returns None if not a UTM code.
pub(crate) fn lookup_utm(epsg: u32) -> Option<ProjectedCrsDef> {
    if (32601..=32660).contains(&epsg) {
        let zone = (epsg - 32600) as u8;
        let mut def = utm_def(zone, true);
        def.name = ""; // no static name for generated zones
        Some(def)
    } else if (32701..=32760).contains(&epsg) {
        let zone = (epsg - 32700) as u8;
        let mut def = utm_def(zone, false);
        def.name = "";
        Some(def)
    } else {
        None
    }
}
