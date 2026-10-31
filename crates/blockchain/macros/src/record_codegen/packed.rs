//! Placement and shared-word emission for `#[storage_record(packed)]`.

use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::Type;

use super::{
    dynamic_field_kind, is_optional_type, last_type_ident, record_field_inner_storage_type,
    record_field_storage_slots, RecordFieldInfo,
};

#[derive(Clone, Copy)]
pub(super) struct FieldPlacement {
    pub(super) slot_offset: u64,
    pub(super) byte_offset: usize,
    pub(super) packed: bool,
}

pub(super) struct PackedSlotOps {
    pub(super) load_preambles: Vec<TokenStream2>,
    pub(super) writes: Vec<TokenStream2>,
    pub(super) deletes: Vec<TokenStream2>,
}

pub(super) fn compute_placements(
    fields: &[RecordFieldInfo],
    packed: bool,
) -> syn::Result<(Vec<FieldPlacement>, u64)> {
    if packed {
        return compute_packed_placements(fields);
    }
    let offsets = compute_order_based_offsets(
        fields,
        |f| record_field_storage_slots(&f.ty),
        |f| f.attrs.order,
    )?;
    let total = fields
        .iter()
        .zip(offsets.iter())
        .map(|(f, offset)| offset + record_field_storage_slots(&f.ty).unwrap())
        .max()
        .unwrap_or(0);
    let placements = offsets
        .into_iter()
        .map(|slot_offset| FieldPlacement {
            slot_offset,
            byte_offset: 0,
            packed: false,
        })
        .collect();
    Ok((placements, total))
}

fn record_field_byte_width(ty: &Type) -> Option<usize> {
    if is_optional_type(ty) || dynamic_field_kind(ty).is_some() {
        return None;
    }
    let inner = record_field_inner_storage_type(ty);
    match last_type_ident(&inner).as_deref() {
        Some("bool" | "u8") => Some(1),
        Some("u16") => Some(2),
        Some("u32" | "WorldwideDay" | "WorldwideDayKey" | "EmissionDate") => Some(4),
        Some("u64") => Some(8),
        Some("u128") => Some(16),
        Some("Address") => Some(20),
        _ => None,
    }
}

fn compute_packed_placements(
    fields: &[RecordFieldInfo],
) -> syn::Result<(Vec<FieldPlacement>, u64)> {
    let mut indexed: Vec<(usize, u64)> = fields
        .iter()
        .enumerate()
        .map(|(i, f)| (i, f.attrs.order.unwrap_or(i as u64)))
        .collect();
    indexed.sort_by_key(|(_, ord)| *ord);

    let mut placements = vec![
        FieldPlacement {
            slot_offset: 0,
            byte_offset: 0,
            packed: false,
        };
        fields.len()
    ];
    let mut state = (0u64, 0usize);

    for (idx, _) in indexed {
        placements[idx] = place_one_field(&fields[idx], &mut state)?;
    }

    let total_slots = if state.1 > 0 { state.0 + 1 } else { state.0 };
    Ok((placements, total_slots))
}

fn place_one_field(
    field: &RecordFieldInfo,
    state: &mut (u64, usize),
) -> syn::Result<FieldPlacement> {
    let Some(width) = record_field_byte_width(&field.ty) else {
        if state.1 > 0 {
            state.0 += 1;
            state.1 = 0;
        }
        let slot_offset = state.0;
        state.0 += record_field_storage_slots(&field.ty)?;
        return Ok(FieldPlacement {
            slot_offset,
            byte_offset: 0,
            packed: false,
        });
    };

    if state.1 + width > 32 {
        state.0 += 1;
        state.1 = 0;
    }
    let placement = FieldPlacement {
        slot_offset: state.0,
        byte_offset: state.1,
        packed: true,
    };
    state.1 += width;
    if state.1 == 32 {
        state.0 += 1;
        state.1 = 0;
    }
    Ok(placement)
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

pub(super) fn emit_packed_slot_ops(
    non_key_fields: &[RecordFieldInfo],
    non_key_placements: &[FieldPlacement],
    key_ty: &Type,
) -> PackedSlotOps {
    let mut packed_slots: Vec<u64> = Vec::new();
    for placement in non_key_placements {
        if placement.packed && !packed_slots.contains(&placement.slot_offset) {
            packed_slots.push(placement.slot_offset);
        }
    }

    let mut load_preambles = Vec::new();
    let mut writes = Vec::new();
    let mut deletes = Vec::new();

    for slot_offset in packed_slots {
        let word_ident = format_ident!("__packed_word_{}", slot_offset);
        load_preambles.push(quote! {
            let #word_ident = ::outbe_primitives::storage::types::Mapping::<#key_ty, ::alloy_primitives::U256>::new(
                entry.base_slot() + ::alloy_primitives::U256::from(#slot_offset),
                entry.address(),
                entry.storage(),
            ).read(entry.key_ref())?;
        });
        let pack_steps = emit_pack_steps_for_slot(non_key_fields, non_key_placements, slot_offset);
        writes.push(quote! {
            {
                let mut __word = ::alloy_primitives::U256::ZERO;
                #(#pack_steps)*
                ::outbe_primitives::storage::types::Mapping::<#key_ty, ::alloy_primitives::U256>::new(
                    entry.base_slot() + ::alloy_primitives::U256::from(#slot_offset),
                    entry.address(),
                    entry.storage(),
                ).write(entry.key_ref(), __word)?;
            }
        });
        deletes.push(quote! {
            ::outbe_primitives::storage::types::Mapping::<#key_ty, ::alloy_primitives::U256>::new(
                entry.base_slot() + ::alloy_primitives::U256::from(#slot_offset),
                entry.address(),
                entry.storage(),
            ).get(entry.key_ref()).delete()?;
        });
    }

    PackedSlotOps {
        load_preambles,
        writes,
        deletes,
    }
}

fn emit_pack_steps_for_slot(
    non_key_fields: &[RecordFieldInfo],
    non_key_placements: &[FieldPlacement],
    slot_offset: u64,
) -> Vec<TokenStream2> {
    non_key_fields
        .iter()
        .zip(non_key_placements.iter())
        .filter(|(_, p)| p.packed && p.slot_offset == slot_offset)
        .map(|(field, placement)| {
            let fname = &field.name;
            let storage_ty = record_field_inner_storage_type(&field.ty);
            let byte_offset_lit = placement.byte_offset;
            quote! {
                __word = ::outbe_primitives::storage::types::pack_into_word::<#storage_ty>(
                    __word,
                    #byte_offset_lit,
                    &value.#fname,
                );
            }
        })
        .collect()
}
