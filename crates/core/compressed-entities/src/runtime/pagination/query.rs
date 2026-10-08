use super::*;

pub(super) fn validate_page_request(query: QueryRef, request: IdPageRequest) -> Result<()> {
    if request.limit == 0 || request.limit > MAX_ID_PAGE_LIMIT {
        return Err(revert(format!(
            "page limit must be in 1..={MAX_ID_PAGE_LIMIT}"
        )));
    }
    if let (QueryRef::TributeByDay(day), Some(after)) = (query, request.after) {
        if after.worldwide_day() != day {
            return Err(revert("TributeByDay cursor has the wrong day prefix"));
        }
    }
    Ok(())
}

pub(super) fn validate_parent_page(
    query: QueryRef,
    after: Option<WwdEntityId>,
    limit: u32,
    page: &crate::IdPage,
) -> Result<()> {
    if page.ids.len() > limit as usize {
        return Err(PrecompileError::BodyReadCorruption(
            "parent page exceeds requested limit".into(),
        ));
    }
    let mut previous = after;
    for id in &page.ids {
        if previous.is_some_and(|value| *id <= value) {
            return Err(PrecompileError::BodyReadCorruption(
                "parent IDs are not strictly ascending after the cursor".into(),
            ));
        }
        if let QueryRef::TributeByDay(day) = query {
            if id.worldwide_day() != day {
                return Err(PrecompileError::BodyReadCorruption(
                    "parent TributeByDay ID has the wrong day prefix".into(),
                ));
            }
        }
        previous = Some(*id);
    }
    match page.next_after {
        Some(next) if page.ids.last().copied() != Some(next) => {
            Err(PrecompileError::BodyReadCorruption(
                "parent next_after must equal its last returned ID".into(),
            ))
        }
        Some(_) if page.ids.is_empty() => Err(PrecompileError::BodyReadCorruption(
            "empty parent page cannot advertise a continuation".into(),
        )),
        _ => Ok(()),
    }
}

pub(super) fn merged_candidates(
    parent: &BTreeSet<WwdEntityId>,
    added: &BTreeSet<WwdEntityId>,
    removed: &BTreeSet<WwdEntityId>,
) -> Vec<WwdEntityId> {
    parent
        .difference(removed)
        .copied()
        .chain(added.iter().copied())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

pub(super) fn record_matches_query(record: &IndexRecord, query: QueryRef) -> bool {
    match query {
        QueryRef::TributeByOwner(owner) => {
            record.kind == IndexKind::TributeByOwner && record.partition == owner.as_slice()
        }
        QueryRef::TributeByDay(day) => {
            record.kind == IndexKind::TributeByDay && record.partition == day.value().to_be_bytes()
        }
        QueryRef::NodByOwner(owner) => {
            record.kind == IndexKind::NodByOwner && record.partition == owner.as_slice()
        }
        QueryRef::NodAll => record.kind == IndexKind::NodAll && record.partition.is_empty(),
    }
}

pub(super) fn verified_matches_query(body: &VerifiedBody, query: QueryRef) -> bool {
    if let Some(tribute) = body.payload().as_encrypted_tribute() {
        return match query {
            QueryRef::TributeByOwner(owner) => tribute.context.owner == owner,
            QueryRef::TributeByDay(day) => tribute.context.worldwide_day == day,
            _ => false,
        };
    }
    if let Some(item) = body.payload().as_encrypted_nod_item() {
        return match query {
            QueryRef::NodByOwner(owner) => item.encrypted.terms.owner == owner,
            QueryRef::NodAll => true,
            _ => false,
        };
    }
    match query {
        QueryRef::TributeByOwner(owner) => body
            .payload()
            .as_tribute()
            .is_some_and(|tribute| tribute.owner == owner),
        QueryRef::TributeByDay(day) => body
            .payload()
            .as_tribute()
            .is_some_and(|tribute| tribute.worldwide_day == day),
        QueryRef::NodByOwner(owner) => body
            .payload()
            .as_nod_item()
            .is_some_and(|item| item.owner == owner),
        QueryRef::NodAll => body.payload().as_nod_item().is_some(),
    }
}

pub(super) fn entity_for_query(query: QueryRef, id: WwdEntityId) -> EntityRef {
    match query {
        QueryRef::TributeByOwner(_) | QueryRef::TributeByDay(_) => EntityRef::Tribute(id),
        QueryRef::NodByOwner(_) | QueryRef::NodAll => EntityRef::NodItem(id),
    }
}
