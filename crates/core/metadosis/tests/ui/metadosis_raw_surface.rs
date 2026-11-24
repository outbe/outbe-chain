use outbe_metadosis::{limits, runtime, schema};

fn main() {
    let _ = std::any::type_name::<schema::MetadosisContract<'static>>();
    let _ = limits::set_metadosis_limit;
    let _ = runtime::process_emission_day;
}
