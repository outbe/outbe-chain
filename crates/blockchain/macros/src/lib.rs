//! Procedural macros for Outbe precompile contracts and storage DSL.

mod contract_codegen;
mod dispatch_codegen;
mod field_attrs;
mod record_codegen;

use contract_codegen::{generate_contract, ContractConfig};
use proc_macro::TokenStream;
use record_codegen::{generate_storage_record, StorageRecordConfig};
use syn::{parse_macro_input, DeriveInput, GenericArgument, PathArguments, Type};

fn type_args(ty: &Type) -> Option<Vec<GenericArgument>> {
    let Type::Path(p) = ty else {
        return None;
    };
    let seg = p.path.segments.last()?;
    let PathArguments::AngleBracketed(args) = &seg.arguments else {
        return Some(Vec::new());
    };
    Some(args.args.iter().cloned().collect())
}

fn last_type_ident(ty: &Type) -> Option<String> {
    let Type::Path(p) = ty else {
        return None;
    };
    Some(p.path.segments.last()?.ident.to_string())
}

fn is_ident_type(ty: &Type, wanted: &[&str]) -> bool {
    last_type_ident(ty)
        .map(|name| wanted.iter().any(|wanted| name == *wanted))
        .unwrap_or(false)
}

fn is_mapping_type(ty: &Type) -> bool {
    is_ident_type(ty, &["Mapping"])
}

fn is_slot_type(ty: &Type) -> bool {
    is_ident_type(ty, &["Slot"])
}

fn is_storage_collection(ty: &Type) -> Option<(&'static str, u64)> {
    if let Some(name) = last_type_ident(ty) {
        return match name.as_str() {
            "StorageVec" => Some(("StorageVec", 1)),
            "StorageSet" => Some(("StorageSet", 2)),
            "StorageArray" => Some(("StorageArray", 1)),
            "StorageBytes" => Some(("StorageBytes", 1)),
            _ => None,
        };
    }
    None
}

fn is_dsl_value_type(ty: &Type) -> bool {
    is_ident_type(ty, &["Value"])
}

fn is_dsl_map_type(ty: &Type) -> bool {
    is_ident_type(ty, &["Map"])
}

fn is_dsl_list_type(ty: &Type) -> bool {
    is_ident_type(ty, &["List"])
}

fn is_dsl_set_type(ty: &Type) -> bool {
    is_ident_type(ty, &["Set"])
}

fn is_dsl_binary_heap_type(ty: &Type) -> bool {
    is_ident_type(ty, &["BinaryHeap"])
}

fn is_dsl_deque_type(ty: &Type) -> bool {
    is_ident_type(ty, &["Deque"])
}

fn is_dsl_circular_buffer_type(ty: &Type) -> bool {
    is_ident_type(ty, &["CircularBuffer"])
}

fn is_optional_type(ty: &Type) -> bool {
    is_ident_type(ty, &["Optional", "Option"])
}

fn is_deprecated_type(ty: &Type) -> bool {
    is_ident_type(ty, &["Deprecated"])
}

fn unwrap_single_generic_type(ty: &Type) -> Option<Type> {
    let args = type_args(ty)?;
    match args.first() {
        Some(GenericArgument::Type(inner)) => Some(inner.clone()),
        _ => None,
    }
}

fn is_scalar_like_type(ty: &Type) -> bool {
    matches!(
        last_type_ident(ty).as_deref(),
        Some("u8")
            | Some("u16")
            | Some("u32")
            | Some("u64")
            | Some("bool")
            | Some("U256")
            | Some("Address")
            | Some("B256")
            | Some("Optional")
            | Some("Option")
            | Some("Deprecated")
    )
}

#[proc_macro_attribute]
pub fn storage_schema(_attr: TokenStream, item: TokenStream) -> TokenStream {
    item
}

/// Annotates an `impl` block whose methods (each carrying
/// `#[contract_public("sig")]`) declare a precompile's ABI surface and
/// dispatch wiring. Generates a private `sol!` interface plus a free
/// `pub fn dispatch(storage, data, caller, value) -> Result<Bytes>`.
///
/// Companion markers on individual methods:
/// - `#[contract_view]` - read-only; method takes only ABI args.
/// - `#[contract_payable]` - `caller: Address, value: U256` are the first
///   two parameters after `&mut self`, followed by ABI args.
/// - (no marker) - default mutating: `caller: Address` is the first
///   parameter after `&mut self`, followed by ABI args.
#[proc_macro_attribute]
pub fn contract_dispatch(attr: TokenStream, item: TokenStream) -> TokenStream {
    dispatch_codegen::expand_dispatch(attr, item)
}

/// Marks a method inside a `#[contract_dispatch]` impl block as an ABI
/// entry. The string is a Solidity-style signature; argument names are
/// taken from the Rust method (only types are read from the string).
/// Consumed by the surrounding `#[contract_dispatch]` macro.
#[proc_macro_attribute]
pub fn contract_public(_attr: TokenStream, item: TokenStream) -> TokenStream {
    item
}

/// Inside a `#[contract_dispatch]` impl block: marks the method as
/// read-only (no caller / no msg.value injection). Consumed by the
/// surrounding `#[contract_dispatch]` macro.
#[proc_macro_attribute]
pub fn contract_view(_attr: TokenStream, item: TokenStream) -> TokenStream {
    item
}

/// Inside a `#[contract_dispatch]` impl block: marks the method as
/// payable; first two parameters after `&mut self` are
/// `caller: Address, value: U256`. Consumed by the surrounding
/// `#[contract_dispatch]` macro.
///
/// Using this requires the module to publish its payable surface next to the
/// impl block:
///
/// ```ignore
/// pub const PAYABLE_SELECTORS: &[[u8; 4]] = &[__MyContractAbi::fundCall::SELECTOR];
/// ```
///
/// The generated dispatch refuses value for every selector missing from that
/// list, and the precompile route table asserts at compile time that the list
/// agrees with the address's declared value policy. Omitting the const is a
/// compile error (see `tests/compile_fail/payable_without_selectors.rs`).
#[proc_macro_attribute]
pub fn contract_payable(_attr: TokenStream, item: TokenStream) -> TokenStream {
    item
}

#[proc_macro_attribute]
pub fn contract(attr: TokenStream, item: TokenStream) -> TokenStream {
    let config = parse_macro_input!(attr as ContractConfig);
    let input = parse_macro_input!(item as DeriveInput);

    match generate_contract(input, config) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

#[proc_macro_attribute]
pub fn storage_record(attr: TokenStream, item: TokenStream) -> TokenStream {
    let config = parse_macro_input!(attr as StorageRecordConfig);
    let input = parse_macro_input!(item as DeriveInput);

    match generate_storage_record(input, config) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

#[cfg(test)]
mod tests;
