use outbe_metadosis::{emission_sink, runtime, schema};

fn main() {
    let _ = std::any::type_name::<schema::MetadosisContract<'static>>();
    let _ = emission_sink::apply;
    let _ = runtime::advance_active_worldwide_days;
}
