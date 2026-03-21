use proj::Proj;
use serde::Serialize;

#[derive(Serialize)]
struct ReferencePoint {
    from_epsg: u32,
    to_epsg: u32,
    input_x: f64,
    input_y: f64,
    expected_x: f64,
    expected_y: f64,
    tolerance: f64,
    description: String,
}

fn transform(from: u32, to: u32, x: f64, y: f64, tol: f64, desc: &str) -> Option<ReferencePoint> {
    let from_crs = format!("EPSG:{from}");
    let to_crs = format!("EPSG:{to}");
    let proj = Proj::new_known_crs(&from_crs, &to_crs, None).ok()?;
    let (ox, oy) = proj.convert((x, y)).ok()?;
    if ox.is_finite() && oy.is_finite() {
        Some(ReferencePoint {
            from_epsg: from,
            to_epsg: to,
            input_x: x,
            input_y: y,
            expected_x: ox,
            expected_y: oy,
            tolerance: tol,
            description: desc.to_string(),
        })
    } else {
        None
    }
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
            points.extend(transform(4326, 3857, lon, lat, 0.001,
                &format!("{name} 4326→3857")));
        }
    }

    // World Mercator (3395)
    for &(lon, lat, name) in GEO_POINTS {
        if lat.abs() <= 80.0 {
            points.extend(transform(4326, 3395, lon, lat, 0.01,
                &format!("{name} 4326→3395")));
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
        let epsg = if north { 32600 + zone as u32 } else { 32700 + zone as u32 };
        points.extend(transform(4326, epsg, lon, lat, 0.01,
            &format!("{name} 4326→UTM{epsg}")));
        // Also test inverse
        if let Some(fwd) = transform(4326, epsg, lon, lat, 0.01, "") {
            points.extend(transform(epsg, 4326, fwd.expected_x, fwd.expected_y, 2e-7,
                &format!("{name} UTM{epsg}→4326 inverse")));
        }
    }

    // Polar Stereographic North (3413)
    for &(lon, lat, name) in ARCTIC_POINTS {
        points.extend(transform(4326, 3413, lon, lat, 0.01,
            &format!("{name} 4326→3413")));
    }

    // Antarctic Polar Stereographic (3031)
    for &(lon, lat, name) in ANTARCTIC_POINTS {
        points.extend(transform(4326, 3031, lon, lat, 0.1,
            &format!("{name} 4326→3031")));
    }

    // Arctic Polar Stereographic (3995)
    points.extend(transform(4326, 3995, 0.0, 80.0, 0.01, "80N 0E 4326→3995"));
    points.extend(transform(4326, 3995, 90.0, 75.0, 0.01, "75N 90E 4326→3995"));

    // France Lambert-93 (2154)
    points.extend(transform(4326, 2154, 2.3522, 48.8566, 0.1, "Paris 4326→2154"));
    points.extend(transform(4326, 2154, -1.6778, 48.1173, 0.1, "Rennes 4326→2154"));
    points.extend(transform(4326, 2154, 7.2620, 43.7102, 0.1, "Nice 4326→2154"));

    // CONUS Albers (5070)
    points.extend(transform(4326, 5070, -96.0, 37.0, 0.1, "US center 4326→5070"));
    points.extend(transform(4326, 5070, -74.0, 40.7, 0.1, "NYC 4326→5070"));
    points.extend(transform(4326, 5070, -122.4, 37.8, 0.1, "SF 4326→5070"));

    // BC Albers (3005)
    points.extend(transform(4326, 3005, -123.1, 49.3, 0.1, "Vancouver 4326→3005"));

    // Canada Lambert (3347)
    points.extend(transform(4326, 3347, -75.7, 45.4, 0.1, "Ottawa 4326→3347"));

    // British National Grid (27700) — requires datum shift OSGB36
    points.extend(transform(4326, 27700, -0.1278, 51.5074, 1.0, "London 4326→27700"));

    // Plate Carree (32662)
    for &(lon, lat, name) in GEO_POINTS {
        points.extend(transform(4326, 32662, lon, lat, 0.01,
            &format!("{name} 4326→32662")));
    }

    // =========================================================================
    // 2. INVERSE: projected → WGS84 for key CRS
    // =========================================================================

    // 3857 → 4326 inverse
    points.extend(transform(3857, 4326, -8242596.0, 4966606.0, 1e-8, "NYC 3857→4326"));
    points.extend(transform(3857, 4326, 0.0, 0.0, 1e-8, "origin 3857→4326"));
    points.extend(transform(3857, 4326, 15550408.0, 4257980.0, 1e-8, "Tokyo 3857→4326"));

    // 3413 → 4326 inverse
    points.extend(transform(3413, 4326, 0.0, 0.0, 1e-6, "north pole 3413→4326"));
    points.extend(transform(3413, 4326, 0.0, -1633879.0, 1e-6, "75N on CM 3413→4326"));

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
        points.extend(transform(4267, 4326, lon, lat, 0.001,
            &format!("{name} NAD27→WGS84")));
    }

    // WGS84 → NAD27 (inverse datum shift)
    points.extend(transform(4326, 4267, -90.0, 45.0, 0.001, "US Midwest WGS84→NAD27"));

    // OSGB36 → WGS84
    points.extend(transform(4277, 4326, -0.1278, 51.5074, 0.001, "London OSGB36→WGS84"));
    points.extend(transform(4277, 4326, -3.1883, 55.9533, 0.001, "Edinburgh OSGB36→WGS84"));
    points.extend(transform(4277, 4326, -1.8904, 52.4862, 0.001, "Birmingham OSGB36→WGS84"));

    // ED50 → WGS84
    points.extend(transform(4230, 4326, 2.3522, 48.8566, 0.001, "Paris ED50→WGS84"));
    points.extend(transform(4230, 4326, 13.4050, 52.5200, 0.001, "Berlin ED50→WGS84"));

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
            points.extend(transform(to_epsg, 4326, fwd.expected_x, fwd.expected_y, 1e-7,
                &format!("roundtrip {name} inverse")));
        }
    }

    eprintln!("Generated {} reference points", points.len());
    let json = serde_json::to_string_pretty(&points).unwrap();
    println!("{json}");
}
