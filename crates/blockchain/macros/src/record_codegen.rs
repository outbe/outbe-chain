//! Storage-record analysis and per-field token emission.

use super::{
    field_attrs::{parse_common_field_attrs, CommonFieldAttrs},
    is_deprecated_type, is_ident_type, is_optional_type, last_type_ident, type_args,
    unwrap_single_generic_type,
};
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::{
    parse::{Parse, ParseStream},
    spanned::Spanned,
    Data, DeriveInput, Fields, GenericArgument, Ident, Token, Type,
};

#[derive(Default)]
pub(super) struct StorageRecordConfig {
    exists_field: Option<Ident>,
}

impl Parse for StorageRecordConfig {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let mut config = Self::default();
        while !input.is_empty() {
            let ident: Ident = input.parse()?;
            if ident != "exists_field" {
                return Err(syn::Error::new(
                    ident.span(),
                    "expected `exists_field = ident`",
                ));
            }
            input.parse::<Token![=]>()?;
            config.exists_field = Some(input.parse()?);
            if input.is_empty() {
                break;
            }
            input.parse::<Token![,]>()?;
        }
        Ok(config)
    }
}

#[derive(Clone)]
struct RecordFieldInfo {
    vis: syn::Visibility,
    name: Ident,
    ty: Type,
    attrs: CommonFieldAttrs,
}

struct RecordDescription {
    input: DeriveInput,
    fields: Vec<RecordFieldInfo>,
    key_field: RecordFieldInfo,
    non_key_fields: Vec<RecordFieldInfo>,
    non_key_offsets: Vec<u64>,
    total_slots: u64,
    exists_field_ident: Ident,
}

pub(super) fn generate_storage_record(
    input: DeriveInput,
    config: StorageRecordConfig,
) -> syn::Result<TokenStream2> {
    let description = parse_record(input, config)?;
    Ok(emit_record(&description))
}

fn parse_record(input: DeriveInput, config: StorageRecordConfig) -> syn::Result<RecordDescription> {
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
        fields.push(RecordFieldInfo {
            vis: field.vis.clone(),
            name: field.ident.clone().unwrap(),
            ty: field.ty.clone(),
            attrs: parse_common_field_attrs(&field.attrs)?,
        });
    }

    let key_fields: Vec<_> = fields.iter().filter(|f| f.attrs.key).collect();
    if key_fields.len() != 1 {
        return Err(syn::Error::new_spanned(
            name,
            "#[storage_record] requires exactly one #[key] field",
        ));
    }
    let key_field = key_fields[0];
    let key_field = key_field.clone();

    let exists_field_ident = config.exists_field.ok_or_else(|| {
        syn::Error::new_spanned(
            name,
            "#[storage_record(...)] requires `exists_field = field_name`",
        )
    })?;

    let non_key_fields: Vec<_> = fields.iter().filter(|f| !f.attrs.key).cloned().collect();
    let non_key_offsets = compute_order_based_offsets(
        &non_key_fields,
        |f| record_field_storage_slots(&f.ty),
        |f| f.attrs.order,
    )?;
    let total_slots: u64 = non_key_fields
        .iter()
        .zip(non_key_offsets.iter())
        .map(|(f, offset)| offset + record_field_storage_slots(&f.ty).unwrap())
        .max()
        .unwrap_or(0);

    for field in &non_key_fields {
        if is_optional_type(&field.ty) && dynamic_field_kind(&field.ty).is_some() {
            return Err(syn::Error::new_spanned(
                &field.name,
                "optional dynamic String or Vec<u8> record fields are not supported",
            ));
        }
    }
    if !non_key_fields
        .iter()
        .any(|field| field.name == exists_field_ident)
    {
        return Err(syn::Error::new_spanned(
            name,
            format!(
                "exists_field `{}` not found among non-key fields",
                exists_field_ident
            ),
        ));
    }
    Ok(RecordDescription {
        input,
        fields,
        key_field,
        non_key_fields,
        non_key_offsets,
        total_slots,
        exists_field_ident,
    })
}

