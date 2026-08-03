use outbe_metadosis::fixture_kernel::FixtureKernelExt;

fn main() {
    let _ = std::any::type_name::<dyn FixtureKernelExt>();
}
