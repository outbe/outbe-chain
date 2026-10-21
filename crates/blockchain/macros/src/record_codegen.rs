//! Storage-record analysis and per-field token emission.

mod packed;

use super::{
    field_attrs::{parse_common_field_attrs, CommonFieldAttrs},
    is_deprecated_type, is_ident_type, is_optional_type, last_type_ident, type_args,
    unwrap_single_generic_type,
};
use packed::{compute_placements, emit_packed_slot_ops, FieldPlacement};
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
    packed: bool,
}

impl Parse for StorageRecordConfig {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let mut config = Self::default();
        while !input.is_empty() {
            let ident: Ident = input.parse()?;
            if ident == "packed" {
                config.packed = true;
            } else if ident == "exists_field" {
                input.parse::<Token![=]>()?;
                config.exists_field = Some(input.parse()?);
            } else {
                return Err(syn::Error::new(
                    ident.span(),
                    "expected `exists_field = ident` or `packed`",
                ));
            }
            if input.is_empty() {
                break;
            }
            input.parse::<Token![,]>()?;
        }
        Ok(config)
    }
}

#[derive(Clone)]
pub(super) struct RecordFieldInfo {
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
    non_key_placements: Vec<FieldPlacement>,
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
    let fields = extract_named_fields(&input)?;
    let key_field = find_single_key_field(name, &fields)?;

    let exists_field_ident = config.exists_field.ok_or_else(|| {
        syn::Error::new_spanned(
            name,
            "#[storage_record(...)] requires `exists_field = field_name`",
        )
    })?;

    let non_key_fields: Vec<_> = fields.iter().filter(|f| !f.attrs.key).cloned().collect();
    let (non_key_placements, total_slots) = compute_placements(&non_key_fields, config.packed)?;
    validate_non_key_fields(name, &non_key_fields, &exists_field_ident)?;

    Ok(RecordDescription {
        input,
        fields,
        key_field,
        non_key_fields,
        non_key_placements,
        total_slots,
        exists_field_ident,
    })
}

fn extract_named_fields(input: &DeriveInput) -> syn::Result<Vec<RecordFieldInfo>> {
    let name = &input.ident;
    let Data::Struct(data) = &input.data else {
        return Err(syn::Error::new_spanned(name, "only structs are supported"));
    };
    let Fields::Named(f) = &data.fields else {
        return Err(syn::Error::new_spanned(
            name,
            "only structs with named fields are supported",
        ));
    };
    f.named
        .iter()
        .map(|field| {
            Ok(RecordFieldInfo {
                vis: field.vis.clone(),
                name: field.ident.clone().unwrap(),
                ty: field.ty.clone(),
                attrs: parse_common_field_attrs(&field.attrs)?,
            })
        })
        .collect()
}

fn find_single_key_field(name: &Ident, fields: &[RecordFieldInfo]) -> syn::Result<RecordFieldInfo> {
    let key_fields: Vec<_> = fields.iter().filter(|f| f.attrs.key).collect();
    if key_fields.len() != 1 {
        return Err(syn::Error::new_spanned(
            name,
            "#[storage_record] requires exactly one #[key] field",
        ));
    }
    Ok(key_fields[0].clone())
}

fn validate_non_key_fields(
    name: &Ident,
    non_key_fields: &[RecordFieldInfo],
    exists_field_ident: &Ident,
) -> syn::Result<()> {
    for field in non_key_fields {
        if is_optional_type(&field.ty) && dynamic_field_kind(&field.ty).is_some() {
            return Err(syn::Error::new_spanned(
                &field.name,
                "optional dynamic String or Vec<u8> record fields are not supported",
            ));
        }
    }
    if !non_key_fields.iter().any(|f| f.name == *exists_field_ident) {
        return Err(syn::Error::new_spanned(
            name,
            format!(
                "exists_field `{}` not found among non-key fields",
                exists_field_ident
            ),
        ));
    }
    Ok(())
}