fn emit_record(description: &RecordDescription) -> TokenStream2 {
    let RecordDescription {
        input,
        fields,
        key_field,
        non_key_fields,
        non_key_offsets,
        total_slots,
        exists_field_ident,
    } = description;
    let name = &input.ident;
    let vis = &input.vis;
    let key_name = &key_field.name;
    let key_ty = &key_field.ty;
    let struct_fields: Vec<_> = fields
        .iter()
        .map(|f| {
            let vis = &f.vis;
            let name = &f.name;
            let ty = &f.ty;
            quote! { #vis #name: #ty }
        })
        .collect();

    let cleaned_struct = quote! {
        #vis struct #name {
            #(#struct_fields,)*
        }
    };

    let with_key_defaults: Vec<_> = fields
        .iter()
        .map(|f| {
            let fname = &f.name;
            if f.attrs.key {
                quote! { #fname: key }
            } else if let Some(default) = &f.attrs.default {
                quote! { #fname: #default }
            } else {
                quote! { #fname: ::core::default::Default::default() }
            }
        })
        .collect();

    let helper_impl = quote! {
        impl #name {
            pub fn with_key(key: #key_ty) -> Self {
                Self {
                    #(#with_key_defaults,)*
                }
            }
        }
    };

    let entry_trait_name = format_ident!("{}EntryExt", name);
    let mut accessor_trait_methods = Vec::new();
    let mut accessor_impl_methods = Vec::new();
    let mut load_fields = Vec::new();
    let mut write_fields = Vec::new();
    let mut delete_fields = Vec::new();
    let mut exists_expr = None;
    for (field, offset) in non_key_fields.iter().zip(non_key_offsets.iter()) {
        let (accessor_trait, accessor_impl) = emit_accessor(field, key_ty, *offset);
        accessor_trait_methods.push(accessor_trait);
        accessor_impl_methods.push(accessor_impl);
        let (load, write, delete) = emit_field_operations(field, key_ty, *offset);
        load_fields.push(load);
        write_fields.push(write);
        delete_fields.push(delete);
        if field.name == *exists_field_ident {
            exists_expr = Some(emit_exists(field, key_ty, *offset));
        }
    }
    let exists_expr = exists_expr.expect("exists field validated during record analysis");
    let record_impl = quote! {
        impl ::outbe_primitives::storage::dsl::StorageRecord for #name {
            type Key = #key_ty;
            const SLOTS: u64 = #total_slots;

            fn key(&self) -> Self::Key {
                self.#key_name.clone()
            }

            fn exists(entry: &::outbe_primitives::storage::dsl::RecordEntry<'_, Self::Key, Self>) -> ::outbe_primitives::error::Result<bool> {
                #exists_expr
            }

            fn load(entry: &::outbe_primitives::storage::dsl::RecordEntry<'_, Self::Key, Self>) -> ::outbe_primitives::error::Result<Option<Self>> {
                if !Self::exists(entry)? {
                    return Ok(None);
                }
                Ok(Some(Self {
                    #(#load_fields,)*
                    #key_name: entry.key(),
                }))
            }

            fn create(entry: &::outbe_primitives::storage::dsl::RecordEntry<'_, Self::Key, Self>, value: &Self) -> ::outbe_primitives::error::Result<()> {
                if Self::exists(entry)? {
                    return Err(::outbe_primitives::storage::dsl::existing_record_err(stringify!(#name)));
                }
                #(#write_fields)*
                Ok(())
            }

            fn update(entry: &::outbe_primitives::storage::dsl::RecordEntry<'_, Self::Key, Self>, value: &Self) -> ::outbe_primitives::error::Result<()> {
                if !Self::exists(entry)? {
                    return Err(::outbe_primitives::storage::dsl::missing_record_err(stringify!(#name)));
                }
                #(#write_fields)*
                Ok(())
            }

            fn delete(entry: &::outbe_primitives::storage::dsl::RecordEntry<'_, Self::Key, Self>) -> ::outbe_primitives::error::Result<()> {
                #(#delete_fields)*
                Ok(())
            }
        }

        pub trait #entry_trait_name<'storage> {
            #(#accessor_trait_methods)*
        }

        impl<'storage> #entry_trait_name<'storage> for ::outbe_primitives::storage::dsl::RecordEntry<'storage, #key_ty, #name> {
            #(#accessor_impl_methods)*
        }
    };

    quote! {
        #cleaned_struct
        #helper_impl
        #record_impl
    }
}

fn emit_mapping(
    field: &RecordFieldInfo,
    key_ty: &Type,
    offset_lit: u64,
) -> (TokenStream2, TokenStream2, TokenStream2) {
    let fname = &field.name;
    let storage_ty = record_field_inner_storage_type(&field.ty);
    let dynamic_kind = dynamic_field_kind(&field.ty);
    match dynamic_kind {
        Some(DynamicFieldKind::String) => {
            let map_new = dynamic_bytes_mapping_new(&format_ident!("entry"), key_ty, offset_lit);
            (
                quote! { #map_new.read_string(entry.key_ref())? },
                // write_if_changed: one read (SLOADs) to skip the full
                // rewrite (SSTOREs, ~50x per slot) when the value is equal.
                quote! { #map_new.get_bytes(entry.key_ref()).write_if_changed(value.#fname.as_bytes())?; },
                quote! { #map_new.get_bytes(entry.key_ref()).clear()?; },
            )
        }
        Some(DynamicFieldKind::VecU8) => {
            let map_new = dynamic_bytes_mapping_new(&format_ident!("entry"), key_ty, offset_lit);
            (
                quote! { #map_new.get_bytes(entry.key_ref()).read()? },
                quote! { #map_new.get_bytes(entry.key_ref()).write_if_changed(&value.#fname)?; },
                quote! { #map_new.get_bytes(entry.key_ref()).clear()?; },
            )
        }
        None => (
            quote! {
                ::outbe_primitives::storage::types::Mapping::<#key_ty, #storage_ty>::new(
                    entry.base_slot() + ::alloy_primitives::U256::from(#offset_lit),
                    entry.address(),
                    entry.storage(),
                ).read(entry.key_ref())?
            },
            quote! {
                ::outbe_primitives::storage::types::Mapping::<#key_ty, #storage_ty>::new(
                    entry.base_slot() + ::alloy_primitives::U256::from(#offset_lit),
                    entry.address(),
                    entry.storage(),
                ).write(entry.key_ref(), value.#fname)?;
            },
            quote! {
                ::outbe_primitives::storage::types::Mapping::<#key_ty, #storage_ty>::new(
                    entry.base_slot() + ::alloy_primitives::U256::from(#offset_lit),
                    entry.address(),
                    entry.storage(),
                ).get(entry.key_ref()).delete()?;
            },
        ),
    }
}

fn emit_accessor(
    field: &RecordFieldInfo,
    key_ty: &Type,
    offset_lit: u64,
) -> (TokenStream2, TokenStream2) {
    let fname = &field.name;
    let storage_ty = record_field_inner_storage_type(&field.ty);
    let dynamic_kind = dynamic_field_kind(&field.ty);
    if is_optional_type(&field.ty) {
        let trait_method = quote! {
            fn #fname(&self) -> ::outbe_primitives::storage::dsl::OptionalField<'storage, #key_ty, #storage_ty>;
        };
        let impl_method = quote! {
            fn #fname(&self) -> ::outbe_primitives::storage::dsl::OptionalField<'storage, #key_ty, #storage_ty> {
                ::outbe_primitives::storage::dsl::OptionalField::new(
                    self.base_slot() + ::alloy_primitives::U256::from(#offset_lit),
                    self.address(),
                    self.storage(),
                    self.key(),
                )
            }
        };
        (trait_method, impl_method)
    } else if dynamic_kind.is_some() {
        let map_new = dynamic_bytes_mapping_new(&format_ident!("self"), key_ty, offset_lit);
        let trait_method = quote! {
            fn #fname(&self) -> ::outbe_primitives::storage::types::StorageBytes<'storage>;
        };
        let impl_method = quote! {
            fn #fname(&self) -> ::outbe_primitives::storage::types::StorageBytes<'storage> {
                #map_new.get_bytes(self.key_ref())
            }
        };
        (trait_method, impl_method)
    } else {
        let trait_method = quote! {
            fn #fname(&self) -> ::outbe_primitives::storage::types::Slot<'storage, #storage_ty>;
        };
        let impl_method = quote! {
            fn #fname(&self) -> ::outbe_primitives::storage::types::Slot<'storage, #storage_ty> {
                ::outbe_primitives::storage::types::Mapping::<#key_ty, #storage_ty>::new(
                    self.base_slot() + ::alloy_primitives::U256::from(#offset_lit),
                    self.address(),
                    self.storage(),
                ).get(self.key_ref())
            }
        };
        (trait_method, impl_method)
    }
}

fn emit_field_operations(
    field: &RecordFieldInfo,
    key_ty: &Type,
    offset_lit: u64,
) -> (TokenStream2, TokenStream2, TokenStream2) {
    let fname = &field.name;
    let storage_ty = record_field_inner_storage_type(&field.ty);
    let (mapping_read, mapping_write, mapping_delete) = emit_mapping(field, key_ty, offset_lit);
    let load = if is_optional_type(&field.ty) {
        quote! { #fname: ::outbe_primitives::storage::dsl::OptionalField::<#key_ty, #storage_ty>::new(
            entry.base_slot() + ::alloy_primitives::U256::from(#offset_lit),
            entry.address(),
            entry.storage(),
            entry.key(),
        ).read()? }
    } else {
        quote! { #fname: #mapping_read }
    };

    let write = if is_optional_type(&field.ty) {
        quote! {
            ::outbe_primitives::storage::dsl::OptionalField::<#key_ty, #storage_ty>::new(
                entry.base_slot() + ::alloy_primitives::U256::from(#offset_lit),
                entry.address(),
                entry.storage(),
                entry.key(),
            ).write(value.#fname)?;
        }
    } else {
        mapping_write
    };

    let delete = if is_optional_type(&field.ty) {
        quote! {
            ::outbe_primitives::storage::dsl::OptionalField::<#key_ty, #storage_ty>::new(
                entry.base_slot() + ::alloy_primitives::U256::from(#offset_lit),
                entry.address(),
                entry.storage(),
                entry.key(),
            ).delete()?;
        }
    } else {
        mapping_delete
    };

    (load, write, delete)
}

fn emit_exists(field: &RecordFieldInfo, key_ty: &Type, offset_lit: u64) -> TokenStream2 {
    let storage_ty = record_field_inner_storage_type(&field.ty);
    let dynamic_kind = dynamic_field_kind(&field.ty);
    let (mapping_read, _, _) = emit_mapping(field, key_ty, offset_lit);
    if dynamic_kind.is_some() {
        let map_new = dynamic_bytes_mapping_new(&format_ident!("entry"), key_ty, offset_lit);
        quote! { Ok(!#map_new.get_bytes(entry.key_ref()).is_empty()?) }
    } else if is_optional_type(&field.ty) {
        quote! {
            Ok(::outbe_primitives::storage::dsl::OptionalField::<#key_ty, #storage_ty>::new(
                entry.base_slot() + ::alloy_primitives::U256::from(#offset_lit),
                entry.address(),
                entry.storage(),
                entry.key(),
            ).read()?.is_some())
        }
    } else {
        quote! {
            let value = #mapping_read;
            Ok(!<#storage_ty as ::outbe_primitives::storage::types::Storable>::to_word(&value).is_zero())
        }
    }
}

fn record_field_storage_slots(ty: &Type) -> syn::Result<u64> {
    if is_optional_type(ty) {
        return Ok(2);
    }
    if is_deprecated_type(ty) {
        let inner = unwrap_single_generic_type(ty)
            .ok_or_else(|| syn::Error::new(ty.span(), "Deprecated<T> requires one type arg"))?;
        return record_field_storage_slots(&inner);
    }
    Ok(1)
}

fn record_field_inner_storage_type(ty: &Type) -> Type {
    if is_optional_type(ty) || is_deprecated_type(ty) {
        return unwrap_single_generic_type(ty).unwrap_or_else(|| ty.clone());
    }
    ty.clone()
}

fn is_string_type(ty: &Type) -> bool {
    is_ident_type(ty, &["String"])
}

fn is_vec_u8_type(ty: &Type) -> bool {
    if last_type_ident(ty).as_deref() != Some("Vec") {
        return false;
    }
    let Some(args) = type_args(ty) else {
        return false;
    };
    matches!(args.first(), Some(GenericArgument::Type(inner)) if is_ident_type(inner, &["u8"]))
}

enum DynamicFieldKind {
    String,
    VecU8,
}

fn dynamic_field_kind(ty: &Type) -> Option<DynamicFieldKind> {
    let inner = record_field_inner_storage_type(ty);
    if is_string_type(&inner) {
        Some(DynamicFieldKind::String)
    } else if is_vec_u8_type(&inner) {
        Some(DynamicFieldKind::VecU8)
    } else {
        None
    }
}

fn dynamic_bytes_mapping_new(
    receiver: &Ident,
    key_ty: &Type,
    offset_lit: u64,
) -> proc_macro2::TokenStream {
    quote! {
        ::outbe_primitives::storage::types::Mapping::<#key_ty, ::outbe_primitives::storage::types::StorageBytes>::new(
            #receiver.base_slot() + ::alloy_primitives::U256::from(#offset_lit),
            #receiver.address(),
            #receiver.storage(),
        )
    }
}

fn compute_order_based_offsets<T, FSlot, FOrder>(
    items: &[T],
    slot_count: FSlot,
    order: FOrder,
) -> syn::Result<Vec<u64>>
where
    FSlot: Fn(&T) -> syn::Result<u64>,
    FOrder: Fn(&T) -> Option<u64>,
{
    let mut indexed: Vec<(usize, u64, u64)> = items
        .iter()
        .enumerate()
        .map(|(i, item)| Ok((i, order(item).unwrap_or(i as u64), slot_count(item)?)))
        .collect::<syn::Result<_>>()?;

    indexed.sort_by_key(|(_, ord, _)| *ord);

    let mut offsets = vec![0u64; items.len()];
    let mut next = 0u64;
    for (idx, _, slots) in indexed {
        offsets[idx] = next;
        next += slots;
    }
    Ok(offsets)
}
