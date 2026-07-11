use proj::Proj;
use serde::Serialize;

#[derive(Serialize)]
struct ReferencePoint {
    from_epsg: u32,
    to_epsg: u32,
    input_x: f64,
    input_y: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    input_z: Option<f64>,
    expected_x: f64,
    expected_y: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    expected_z: Option<f64>,
    tolerance: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    tolerance_z: Option<f64>,
    description: String,
    /// When set, proj-core is known to diverge from this reference value until
    /// the named fix lands; the default corpus test skips the point and the
    /// ignored `corpus_pending_fixes_resolved` test tracks it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pending_fix: Option<String>,
}

fn transform(from: u32, to: u32, x: f64, y: f64, tol: f64, desc: &str) -> Option<ReferencePoint> {
    let label = if desc.is_empty() {
        format!("EPSG:{from}->{to} ({x}, {y})")
    } else {
        desc.to_string()
    };
    let from_crs = format!("EPSG:{from}");
    let to_crs = format!("EPSG:{to}");
    let proj = match Proj::new_known_crs(&from_crs, &to_crs, None) {
        Ok(proj) => proj,
        Err(err) => {
            eprintln!("Skipping reference point {label}: failed to create transform: {err}");
            return None;
        }
    };
    let (ox, oy) = match proj.convert((x, y)) {
        Ok(coord) => coord,
        Err(err) => {
            eprintln!("Skipping reference point {label}: transform failed: {err}");
            return None;
        }
    };
    if !ox.is_finite() || !oy.is_finite() {
        eprintln!("Skipping reference point {label}: non-finite output ({ox}, {oy})");
        return None;
    }

    Some(ReferencePoint {
        from_epsg: from,
        to_epsg: to,
        input_x: x,
        input_y: y,
        input_z: None,
        expected_x: ox,
        expected_y: oy,
        expected_z: None,
        tolerance: tol,
        tolerance_z: None,
        description: desc.to_string(),
        pending_fix: None,
    })
}

/// Transform through the 3D promotions of both CRSs (`proj_crs_promote_to_3D`),
/// so datum-shift-induced ellipsoidal height changes appear in the reference
/// values instead of C PROJ's 2D `push/pop v_3` height passthrough.
mod promoted_3d {
    use std::ffi::CString;
    use std::ptr;

    use proj_sys::{
        proj_area_create, proj_area_destroy, proj_context_create, proj_context_destroy,
        proj_context_errno, proj_create, proj_create_crs_to_crs_from_pj, proj_crs_promote_to_3D,
        proj_destroy, proj_errno, proj_errno_string, proj_normalize_for_visualization, proj_trans,
        PJ_CONTEXT, PJ_COORD, PJ_DIRECTION_PJ_FWD, PJ_XYZT,
    };

    fn error_message(err: i32) -> String {
        unsafe {
            let ptr = proj_errno_string(err);
            if ptr.is_null() {
                return format!("PROJ error code {err}");
            }
            std::ffi::CStr::from_ptr(ptr).to_string_lossy().into_owned()
        }
    }

    unsafe fn create_promoted_crs(
        ctx: *mut PJ_CONTEXT,
        code: u32,
    ) -> Result<*mut proj_sys::PJ, String> {
        let def = CString::new(format!("EPSG:{code}")).expect("EPSG code strings have no NUL");
        let crs = proj_create(ctx, def.as_ptr());
        if crs.is_null() {
            return Err(format!(
                "failed to create CRS EPSG:{code}: {}",
                error_message(proj_context_errno(ctx))
            ));
        }
        let crs_3d = proj_crs_promote_to_3D(ctx, ptr::null(), crs);
        proj_destroy(crs);
        if crs_3d.is_null() {
            return Err(format!(
                "failed to promote CRS EPSG:{code} to 3D: {}",
                error_message(proj_context_errno(ctx))
            ));
        }
        Ok(crs_3d)
    }

    pub fn convert(from: u32, to: u32, coord: (f64, f64, f64)) -> Result<(f64, f64, f64), String> {
        unsafe {
            let ctx = proj_context_create();
            if ctx.is_null() {
                return Err("failed to create PROJ context".into());
            }
            let result = convert_in_ctx(ctx, from, to, coord);
            proj_context_destroy(ctx);
            result
        }
    }

