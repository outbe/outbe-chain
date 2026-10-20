//! Contract field analysis, layout and token emission.

use super::{
    field_attrs::{parse_common_field_attrs, CommonFieldAttrs},
    is_dsl_binary_heap_type, is_dsl_circular_buffer_type, is_dsl_deque_type, is_dsl_list_type,
    is_dsl_map_type, is_dsl_set_type, is_dsl_value_type, is_mapping_type, is_slot_type,
    is_storage_array, is_storage_collection, type_args, unwrap_single_generic_type,
};
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{
    parse::{Parse, ParseStream},
    Data, DeriveInput, Expr, Fields, GenericArgument, Ident, Token, Type,
};

mod types;

use types::{contract_slot_count_expr, storage_type_with_lifetime};

pub(super) struct ContractConfig {
    address: Option<Expr>,
}

impl Parse for ContractConfig {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        if input.is_empty() {
            return Ok(Self { address: None });
        }
        let ident: Ident = input.parse()?;
        if ident != "addr" && ident != "address" {
            return Err(syn::Error::new(ident.span(), "expected `addr = EXPR`"));
        }
        input.parse::<Token![=]>()?;
        let address: Expr = input.parse()?;
        Ok(Self {
            address: Some(address),
        })
    }
}

#[derive(Clone)]
struct ContractFieldInfo {
    vis: syn::Visibility,
    name: Ident,
    ty: Type,
    attrs: CommonFieldAttrs,
}

/// Base slot of each field (in declaration order) and the order in which the
/// allocator placed the fields.
struct SlotLayout {
    slots: Vec<TokenStream2>,
    allocation_order: Vec<usize>,
}

pub(super) fn generate_contract(
    input: DeriveInput,
    config: ContractConfig,
) -> syn::Result<TokenStream2> {
    let fields = parse_fields(&input)?;
    let layout = assign_slots(&fields);
    Ok(emit_contract(&input, &config, &fields, &layout))
}

fn parse_fields(input: &DeriveInput) -> syn::Result<Vec<ContractFieldInfo>> {
    let name = &input.ident;
    let named_fields = match &input.data {
        Data::Struct(data) => match &data.fields {
            Fields::Named(f) => &f.named,
            _ => {
                return Err(syn::Error::new_spanned(
                    name,
                    "only structs with named fields are supported",
                ))
            }
        },
        _ => return Err(syn::Error::new_spanned(name, "only structs are supported")),
    };

    let mut fields = Vec::new();
    for field in named_fields.iter() {
        let field_name = field.ident.as_ref().unwrap();
        let n = field_name.to_string();
        if n == "address" || n == "storage" {
            return Err(syn::Error::new_spanned(
                field_name,
                format!("field name `{n}` is reserved - generated automatically"),
            ));
        }
        fields.push(ContractFieldInfo {
            vis: field.vis.clone(),
            name: field_name.clone(),
            ty: field.ty.clone(),
            attrs: parse_common_field_attrs(&field.attrs)?,
        });
    }

    Ok(fields)
}

fn assign_slots(fields: &[ContractFieldInfo]) -> SlotLayout {
    let use_order = fields.iter().any(|f| f.attrs.order.is_some())
        && fields.iter().all(|f| f.attrs.explicit_slot.is_none());
    if use_order {
        assign_ordered_slots(fields)
    } else {
        assign_sequential_slots(fields)
    }
}

