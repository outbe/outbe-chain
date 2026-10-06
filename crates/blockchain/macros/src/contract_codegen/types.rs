//! Storage type lifetimes and slot footprints for contract fields.

use super::*;

pub(super) fn storage_type_with_lifetime(ty: &Type) -> proc_macro2::TokenStream {
    let Type::Path(p) = ty else {
        return quote! { #ty };
    };
    let Some(seg) = p.path.segments.last() else {
        return quote! { #ty };
    };
    let name = &seg.ident;

    match name.to_string().as_str() {
        "Slot" => {
            let args = type_args(ty).unwrap_or_default();
            quote! { #name<'storage, #(#args),*> }
        }
        "Mapping" => {
            let args = type_args(ty).unwrap_or_default();
            if args.len() == 2 {
                let key = &args[0];
                let value = match &args[1] {
                    GenericArgument::Type(inner) => storage_type_with_lifetime(inner),
                    other => quote! { #other },
                };
                quote! { #name<'storage, #key, #value> }
            } else {
                quote! { #ty }
            }
        }
        "StorageBytes" => quote! { #name<'storage> },
        "StorageVec" | "StorageSet" | "StorageArray" => {
            let args = type_args(ty).unwrap_or_default();
            quote! { #name<'storage, #(#args),*> }
        }
        "Value" => {
            let inner = unwrap_single_generic_type(ty).unwrap();
            quote! { ::outbe_primitives::storage::dsl::Value<'storage, #inner> }
        }
        "Map" => {
            let args = type_args(ty).unwrap_or_default();
            if args.len() == 2 {
                let key = &args[0];
                let value = match &args[1] {
                    GenericArgument::Type(inner) => storage_type_with_lifetime(inner),
                    other => quote! { #other },
                };
                quote! { ::outbe_primitives::storage::dsl::Map<'storage, #key, #value> }
            } else {
                quote! { ::outbe_primitives::storage::dsl::Map<'storage, #(#args),*> }
            }
        }
        "List" => {
            let inner = unwrap_single_generic_type(ty).unwrap();
            quote! { ::outbe_primitives::storage::dsl::List<'storage, #inner> }
        }
        "Set" => {
            let inner = unwrap_single_generic_type(ty).unwrap();
            quote! { ::outbe_primitives::storage::dsl::Set<'storage, #inner> }
        }
        "BinaryHeap" => {
            let inner = unwrap_single_generic_type(ty).unwrap();
            quote! { ::outbe_primitives::storage::dsl::BinaryHeap<'storage, #inner> }
        }
        "Deque" => {
            let inner = unwrap_single_generic_type(ty).unwrap();
            quote! { ::outbe_primitives::storage::dsl::Deque<'storage, #inner> }
        }
        "CircularBuffer" => {
            let inner = unwrap_single_generic_type(ty).unwrap();
            quote! { ::outbe_primitives::storage::dsl::CircularBuffer<'storage, #inner> }
        }
        _ => quote! { #ty },
    }
}

pub(super) fn contract_slot_count_expr(ty: &Type) -> proc_macro2::TokenStream {
    if is_slot_type(ty) || is_mapping_type(ty) || is_single_slot_dsl_type(ty) {
        return quote! { 1u64 };
    }
    if is_dsl_set_type(ty) || is_dsl_deque_type(ty) || is_dsl_circular_buffer_type(ty) {
        return quote! { 2u64 };
    }
    if let Some((_, count)) = is_storage_collection(ty) {
        return quote! { #count as u64 };
    }
    if is_dsl_map_type(ty) {
        let args = type_args(ty).unwrap_or_default();
        if let Some(GenericArgument::Type(value_ty)) = args.get(1) {
            if is_scalar_like_type(value_ty) || is_dsl_list_type(value_ty) {
                quote! { 1u64 }
            } else {
                quote! { <#value_ty as ::outbe_primitives::storage::dsl::StorageRecord>::SLOTS as u64 }
            }
        } else {
            quote! { 1u64 }
        }
    } else {
        quote! { 1u64 }
    }
}

fn is_single_slot_dsl_type(ty: &Type) -> bool {
    is_dsl_value_type(ty) || is_dsl_list_type(ty) || is_dsl_binary_heap_type(ty)
}