    unsafe fn convert_in_ctx(
        ctx: *mut PJ_CONTEXT,
        from: u32,
        to: u32,
        coord: (f64, f64, f64),
    ) -> Result<(f64, f64, f64), String> {
        let from_crs = create_promoted_crs(ctx, from)?;
        let to_crs = match create_promoted_crs(ctx, to) {
            Ok(crs) => crs,
            Err(err) => {
                proj_destroy(from_crs);
                return Err(err);
            }
        };

        let area = proj_area_create();
        let raw = proj_create_crs_to_crs_from_pj(ctx, from_crs, to_crs, area, ptr::null());
        proj_area_destroy(area);
        proj_destroy(from_crs);
        proj_destroy(to_crs);
        if raw.is_null() {
            return Err(format!(
                "failed to create promoted 3D transform EPSG:{from}->EPSG:{to}: {}",
                error_message(proj_context_errno(ctx))
            ));
        }

        let pj = proj_normalize_for_visualization(ctx, raw);
        proj_destroy(raw);
        if pj.is_null() {
            return Err(format!(
                "failed to normalize promoted 3D transform EPSG:{from}->EPSG:{to}: {}",
                error_message(proj_context_errno(ctx))
            ));
        }

        let trans = proj_trans(
            pj,
            PJ_DIRECTION_PJ_FWD,
            PJ_COORD {
                xyzt: PJ_XYZT {
                    x: coord.0,
                    y: coord.1,
                    z: coord.2,
                    t: f64::INFINITY,
                },
            },
        );
        let err = proj_errno(pj);
        proj_destroy(pj);
        if err != 0 {
            return Err(format!(
                "promoted 3D convert failed: {}",
                error_message(err)
            ));
        }
        Ok((trans.xyzt.x, trans.xyzt.y, trans.xyzt.z))
    }
}

/// Generate a 3D reference point through promoted 3D CRSs.
#[allow(clippy::too_many_arguments)]
fn transform_3d(
    from: u32,
    to: u32,
    x: f64,
    y: f64,
    z: f64,
    tol: f64,
    tol_z: f64,
    desc: &str,
    pending: Option<&str>,
) -> Option<ReferencePoint> {
    let (ox, oy, oz) = match promoted_3d::convert(from, to, (x, y, z)) {
        Ok(out) => out,
        Err(err) => {
            eprintln!("Skipping 3D reference point {desc}: {err}");
            return None;
        }
    };
    if !ox.is_finite() || !oy.is_finite() || !oz.is_finite() {
        eprintln!("Skipping 3D reference point {desc}: non-finite output ({ox}, {oy}, {oz})");
        return None;
    }

    Some(ReferencePoint {
        from_epsg: from,
        to_epsg: to,
        input_x: x,
        input_y: y,
        input_z: Some(z),
        expected_x: ox,
        expected_y: oy,
        expected_z: Some(oz),
        tolerance: tol,
        tolerance_z: Some(tol_z),
        description: desc.to_string(),
        pending_fix: pending.map(str::to_string),
    })
}

/// Geographic test points: (lon, lat, name)
const GEO_POINTS: &[(f64, f64, &str)] = &[
    (0.0, 0.0, "origin"),
    (-74.0445, 40.6892, "NYC"),
    (139.6917, 35.6895, "Tokyo"),
    (-0.1278, 51.5074, "London"),
    (2.3522, 48.8566, "Paris"),
    (-43.1729, -22.9068, "Rio"),
    (151.2093, -33.8688, "Sydney"),
    (37.6173, 55.7558, "Moscow"),
    (-96.0, 37.0, "US center"),
    (180.0, 0.0, "dateline equator"),
    (-180.0, 0.0, "antimeridian equator"),
    (0.0, 85.0, "near north pole"),
    (0.0, -85.0, "near south pole"),
    (90.0, 0.0, "90E equator"),
    (-90.0, 0.0, "90W equator"),
    (0.0, 45.0, "45N prime meridian"),
    (0.0, -45.0, "45S prime meridian"),
    (179.9, 0.0, "near dateline"),
    (-179.9, 0.0, "near antimeridian"),
    (0.0, 0.01, "just north of equator"),
    (0.0, -0.01, "just south of equator"),
];