fn emit_record(description: &RecordDescription) -> TokenStream2 {
    let name = &description.input.ident;
    let vis = &description.input.vis;
    let key_name = &description.key_field.name;
    let key_ty = &description.key_field.ty;
    let total_slots = &description.total_slots;

    let cleaned_struct = emit_cleaned_struct(vis, name, &description.fields);
    let helper_impl = emit_with_key_impl(name, key_ty, &description.fields);
    let mut packed_ops = emit_packed_slot_ops(
        &description.non_key_fields,
        &description.non_key_placements,
        key_ty,
    );

    let mut accessor_trait_methods = Vec::new();
    let mut accessor_impl_methods = Vec::new();
    let mut load_fields = Vec::new();
    let mut exists_expr = None;

    for (field, &placement) in description
        .non_key_fields
        .iter()
        .zip(description.non_key_placements.iter())
    {
        let (accessor_trait, accessor_impl) = emit_accessor(field, key_ty, placement);
        accessor_trait_methods.push(accessor_trait);
        accessor_impl_methods.push(accessor_impl);
        load_fields.push(append_field_ops(field, key_ty, placement, &mut packed_ops));
        if field.name == description.exists_field_ident {
            exists_expr = Some(emit_exists(field, key_ty, placement));
        }
    }

    let exists_expr = exists_expr.expect("exists field validated during record analysis");
    let packed_load_preambles = packed_ops.load_preambles;
    let write_fields = packed_ops.writes;
    let delete_fields = packed_ops.deletes;
    let entry_trait_name = format_ident!("{}EntryExt", name);

    quote! {
        #cleaned_struct
        #helper_impl

        impl ::outbe_primitives::storage::types::StorableType for #name {
            const SLOTS: usize = #total_slots as usize;
        }

        impl ::outbe_primitives::storage::dsl::StorageRecord for #name {
            type Key = #key_ty;
            const SLOTS: u64 = <Self as ::outbe_primitives::storage::types::StorableType>::SLOTS as u64;

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
                #(#packed_load_preambles)*
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
    }
}

fn emit_cleaned_struct(
    vis: &syn::Visibility,
    name: &Ident,
    fields: &[RecordFieldInfo],
) -> TokenStream2 {
    let struct_fields = fields.iter().map(|f| {
        let fvis = &f.vis;
        let fname = &f.name;
        let fty = &f.ty;
        quote! { #fvis #fname: #fty }
    });
    quote! {
        #vis struct #name {
            #(#struct_fields,)*
        }
    }
}

