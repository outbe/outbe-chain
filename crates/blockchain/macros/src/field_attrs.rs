//! Parsing shared storage field markers.

use syn::{Expr, LitBool, LitInt};

#[derive(Default, Clone)]
pub(super) struct CommonFieldAttrs {
    pub(super) explicit_slot: Option<u64>,
    pub(super) order: Option<u64>,
    pub(super) default: Option<Expr>,
    pub(super) deprecated: bool,
    pub(super) key: bool,
}

pub(super) fn parse_common_field_attrs(attrs: &[syn::Attribute]) -> syn::Result<CommonFieldAttrs> {
    let mut out = CommonFieldAttrs::default();
    for attr in attrs {
        if attr.path().is_ident("slot") {
            let lit: LitInt = attr.parse_args()?;
            out.explicit_slot = Some(lit.base10_parse()?);
            continue;
        }
        if attr.path().is_ident("key") {
            out.key = true;
            continue;
        }
        if attr.path().is_ident("attribute") {
            attr.parse_nested_meta(|meta| parse_attribute_key(meta, &mut out))?;
        }
    }
    Ok(out)
}

fn parse_attribute_key(
    meta: syn::meta::ParseNestedMeta<'_>,
    out: &mut CommonFieldAttrs,
) -> syn::Result<()> {
    if meta.path.is_ident("order") {
        let lit: LitInt = meta.value()?.parse()?;
        out.order = Some(lit.base10_parse()?);
        return Ok(());
    }
    if meta.path.is_ident("default") {
        out.default = Some(meta.value()?.parse()?);
        return Ok(());
    }
    if meta.path.is_ident("deprecated") {
        let lit: LitBool = meta.value()?.parse()?;
        out.deprecated = lit.value;
        return Ok(());
    }
    Err(meta.error("unsupported key in #[attribute(...)]"))
}