/// Arctic test points for polar stereographic north
const ARCTIC_POINTS: &[(f64, f64, &str)] = &[
    (-45.0, 90.0, "north pole"),
    (-45.0, 85.0, "85N on CM"),
    (-45.0, 75.0, "75N on CM"),
    (-45.0, 65.0, "65N on CM"),
    (0.0, 80.0, "80N 0E"),
    (90.0, 80.0, "80N 90E"),
    (-90.0, 80.0, "80N 90W"),
    (135.0, 70.0, "70N 135E"),
    (-135.0, 70.0, "70N 135W"),
];

/// Antarctic test points
const ANTARCTIC_POINTS: &[(f64, f64, &str)] = &[
    (0.0, -90.0, "south pole"),
    (0.0, -85.0, "85S 0E"),
    (0.0, -75.0, "75S 0E"),
    (90.0, -80.0, "80S 90E"),
    (-90.0, -80.0, "80S 90W"),
];

fn main() {
    let mut points: Vec<ReferencePoint> = Vec::new();

    // =========================================================================
    // 1. FORWARD: WGS84 (4326) → every projected CRS in our registry
    // =========================================================================

    // Web Mercator (3857) — all geo points within ±85° lat
    for &(lon, lat, name) in GEO_POINTS {
        if lat.abs() <= 85.0 {
            points.extend(transform(
                4326,
                3857,
                lon,
                lat,
                0.001,
                &format!("{name} 4326→3857"),
            ));
        }
    }

    // World Mercator (3395)
    for &(lon, lat, name) in GEO_POINTS {
        if lat.abs() <= 80.0 {
            points.extend(transform(
                4326,
                3395,
                lon,
                lat,
                0.01,
                &format!("{name} 4326→3395"),
            ));
        }
    }

    // UTM zones — sample 12 zones across the globe
    let utm_samples: &[(u8, bool, f64, f64, &str)] = &[
        (1, true, -177.0, 10.0, "zone1N"),
        (10, true, -123.0, 37.0, "zone10N SF"),
        (18, true, -74.006, 40.7128, "zone18N NYC"),
        (30, true, -0.1278, 51.5074, "zone30N London"),
        (31, true, 2.3522, 48.8566, "zone31N Paris"),
        (36, true, 37.6173, 55.7558, "zone36N Moscow"),
        (54, true, 139.6917, 35.6895, "zone54N Tokyo"),
        (60, true, 179.0, 10.0, "zone60N"),
        (18, false, -74.0, -10.0, "zone18S"),
        (23, false, -43.1729, -22.9068, "zone23S Rio"),
        (56, false, 151.2093, -33.8688, "zone56S Sydney"),
        (1, false, -177.0, -10.0, "zone1S"),
    ];
    for &(zone, north, lon, lat, name) in utm_samples {
        let epsg = if north {
            32600 + zone as u32
        } else {
            32700 + zone as u32
        };
        points.extend(transform(
            4326,
            epsg,
            lon,
            lat,
            0.01,
            &format!("{name} 4326→UTM{epsg}"),
        ));
        // Also test inverse
        if let Some(fwd) = transform(4326, epsg, lon, lat, 0.01, "") {
            points.extend(transform(
                epsg,
                4326,
                fwd.expected_x,
                fwd.expected_y,
                2e-7,
                &format!("{name} UTM{epsg}→4326 inverse"),
            ));
        }
    }

    // Polar Stereographic North (3413)
    for &(lon, lat, name) in ARCTIC_POINTS {
        points.extend(transform(
            4326,
            3413,
            lon,
            lat,
            0.01,
            &format!("{name} 4326→3413"),
        ));
    }

    // Antarctic Polar Stereographic (3031)
    for &(lon, lat, name) in ANTARCTIC_POINTS {
        points.extend(transform(
            4326,
            3031,
            lon,
            lat,
            0.1,
            &format!("{name} 4326→3031"),
        ));
    }

    // Arctic Polar Stereographic (3995)
    points.extend(transform(4326, 3995, 0.0, 80.0, 0.01, "80N 0E 4326→3995"));
    points.extend(transform(4326, 3995, 90.0, 75.0, 0.01, "75N 90E 4326→3995"));

    // France Lambert-93 (2154)
    points.extend(transform(
        4326,
        2154,
        2.3522,
        48.8566,
        0.1,
        "Paris 4326→2154",
    ));
    points.extend(transform(
        4326,
        2154,
        -1.6778,
        48.1173,
        0.1,
        "Rennes 4326→2154",
    ));
    points.extend(transform(
        4326,
        2154,
        7.2620,
        43.7102,
        0.1,
        "Nice 4326→2154",
    ));

    // CONUS Albers (5070)
    points.extend(transform(
        4326,
        5070,
        -96.0,
        37.0,
        0.1,
        "US center 4326→5070",
    ));
    points.extend(transform(4326, 5070, -74.0, 40.7, 0.1, "NYC 4326→5070"));
    points.extend(transform(4326, 5070, -122.4, 37.8, 0.1, "SF 4326→5070"));

    // BC Albers (3005)
    points.extend(transform(
        4326,
        3005,
        -123.1,
        49.3,
        0.1,
        "Vancouver 4326→3005",
    ));

    // Canada Lambert (3347)
    points.extend(transform(4326, 3347, -75.7, 45.4, 0.1, "Ottawa 4326→3347"));

    // Lambert Azimuthal Equal Area (3035, 6931, 6932)
    points.extend(transform(
        4258,
        3035,
        5.0,
        50.0,
        0.05,
        "Europe 4258→3035 LAEA",
    ));
    points.extend(transform(
        4258,
        3035,
        10.0,
        52.0,
        0.05,
        "origin 4258→3035 LAEA",
    ));
    points.extend(transform(
        4326,
        6931,
        -150.0,
        70.0,
        0.05,
        "Alaska 4326→6931 LAEA north",
    ));
    points.extend(transform(
        4326,
        6932,
        45.0,
        -75.0,
        0.05,
        "Antarctic 4326→6932 LAEA south",
    ));
    points.extend(transform(
        10346,
        3408,
        -150.0,
        70.0,
        0.05,
        "Alaska 10346→3408 spherical LAEA north",
    ));
    points.extend(transform(
        10346,
        3409,
        45.0,
        -75.0,
        0.05,
        "Antarctic 10346→3409 spherical LAEA south",
    ));
    points.extend(transform(
        4267,
        9311,
        -96.0,
        37.0,
        0.05,
        "US center 4267→9311 spherical LAEA",
    ));

    // Oblique Stereographic / Double Stereographic (28992)
    points.extend(transform(
        4289,
        28992,
        5.0,
        53.0,
        0.02,
        "RD example 4289→28992",
    ));
    points.extend(transform(
        4289,
        28992,
        4.9,
        52.37,
        0.02,
        "Amsterdam 4289→28992",
    ));

    // Hotine Oblique Mercator variants A and B
    points.extend(transform(
        4269,
        3078,
        -86.0,
        45.3,
        0.05,
        "Michigan 4269→3078 Hotine A",
    ));
    points.extend(transform(
        4269,
        3078,
        -83.0,
        42.3,
        0.05,
        "Detroit 4269→3078 Hotine A",
    ));
    points.extend(transform(
        4150,
        2056,
        7.4386,
        46.9511,
        0.05,
        "Bern 4150→2056 Hotine B",
    ));
    points.extend(transform(
        4150,
        2056,
        8.5417,
        47.3769,
        0.05,
        "Zurich 4150→2056 Hotine B",
    ));

    // Cassini-Soldner (30200, 3377)
    points.extend(transform(
        4302,
        30200,
        -62.0,
        10.0,
        0.1,
        "Trinidad 4302→30200 Cassini",
    ));
    points.extend(transform(
        4742,
        3377,
        103.75,
        1.5,
        0.1,
        "Johor 4742→3377 Cassini",
    ));

    // British National Grid (27700) — requires datum shift OSGB36
    points.extend(transform(
        4326,
        27700,
        -0.1278,
        51.5074,
        1.0,
        "London 4326→27700",
    ));

    // Plate Carree (32662)
    for &(lon, lat, name) in GEO_POINTS {
        points.extend(transform(
            4326,
            32662,
            lon,
            lat,
            0.01,
            &format!("{name} 4326→32662"),
        ));
    }

    // =========================================================================
    // 2. INVERSE: projected → WGS84 for key CRS
    // =========================================================================

    // 3857 → 4326 inverse
    points.extend(transform(
        3857,
        4326,
        -8242596.0,
        4966606.0,
        1e-8,
        "NYC 3857→4326",
    ));
    points.extend(transform(3857, 4326, 0.0, 0.0, 1e-8, "origin 3857→4326"));
    points.extend(transform(
        3857,
        4326,
        15550408.0,
        4257980.0,
        1e-8,
        "Tokyo 3857→4326",
    ));

    // 3413 → 4326 inverse
    points.extend(transform(
        3413,
        4326,
        0.0,
        0.0,
        1e-6,
        "north pole 3413→4326",
    ));
    points.extend(transform(
        3413,
        4326,
        0.0,
        -1633879.0,
        1e-6,
        "75N on CM 3413→4326",
    ));

    // =========================================================================
    // 3. DATUM SHIFTS: cross-datum geographic transforms
    // =========================================================================

    // NAD27 → WGS84
    let nad27_points: &[(f64, f64, &str)] = &[
        (-90.0, 45.0, "US Midwest"),
        (-74.0, 40.7, "NYC area"),
        (-122.4, 37.8, "SF area"),
        (-96.0, 32.0, "Dallas area"),
        (-80.0, 25.8, "Miami area"),
        (-105.0, 40.0, "Denver area"),
    ];
    for &(lon, lat, name) in nad27_points {
        points.extend(transform(
            4267,
            4326,
            lon,
            lat,
            0.001,
            &format!("{name} NAD27→WGS84"),
        ));
    }

    // WGS84 → NAD27 (inverse datum shift)
    points.extend(transform(
        4326,
        4267,
        -90.0,
        45.0,
        0.001,
        "US Midwest WGS84→NAD27",
    ));

    // OSGB36 → WGS84
    points.extend(transform(
        4277,
        4326,
        -0.1278,
        51.5074,
        0.001,
        "London OSGB36→WGS84",
    ));
    points.extend(transform(
        4277,
        4326,
        -3.1883,
        55.9533,
        0.001,
        "Edinburgh OSGB36→WGS84",
    ));
    points.extend(transform(
        4277,
        4326,
        -1.8904,
        52.4862,
        0.001,
        "Birmingham OSGB36→WGS84",
    ));

    // ED50 → WGS84
    points.extend(transform(
        4230,
        4326,
        2.3522,
        48.8566,
        0.001,
        "Paris ED50→WGS84",
    ));
    points.extend(transform(
        4230,
        4326,
        13.4050,
        52.5200,
        0.001,
        "Berlin ED50→WGS84",
    ));

    // =========================================================================
    // 4. ROUNDTRIP verification points (forward then inverse)
    // =========================================================================

    // For each projected CRS, verify a roundtrip
    let roundtrip_targets: &[(u32, f64, f64, &str)] = &[
        (3857, -74.0445, 40.6892, "NYC↔3857"),
        (32618, -74.006, 40.7128, "NYC↔UTM18N"),
        (3413, -45.0, 75.0, "75N↔3413"),
        (3031, 0.0, -75.0, "75S↔3031"),
        (2154, 2.3522, 48.8566, "Paris↔Lambert93"),
        (5070, -96.0, 37.0, "USCenter↔Albers"),
        (3395, -74.006, 40.7128, "NYC↔WorldMerc"),
        (32662, -74.006, 40.7128, "NYC↔PlateCarree"),
    ];
    for &(to_epsg, lon, lat, name) in roundtrip_targets {
        if let Some(fwd) = transform(4326, to_epsg, lon, lat, 0.001, "") {
            points.extend(transform(
                to_epsg,
                4326,
                fwd.expected_x,
                fwd.expected_y,
                1e-7,
                &format!("roundtrip {name} inverse"),
            ));
        }
    }

    let projected_roundtrip_targets: &[(u32, u32, f64, f64, &str)] = &[
        (4258, 3035, 5.0, 50.0, "Europe↔LAEA"),
        (4326, 6931, -150.0, 70.0, "Alaska↔LAEA north"),
        (4326, 6932, 45.0, -75.0, "Antarctic↔LAEA south"),
        (10346, 3408, -150.0, 70.0, "Alaska↔spherical LAEA north"),
        (10346, 3409, 45.0, -75.0, "Antarctic↔spherical LAEA south"),
        (4267, 9311, -96.0, 37.0, "USCenter↔spherical LAEA"),
        (4289, 28992, 5.0, 53.0, "Netherlands↔RD New"),
        (4269, 3078, -86.0, 45.3, "Michigan↔Hotine A"),
        (4150, 2056, 7.4386, 46.9511, "Bern↔Hotine B"),
        (4302, 30200, -62.0, 10.0, "Trinidad↔Cassini"),
        (4742, 3377, 103.75, 1.5, "Johor↔Cassini"),
    ];
    for &(from_epsg, to_epsg, lon, lat, name) in projected_roundtrip_targets {
        if let Some(fwd) = transform(from_epsg, to_epsg, lon, lat, 0.001, "") {
            points.extend(transform(
                to_epsg,
                from_epsg,
                fwd.expected_x,
                fwd.expected_y,
                1e-7,
                &format!("roundtrip {name} inverse"),
            ));
        }
    }

    // =========================================================================
    // 5. PRECISION points for projections previously covered only by loose
    //    tolerances or roundtrips: LCC, Albers, Cassini, Mercator, Eq. Cyl.
    //    Tolerance is 1 mm: proj-core's LCC/Albers forward northing differs
    //    from C PROJ by a uniform ~0.10 mm (series/precision difference), so
    //    sub-0.1 mm parity is tracked separately rather than asserted here.
    // =========================================================================

    let precision_targets: &[(u32, u32, f64, f64, &str)] = &[
        (4326, 2154, 2.3522, 48.8566, "Paris 4326→2154 LCC precise"),
        (4326, 2154, 5.0, 44.0, "SE France 4326→2154 LCC precise"),
        (
            4326,
            5070,
            -96.0,
            37.0,
            "US center 4326→5070 Albers precise",
        ),
        (4326, 5070, -118.24, 34.05, "LA 4326→5070 Albers precise"),
        (
            4302,
            30200,
            -61.5,
            10.5,
            "Trinidad 4302→30200 Cassini precise",
        ),
        (
            4326,
            3395,
            -74.006,
            40.7128,
            "NYC 4326→3395 Mercator precise",
        ),
        (
            4326,
            3395,
            151.2093,
            -33.8688,
            "Sydney 4326→3395 Mercator precise",
        ),
        (
            4326,
            32662,
            -74.006,
            40.7128,
            "NYC 4326→32662 EqCyl precise",
        ),
    ];
    for &(from_epsg, to_epsg, lon, lat, name) in precision_targets {
        points.extend(transform(from_epsg, to_epsg, lon, lat, 1e-3, name));
        if let Some(fwd) = transform(from_epsg, to_epsg, lon, lat, 1e-3, "") {
            points.extend(transform(
                to_epsg,
                from_epsg,
                fwd.expected_x,
                fwd.expected_y,
                1e-9,
                &format!("{name} inverse"),
            ));
        }
    }

    // Colombia Urban (EPSG method 1052): MAGNA-SIRGAS / Bogota urban grid.
    points.extend(transform(
        4686,
        6247,
        -74.25,
        4.8,
        1e-3,
        "Bogota 4686→6247 Colombia Urban",
    ));
    points.extend(transform(
        4686,
        6247,
        -74.0,
        4.6,
        1e-3,
        "Bogota SE 4686→6247 Colombia Urban",
    ));
    if let Some(fwd) = transform(4686, 6247, -74.25, 4.8, 1e-3, "") {
        points.extend(transform(
            6247,
            4686,
            fwd.expected_x,
            fwd.expected_y,
            1e-9,
            "Bogota 6247→4686 Colombia Urban inverse",
        ));
    }

    // =========================================================================
    // 6. EDGE cases: near-pole inverses and wrong-hemisphere polar stereo
    // =========================================================================

    // Near-pole inverse points for transverse Mercator: project a point close
    // to the pole, then record the inverse as its own reference point. The
    // exact (Poder/Engsager) transverse Mercator recovers these like C PROJ.
    let near_pole_tm: &[(u32, f64, f64, Option<&str>, &str)] = &[
        (32618, -75.0, 89.9999999, None, "UTM18N near north pole"),
        (
            32618,
            -70.5,
            89.999999,
            None,
            "UTM18N off-CM near north pole",
        ),
        (32718, -72.0, -89.9999999, None, "UTM18S near south pole"),
    ];
    for &(epsg, lon, lat, pending, name) in near_pole_tm {
        if let Some(fwd) = transform(4326, epsg, lon, lat, 0.01, &format!("{name} forward")) {
            points.push(fwd);
            if let Some(fwd_again) = transform(4326, epsg, lon, lat, 0.01, "") {
                // Longitude is ill-conditioned at the pole; C PROJ still
                // recovers it to ~1e-7 deg, so hold the inverse to 1e-4 deg.
                points.extend(
                    transform(
                        epsg,
                        4326,
                        fwd_again.expected_x,
                        fwd_again.expected_y,
                        1e-4,
                        &format!("{name} inverse"),
                    )
                    .map(|mut point| {
                        point.pending_fix = pending.map(str::to_string);
                        point
                    }),
                );
            }
        }
    }

    // Wrong-hemisphere inputs for polar stereographic: the conformal-latitude
    // formula extends continuously across the equator to large-radius points,
    // matching C PROJ.
    let wrong_hemisphere_ps: &[(u32, f64, f64, Option<&str>, &str)] = &[
        (
            3413,
            -45.0,
            -30.0,
            None,
            "southern point into north polar stereo 4326→3413",
        ),
        (
            3031,
            0.0,
            30.0,
            None,
            "northern point into south polar stereo 4326→3031",
        ),
        (
            3413,
            -45.0,
            0.0,
            None,
            "equator into north polar stereo 4326→3413",
        ),
    ];
    for &(epsg, lon, lat, pending, name) in wrong_hemisphere_ps {
        points.extend(
            transform(4326, epsg, lon, lat, 0.01, name).map(|mut point| {
                point.pending_fix = pending.map(str::to_string);
                point
            }),
        );
    }

    // =========================================================================
    // 7. 3D points through promoted 3D CRSs. Cross-datum Helmert paths change
    //    the ellipsoidal height and proj-core propagates it through the
    //    horizontal pipeline; same-datum paths preserve the input height.
    // =========================================================================

    // (from_epsg, to_epsg, lon, lat, height, tolerance, tolerance_z, name,
    // pending fix marker)
    type ThreeDCase = (
        u32,
        u32,
        f64,
        f64,
        f64,
        f64,
        f64,
        &'static str,
        Option<&'static str>,
    );
    let three_d_points: &[ThreeDCase] = &[
        (
            4326,
            3857,
            -74.006,
            40.7128,
            15.0,
            0.001,
            1e-9,
            "NYC 3D 4326→3857 preserves height",
            None,
        ),
        (
            4267,
            4326,
            -90.0,
            45.0,
            250.0,
            0.001,
            0.01,
            "US Midwest 3D NAD27→WGS84",
            Some("P1.7 operation selection parity: C PROJ promoted-3D pair picks a different registry operation"),
        ),
        (
            4277,
            4326,
            -0.1278,
            51.5074,
            45.0,
            0.001,
            0.01,
            "London 3D OSGB36→WGS84",
            None,
        ),
        (
            4230,
            4326,
            2.3522,
            48.8566,
            100.0,
            0.001,
            0.01,
            "Paris 3D ED50→WGS84",
            Some("P1.7 operation selection parity: C PROJ promoted-3D pair picks a different registry operation"),
        ),
        (
            4326,
            4277,
            -0.1278,
            51.5074,
            95.0,
            0.001,
            0.01,
            "London 3D WGS84→OSGB36 reverse",
            None,
        ),
        (
            4326,
            27700,
            -0.1278,
            51.5074,
            45.0,
            0.01,
            0.01,
            "London 3D WGS84→British National Grid",
            None,
        ),
    ];
    for &(from_epsg, to_epsg, x, y, z, tol, tol_z, name, pending) in three_d_points {
        points.extend(transform_3d(
            from_epsg, to_epsg, x, y, z, tol, tol_z, name, pending,
        ));
    }

    eprintln!("Generated {} reference points", points.len());
    let json = serde_json::to_string_pretty(&points).unwrap();
    println!("{json}");
}
