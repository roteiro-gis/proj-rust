use criterion::{black_box, criterion_group, criterion_main, Criterion};
use proj_core::Transform;

fn single_point_web_mercator(c: &mut Criterion) {
    let t = Transform::new("EPSG:4326", "EPSG:3857").unwrap();
    c.bench_function("single point 4326→3857", |b| {
        b.iter(|| t.convert(black_box((-74.006, 40.7128))).unwrap())
    });
}

fn single_point_utm(c: &mut Criterion) {
    let t = Transform::new("EPSG:4326", "EPSG:32618").unwrap();
    c.bench_function("single point 4326→UTM18N", |b| {
        b.iter(|| t.convert(black_box((-74.006, 40.7128))).unwrap())
    });
}

fn single_point_polar_stereo(c: &mut Criterion) {
    let t = Transform::new("EPSG:4326", "EPSG:3413").unwrap();
    c.bench_function("single point 4326→3413", |b| {
        b.iter(|| t.convert(black_box((-45.0, 75.0))).unwrap())
    });
}

fn single_point_datum_shift(c: &mut Criterion) {
    let t = Transform::new("EPSG:4267", "EPSG:4326").unwrap();
    c.bench_function("single point NAD27→WGS84", |b| {
        b.iter(|| t.convert(black_box((-90.0, 45.0))).unwrap())
    });
}

fn batch_10k_web_mercator(c: &mut Criterion) {
    let t = Transform::new("EPSG:4326", "EPSG:3857").unwrap();
    let coords: Vec<(f64, f64)> = (0..10_000)
        .map(|i| (-74.0 + (i as f64) * 0.001, 40.0 + (i as f64) * 0.001))
        .collect();

    c.bench_function("batch 10K 4326→3857 sequential", |b| {
        b.iter(|| t.convert_batch(black_box(&coords)).unwrap())
    });
}

fn batch_10k_web_mercator_parallel(c: &mut Criterion) {
    let t = Transform::new("EPSG:4326", "EPSG:3857").unwrap();
    let coords: Vec<(f64, f64)> = (0..10_000)
        .map(|i| (-74.0 + (i as f64) * 0.001, 40.0 + (i as f64) * 0.001))
        .collect();

    c.bench_function("batch 10K 4326→3857 parallel", |b| {
        b.iter(|| t.convert_batch_parallel(black_box(&coords)).unwrap())
    });
}

criterion_group!(
    benches,
    single_point_web_mercator,
    single_point_utm,
    single_point_polar_stereo,
    single_point_datum_shift,
    batch_10k_web_mercator,
    batch_10k_web_mercator_parallel,
);
criterion_main!(benches);
