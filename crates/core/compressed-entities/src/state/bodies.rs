//! Body identities, first-touch gas and pending-body journal transitions.

use super::*;

impl<'storage> State<'storage> {
    pub(crate) fn pending(
        &self,
        collection: Collection,
        entity_id: WwdEntityId,
    ) -> Result<(B256, PendingWord, Vec<u8>)> {
        self.validate_schema_for_read()?;
        let locator = body_locator(collection, entity_id)?;
        let schema = self.schema();
        let pending = PendingWord::decode(schema.pending_word.read(&locator)?)?;
        let body = schema.pending_body.get_bytes(&locator).read()?;
        let identity_record = self.read_body_identity(locator)?;

        match pending {
            PendingWord::Untouched => {
                if !body.is_empty() || identity_record.is_some() {
                    return Err(fatal(
                        "untouched compressed-entity locator has residual dynamic state",
                    ));
                }
            }
            PendingWord::Set(_) => {
                self.validate_body_record(locator, collection, entity_id, identity_record)?;
                if body.is_empty() {
                    return Err(fatal("compressed-entity Set entry has no pending body"));
                }
            }
            PendingWord::Deleted => {
                self.validate_body_record(locator, collection, entity_id, identity_record)?;
                if !body.is_empty() {
                    return Err(fatal(
                        "compressed-entity Deleted entry retains pending body",
                    ));
                }
            }
        }
        Ok((locator, pending, body))
    }
    pub(super) fn validate_body_record(
        &self,
        locator: B256,
        collection: Collection,
        entity_id: WwdEntityId,
        stored: Option<(Collection, WwdEntityId)>,
    ) -> Result<()> {
        let Some((stored_collection, stored_id)) = stored else {
            return Err(fatal(
                "compressed-entity body identity record/hash mismatch",
            ));
        };
        if stored_collection != collection
            || stored_id != entity_id
            || body_locator(stored_collection, stored_id)? != locator
        {
            return Err(fatal(
                "compressed-entity body identity record/hash mismatch",
            ));
        }
        Ok(())
    }
    /// The identity at `locator`, or `None` when the locator is untouched.
    /// The collection byte, not a zero identity, carries absence:
    /// `Collection::from_id` rejects 0, and a zero identity is a representable
    /// value rather than a sentinel.
    pub(super) fn read_body_identity(
        &self,
        locator: B256,
    ) -> Result<Option<(Collection, WwdEntityId)>> {
        let schema = self.schema();
        let collection_id = schema.body_identity_collection.read(&locator)?;
        if collection_id == 0 {
            return Ok(None);
        }
        let collection = Collection::from_id(collection_id)?;
        Ok(Some((collection, schema.body_identity.read(&locator)?)))
    }
    /// Writes both words. A half-written pair decodes as a different identity
    /// rather than a missing one, so callers holding a checkpoint must keep
    /// this inside it.
    pub(super) fn write_body_identity(
        &self,
        locator: B256,
        collection: Collection,
        entity_id: WwdEntityId,
    ) -> Result<()> {
        let schema = self.schema();
        schema.body_identity.write(&locator, entity_id)?;
        schema
            .body_identity_collection
            .write(&locator, collection.id())
    }
    pub(super) fn clear_body_identity(&self, locator: B256) -> Result<()> {
        let schema = self.schema();
        schema.body_identity.write(&locator, WwdEntityId::ZERO)?;
        schema.body_identity_collection.write(&locator, 0)
    }
    pub(super) fn has_body_identity(&self, locator: B256) -> Result<bool> {
        Ok(self.schema().body_identity_collection.read(&locator)? != 0)
    }
    pub(crate) fn prepare_body_touch(
        &self,
        scope: &ExecutionScope,
        collection: Collection,
        entity_id: WwdEntityId,
    ) -> Result<B256> {
        let (locator, pending, _) = self.pending(collection, entity_id)?;
        let entity = match collection {
            Collection::Tribute => EntityRef::Tribute(entity_id),
            Collection::NodItem => EntityRef::NodItem(entity_id),
            Collection::NodBucket => EntityRef::NodBucket(entity_id),
            Collection::Gem => EntityRef::Gem(entity_id),
            Collection::GemBucket => EntityRef::GemBucket(entity_id),
        };
        let domain = match collection {
            Collection::Tribute => CeDomain::Tribute,
            Collection::NodItem => CeDomain::NodItem,
            Collection::NodBucket => CeDomain::NodBucket,
            Collection::Gem => CeDomain::Gem,
            Collection::GemBucket => CeDomain::GemBucket,
        };
        let collection_key = collection_key(domain, entity_id).map_err(collection_revert)?;
        if !self
            .schema()
            .pending_retirement
            .read(&B256::from(*collection_key.as_bytes()))?
            .is_zero()
        {
            return Err(revert(
                "compressed-entity mutation conflicts with pending collection retirement",
            ));
        }
        // The transaction footprint includes keys already touched by an
        // earlier transaction in this block, even though they need no second
        // block-level sealing reservation.
        scope.reserve_unique_key_work(entity)?;
        if pending == PendingWord::Untouched {
            // Reserve deterministic deferred sealing/persistence work before
            // the first overlay write or event for this key.
            // Reserve cleanup before any corresponding state or event write.
            let schema = self.schema();
            let cleanup_gas = FIRST_BODY_TOUCH_CLEANUP_GAS
                + if schema.touched.is_empty()? {
                    BODY_TOUCHED_LENGTH_CLEANUP_GAS
                } else {
                    0
                };
            scope.deduct_explicit_gas(&self.storage, cleanup_gas)?;
            self.write_body_identity(locator, collection, entity_id)?;
            schema.touched.push(locator)?;
        }
        Ok(locator)
    }
    pub(crate) fn set_pending_prepared(
        &self,
        locator: B256,
        commitment: Commitment,
        stored_body: &[u8],
    ) -> Result<()> {
        self.ensure_schema()?;
        let schema = self.schema();
        if !self.has_body_identity(locator)? {
            return Err(fatal("pending Set was not prepared by first-touch logic"));
        }
        schema.pending_body.get_bytes(&locator).write(stored_body)?;
        schema
            .pending_word
            .write(&locator, PendingWord::Set(commitment).encode())
    }
    pub(crate) fn set_deleted_prepared(&self, locator: B256) -> Result<()> {
        self.ensure_schema()?;
        let schema = self.schema();
        if !self.has_body_identity(locator)? {
            return Err(fatal(
                "pending Deleted was not prepared by first-touch logic",
            ));
        }
        schema.pending_body.get_bytes(&locator).clear()?;
        schema
            .pending_word
            .write(&locator, PendingWord::Deleted.encode())
    }
    pub(crate) fn final_body_mutations(
        &self,
    ) -> Result<Vec<(Collection, WwdEntityId, Option<Commitment>)>> {
        self.validate_schema_for_read()?;
        let schema = self.schema();
        let locators = schema.touched.read_all()?;
        let mut unique_locators = BTreeSet::new();
        let mut unique_identities = BTreeSet::new();
        let mut mutations = Vec::with_capacity(locators.len());
        for locator in locators {
            if !unique_locators.insert(locator) {
                return Err(fatal("duplicate compressed-entity touched body locator"));
            }
            let (collection, entity_id) = self
                .read_body_identity(locator)?
                .ok_or_else(|| fatal("touched compressed-entity body has no identity"))?;
            if body_locator(collection, entity_id)? != locator {
                return Err(fatal("compressed-entity touched body locator mismatch"));
            }
            if !unique_identities.insert((collection.id(), entity_id)) {
                return Err(fatal("duplicate compressed-entity touched identity"));
            }
            let final_leaf = match PendingWord::decode(schema.pending_word.read(&locator)?)? {
                PendingWord::Set(commitment) => Some(commitment),
                PendingWord::Deleted => None,
                PendingWord::Untouched => {
                    return Err(fatal("touched compressed-entity body is Untouched"));
                }
            };
            mutations.push((collection, entity_id, final_leaf));
        }
        Ok(mutations)
    }
}
