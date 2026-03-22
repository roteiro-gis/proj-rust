use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use proj::Proj;
use proj_core::Transform;

struct SinglePointCase {
    name: &'static str,
    from_epsg: u32,
    to_epsg: u32,
    coord: (f64, f64),
}

fn c_proj_transform(from_epsg: u32, to_epsg: u32) -> Proj {
    let from = format!("EPSG:{from_epsg}");
    let to = format!("EPSG:{to_epsg}");
    Proj::new_known_crs(&from, &to, None).unwrap_or_else(|e| {
        panic!("failed to create C PROJ transform {from}->{to}: {e}");
    })
}

fn bench_single_point(c: &mut Criterion) {
    let cases = [
        SinglePointCase {
            name: "4326->3857",
            from_epsg: 4326,
            to_epsg: 3857,
            coord: (-74.006, 40.7128),
        },
        SinglePointCase {
            name: "4326->32618",
            from_epsg: 4326,
            to_epsg: 32618,
            coord: (-74.006, 40.7128),
        },
        SinglePointCase {
            name: "4326->3413",
            from_epsg: 4326,
            to_epsg: 3413,
            coord: (-45.0, 75.0),
        },
        SinglePointCase {
            name: "4267->4326",
            from_epsg: 4267,
            to_epsg: 4326,
            coord: (-90.0, 45.0),
        },
    ];

    let mut group = c.benchmark_group("single-point-vs-c-proj");

    for case in cases {
        let rust_transform = Transform::from_epsg(case.from_epsg, case.to_epsg).unwrap();
        let c_transform = c_proj_transform(case.from_epsg, case.to_epsg);

        group.bench_with_input(
            BenchmarkId::new("proj-rust", case.name),
            &case.coord,
            |b, coord| b.iter(|| rust_transform.convert(black_box(*coord)).unwrap()),
        );
        group.bench_with_input(
            BenchmarkId::new("c-proj", case.name),
            &case.coord,
            |b, coord| b.iter(|| c_transform.convert(black_box(*coord)).unwrap()),
        );
    }

    group.finish();
}

fn bench_batch_web_mercator(c: &mut Criterion) {
    let rust_transform = Transform::from_epsg(4326, 3857).unwrap();
    let c_transform = c_proj_transform(4326, 3857);
    let coords: Vec<(f64, f64)> = (0..10_000)
        .map(|i| (-74.0 + (i as f64) * 0.001, 40.0 + (i as f64) * 0.001))
        .collect();

    let mut group = c.benchmark_group("batch-10k-4326->3857-vs-c-proj");
    group.throughput(Throughput::Elements(coords.len() as u64));

    group.bench_function("proj-rust sequential", |b| {
        b.iter(|| rust_transform.convert_batch(black_box(&coords)).unwrap())
    });
    #[cfg(feature = "rayon")]
    group.bench_function("proj-rust parallel", |b| {
        b.iter(|| {
            rust_transform
                .convert_batch_parallel(black_box(&coords))
                .unwrap()
        })
    });
    group.bench_function("c-proj sequential", |b| {
        b.iter(|| {
            black_box(&coords)
                .iter()
                .map(|&(x, y)| c_transform.convert((x, y)).unwrap())
                .collect::<Vec<_>>()
        })
    });

    group.finish();
}

criterion_group!(benches, bench_single_point, bench_batch_web_mercator);
criterion_main!(benches);