fn emit_with_key_impl(name: &Ident, key_ty: &Type, fields: &[RecordFieldInfo]) -> TokenStream2 {
    let defaults = fields.iter().map(|f| {
        let fname = &f.name;
        if f.attrs.key {
            quote! { #fname: key }
        } else if let Some(default) = &f.attrs.default {
            quote! { #fname: #default }
        } else {
            quote! { #fname: ::core::default::Default::default() }
        }
    });
    quote! {
        impl #name {
            pub fn with_key(key: #key_ty) -> Self {
                Self {
                    #(#defaults,)*
                }
            }
        }
    }
}

fn append_field_ops(
    field: &RecordFieldInfo,
    key_ty: &Type,
    placement: FieldPlacement,
    packed_ops: &mut packed::PackedSlotOps,
) -> TokenStream2 {
    if placement.packed {
        let fname = &field.name;
        let storage_ty = record_field_inner_storage_type(&field.ty);
        let word_ident = format_ident!("__packed_word_{}", placement.slot_offset);
        let byte_offset_lit = placement.byte_offset;
        quote! {
            #fname: ::outbe_primitives::storage::types::unpack_from_word::<#storage_ty>(
                #word_ident,
                #byte_offset_lit,
            )
        }
    } else {
        let (load, write, delete) = emit_field_operations(field, key_ty, placement.slot_offset);
        packed_ops.writes.push(write);
        packed_ops.deletes.push(delete);
        load
    }
}

fn emit_mapping(
    field: &RecordFieldInfo,
    key_ty: &Type,
    offset_lit: u64,
) -> (TokenStream2, TokenStream2, TokenStream2) {
    let fname = &field.name;
    let storage_ty = record_field_inner_storage_type(&field.ty);
    match dynamic_field_kind(&field.ty) {
        Some(DynamicFieldKind::String) => {
            let map_new = dynamic_bytes_mapping_new(&format_ident!("entry"), key_ty, offset_lit);
            (
                quote! { #map_new.read_string(entry.key_ref())? },
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
        None => {
            let map_new = quote! {
                ::outbe_primitives::storage::types::Mapping::<#key_ty, #storage_ty>::new(
                    entry.base_slot() + ::alloy_primitives::U256::from(#offset_lit),
                    entry.address(),
                    entry.storage(),
                )
            };
            (
                quote! { #map_new.read(entry.key_ref())? },
                quote! { #map_new.write(entry.key_ref(), value.#fname)?; },
                quote! { #map_new.get(entry.key_ref()).delete()?; },
            )
        }
    }
}

fn emit_accessor(
    field: &RecordFieldInfo,
    key_ty: &Type,
    placement: FieldPlacement,
) -> (TokenStream2, TokenStream2) {
    let fname = &field.name;
    let storage_ty = record_field_inner_storage_type(&field.ty);
    let offset_lit = placement.slot_offset;
    let byte_offset_lit = placement.byte_offset;
    if is_optional_type(&field.ty) {
        return (
            quote! {
                fn #fname(&self) -> ::outbe_primitives::storage::dsl::OptionalField<'storage, #key_ty, #storage_ty>;
            },
            quote! {
                fn #fname(&self) -> ::outbe_primitives::storage::dsl::OptionalField<'storage, #key_ty, #storage_ty> {
                    ::outbe_primitives::storage::dsl::OptionalField::new(
                        self.base_slot() + ::alloy_primitives::U256::from(#offset_lit),
                        self.address(),
                        self.storage(),
                        self.key(),
                    )
                }
            },
        );
    }
    if dynamic_field_kind(&field.ty).is_some() {
        let map_new = dynamic_bytes_mapping_new(&format_ident!("self"), key_ty, offset_lit);
        return (
            quote! {
                fn #fname(&self) -> ::outbe_primitives::storage::types::StorageBytes<'storage>;
            },
            quote! {
                fn #fname(&self) -> ::outbe_primitives::storage::types::StorageBytes<'storage> {
                    #map_new.get_bytes(self.key_ref())
                }
            },
        );
    }
    let get_expr = if placement.packed {
        quote! { get_with_offset(self.key_ref(), #byte_offset_lit) }
    } else {
        quote! { get(self.key_ref()) }
    };
    (
        quote! {
            fn #fname(&self) -> ::outbe_primitives::storage::types::Slot<'storage, #storage_ty>;
        },
        quote! {
            fn #fname(&self) -> ::outbe_primitives::storage::types::Slot<'storage, #storage_ty> {
                ::outbe_primitives::storage::types::Mapping::<#key_ty, #storage_ty>::new(
                    self.base_slot() + ::alloy_primitives::U256::from(#offset_lit),
                    self.address(),
                    self.storage(),
                ).#get_expr
            }
        },
    )
}

fn emit_field_operations(
    field: &RecordFieldInfo,
    key_ty: &Type,
    offset_lit: u64,
) -> (TokenStream2, TokenStream2, TokenStream2) {
    let fname = &field.name;
    if is_optional_type(&field.ty) {
        let storage_ty = record_field_inner_storage_type(&field.ty);
        let opt_new = quote! {
            ::outbe_primitives::storage::dsl::OptionalField::<#key_ty, #storage_ty>::new(
                entry.base_slot() + ::alloy_primitives::U256::from(#offset_lit),
                entry.address(),
                entry.storage(),
                entry.key(),
            )
        };
        return (
            quote! { #fname: #opt_new.read()? },
            quote! { #opt_new.write(value.#fname)?; },
            quote! { #opt_new.delete()?; },
        );
    }
    let (mapping_read, mapping_write, mapping_delete) = emit_mapping(field, key_ty, offset_lit);
    (
        quote! { #fname: #mapping_read },
        mapping_write,
        mapping_delete,
    )
}

fn emit_exists(field: &RecordFieldInfo, key_ty: &Type, placement: FieldPlacement) -> TokenStream2 {
    let storage_ty = record_field_inner_storage_type(&field.ty);
    let offset_lit = placement.slot_offset;
    let byte_offset_lit = placement.byte_offset;
    if dynamic_field_kind(&field.ty).is_some() {
        let map_new = dynamic_bytes_mapping_new(&format_ident!("entry"), key_ty, offset_lit);
        return quote! { Ok(!#map_new.get_bytes(entry.key_ref()).is_empty()?) };
    }
    if is_optional_type(&field.ty) {
        return quote! {
            Ok(::outbe_primitives::storage::dsl::OptionalField::<#key_ty, #storage_ty>::new(
                entry.base_slot() + ::alloy_primitives::U256::from(#offset_lit),
                entry.address(),
                entry.storage(),
                entry.key(),
            ).read()?.is_some())
        };
    }
    let read_expr = if placement.packed {
        quote! {
            ::outbe_primitives::storage::types::Mapping::<#key_ty, #storage_ty>::new(
                entry.base_slot() + ::alloy_primitives::U256::from(#offset_lit),
                entry.address(),
                entry.storage(),
            ).get_with_offset(entry.key_ref(), #byte_offset_lit).read()?
        }
    } else {
        let (mapping_read, _, _) = emit_mapping(field, key_ty, offset_lit);
        mapping_read
    };
    quote! {
        let value = #read_expr;
        Ok(!<#storage_ty as ::outbe_primitives::storage::types::Storable>::to_word(&value).is_zero())
    }
}

pub(super) fn record_field_storage_slots(ty: &Type) -> syn::Result<u64> {
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

pub(super) fn record_field_inner_storage_type(ty: &Type) -> Type {
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

pub(super) enum DynamicFieldKind {
    String,
    VecU8,
}

pub(super) fn dynamic_field_kind(ty: &Type) -> Option<DynamicFieldKind> {
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
