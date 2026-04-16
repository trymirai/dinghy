use criterion::Criterion;

#[derive(Clone, Copy)]
pub struct TestCase {
    pub name: &'static str,
    pub run: fn(),
}

#[derive(Clone, Copy)]
pub struct BenchCase {
    pub name: &'static str,
    pub run: fn(&mut Criterion),
}

#[derive(Clone, Copy)]
pub struct IgnoredCase {
    pub name: &'static str,
}

inventory::collect!(TestCase);
inventory::collect!(BenchCase);
inventory::collect!(IgnoredCase);
