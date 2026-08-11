pub struct HelmertRegressionCase {
    pub operation_epsg: u32,
    pub source_epsg: u32,
    pub target_epsg: u32,
    pub input: (f64, f64, f64),
    pub expected_xy: (f64, f64),
    pub description: &'static str,
}

pub fn cases() -> [HelmertRegressionCase; 4] {
    [
        HelmertRegressionCase {
            operation_epsg: 8048,
            source_epsg: 4283,
            target_epsg: 7844,
            input: (151.0, -33.0, 100.0),
            expected_xy: (151.00000557415464, -32.99998729124719),
            description: "millimetre-valued coordinate-frame transformation",
        },
        HelmertRegressionCase {
            operation_epsg: 6889,
            source_epsg: 5451,
            target_epsg: 4326,
            input: (-88.5, 14.5, 100.0),
            expected_xy: (-88.49807144430487, 14.4982986482546),
            description: "position-vector Molodensky-Badekas transformation",
        },
        HelmertRegressionCase {
            operation_epsg: 1066,
            source_epsg: 4289,
            target_epsg: 4258,
            input: (5.4, 52.2, 100.0),
            expected_xy: (5.399562976933926, 52.19900652636685),
            description: "coordinate-frame Molodensky-Badekas transformation",
        },
        HelmertRegressionCase {
            operation_epsg: 10676,
            source_epsg: 10636,
            target_epsg: 4326,
            input: (-63.24, 17.64, 100.0),
            expected_xy: (-63.239398460214225, 17.640770844880024),
            description: "coordinate-frame full-matrix transformation",
        },
    ]
}