fn assign_ordered_slots(fields: &[ContractFieldInfo]) -> SlotLayout {
    let mut allocation_order: Vec<usize> = (0..fields.len()).collect();
    allocation_order.sort_by_key(|&i| fields[i].attrs.order.unwrap_or(i as u64));
    let mut slots = vec![quote! { 0u64 }; fields.len()];
    let mut next = quote! { 0u64 };
    for &idx in &allocation_order {
        slots[idx] = next.clone();
        let count = contract_slot_count_expr(&fields[idx].ty);
        next = quote! { (#next) + (#count) };
    }
    SlotLayout {
        slots,
        allocation_order,
    }
}

fn assign_sequential_slots(fields: &[ContractFieldInfo]) -> SlotLayout {
    let mut next_slot = quote! { 0u64 };
    let mut slots = Vec::with_capacity(fields.len());
    for f in fields {
        if let Some(explicit) = f.attrs.explicit_slot {
            next_slot = quote! { #explicit as u64 };
        }
        slots.push(next_slot.clone());
        let count = contract_slot_count_expr(&f.ty);
        next_slot = quote! { (#next_slot) + (#count) };
    }
    SlotLayout {
        slots,
        allocation_order: (0..fields.len()).collect(),
    }
}

/// Emits a const assertion that each field starts at or after the end of the
/// field that the allocator placed before it. An explicit `#[slot(k)]` inside
/// the slots of the previous field does not compile.
fn emit_layout_assertions(fields: &[ContractFieldInfo], layout: &SlotLayout) -> TokenStream2 {
    let checks = layout.allocation_order.windows(2).map(|pair| {
        let (prev, next) = (&fields[pair[0]], &fields[pair[1]]);
        let prev_slot = &layout.slots[pair[0]];
        let next_slot = &layout.slots[pair[1]];
        let prev_count = contract_slot_count_expr(&prev.ty);
        let message = format!(
            "storage field `{}` starts inside the slots of storage field `{}`",
            next.name, prev.name
        );
        quote! { assert!((#next_slot) >= (#prev_slot) + (#prev_count), #message); }
    });
    quote! {
        const _: () = {
            #(#checks)*
        };
    }
}

fn emit_contract(
    input: &DeriveInput,
    config: &ContractConfig,
    fields: &[ContractFieldInfo],
    layout: &SlotLayout,
) -> TokenStream2 {
    let name = &input.ident;
    let vis = &input.vis;
    let field_decls: Vec<_> = fields
        .iter()
        .map(|f| {
            let v = &f.vis;
            let n = &f.name;
            let t = storage_type_with_lifetime(&f.ty);
            quote! { #v #n: #t }
        })
        .collect();

    let struct_def = quote! {
        #vis struct #name<'storage> {
            pub address: ::alloy_primitives::Address,
            pub storage: ::outbe_primitives::storage::StorageHandle<'storage>,
            #(#field_decls,)*
        }
    };

    let field_inits: Vec<_> = fields
        .iter()
        .zip(layout.slots.iter())
        .map(|(f, slot)| emit_field_initializer(f, slot))
        .collect();

    let storage_backed_impl = emit_storage_backed(name, config.address.as_ref());
    let constructor = emit_constructor(name, config.address.as_ref(), &field_inits);
    let layout_assertions = emit_layout_assertions(fields, layout);

    quote! {
        #struct_def
        #constructor
        #storage_backed_impl
        #layout_assertions
    }
}

fn emit_field_initializer(f: &ContractFieldInfo, slot: &TokenStream2) -> TokenStream2 {
    let n = &f.name;
    let ty = &f.ty;
    if is_mapping_type(ty) {
        quote! { #n: ::outbe_primitives::storage::types::Mapping::new(::alloy_primitives::U256::from(#slot), address, storage.clone()) }
    } else if is_slot_type(ty) || is_dsl_value_type(ty) {
        quote! { #n: ::outbe_primitives::storage::types::Slot::new(::alloy_primitives::U256::from(#slot), address, storage.clone()) }
    } else if is_dsl_map_type(ty) {
        quote! { #n: ::outbe_primitives::storage::dsl::Map::new(::alloy_primitives::U256::from(#slot), address, storage.clone()) }
    } else if is_dsl_list_type(ty) {
        quote! { #n: ::outbe_primitives::storage::types::StorageVec::new(::alloy_primitives::U256::from(#slot), address, storage.clone()) }
    } else if is_dsl_set_type(ty) {
        quote! { #n: ::outbe_primitives::storage::types::StorageSet::new(::alloy_primitives::U256::from(#slot), address, storage.clone()) }
    } else if is_dsl_binary_heap_type(ty) {
        quote! { #n: ::outbe_primitives::storage::types::BinaryHeap::new(::alloy_primitives::U256::from(#slot), address, storage.clone()) }
    } else if is_dsl_deque_type(ty) {
        quote! { #n: ::outbe_primitives::storage::types::StorageDeque::new(::alloy_primitives::U256::from(#slot), address, storage.clone()) }
    } else if is_dsl_circular_buffer_type(ty) {
        quote! { #n: ::outbe_primitives::storage::types::StorageCircularBuffer::new(::alloy_primitives::U256::from(#slot), address, storage.clone()) }
    } else if is_storage_array(ty).is_some() {
        quote! { #n: ::outbe_primitives::storage::types::StorageArray::new(::alloy_primitives::U256::from(#slot), address, storage.clone()) }
    } else if let Some((coll_name, _)) = is_storage_collection(ty) {
        let coll_ident = syn::Ident::new(coll_name, proc_macro2::Span::call_site());
        quote! { #n: ::outbe_primitives::storage::types::#coll_ident::new(::alloy_primitives::U256::from(#slot), address, storage.clone()) }
    } else {
        quote! { #n: ::outbe_primitives::storage::types::Slot::new(::alloy_primitives::U256::from(#slot), address, storage.clone()) }
    }
}

fn emit_storage_backed(name: &Ident, address: Option<&Expr>) -> TokenStream2 {
    if let Some(addr) = address {
        quote! {
            impl<'storage> ::outbe_primitives::storage::StorageBacked<'storage> for #name<'storage> {
                const DEFAULT_ADDRESS: ::alloy_primitives::Address = #addr;

                fn at(
                    storage: ::outbe_primitives::storage::StorageHandle<'storage>,
                    address: ::alloy_primitives::Address,
                ) -> Self {
                    #name::at(storage, address)
                }
            }
        }
    } else {
        quote! {}
    }
}

fn emit_constructor(
    name: &Ident,
    address: Option<&Expr>,
    field_inits: &[TokenStream2],
) -> TokenStream2 {
    if let Some(addr) = address {
        quote! {
            impl<'storage> #name<'storage> {
                pub fn new(storage: impl ::core::convert::Into<::outbe_primitives::storage::StorageHandle<'storage>>) -> Self {
                    Self::at(storage, #addr)
                }

                pub fn at(
                    storage: impl ::core::convert::Into<::outbe_primitives::storage::StorageHandle<'storage>>,
                    address: ::alloy_primitives::Address,
                ) -> Self {
                    let storage = storage.into();
                    Self {
                        address,
                        storage: storage.clone(),
                        #(#field_inits,)*
                    }
                }

                pub fn emit<E: ::alloy_sol_types::SolEvent>(&mut self, event: E) -> ::outbe_primitives::error::Result<()> {
                    let log_data = event.encode_log_data();
                    self.storage.emit_event(self.address, log_data)
                }
            }
        }
    } else {
        quote! {
            impl<'storage> #name<'storage> {
                pub fn new(
                    storage: impl ::core::convert::Into<::outbe_primitives::storage::StorageHandle<'storage>>,
                    address: ::alloy_primitives::Address,
                ) -> Self {
                    let storage = storage.into();
                    Self {
                        address,
                        storage: storage.clone(),
                        #(#field_inits,)*
                    }
                }

                pub fn emit<E: ::alloy_sol_types::SolEvent>(&mut self, event: E) -> ::outbe_primitives::error::Result<()> {
                    let log_data = event.encode_log_data();
                    self.storage.emit_event(self.address, log_data)
                }
            }
        }
    }
}
