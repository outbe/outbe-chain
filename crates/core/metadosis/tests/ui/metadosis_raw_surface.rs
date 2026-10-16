use outbe_metadosis::{emission_sink, runtime, schema, state};

fn main() {
    let _ = (
        std::any::type_name::<schema::MetadosisContract<'static>>(),
        std::any::type_name::<state::State<'static>>(),
    );
    let _ = emission_sink::apply;
    let _ = runtime::advance_active_worldwide_days;
}
