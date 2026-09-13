use super::*;

pub(super) const FIRST_TRIBUTE_CLEANUP_GAS: u64 = FIRST_BODY_TOUCH_CLEANUP_GAS
    + BODY_TOUCHED_LENGTH_CLEANUP_GAS
    + 2 * FIRST_INDEX_TOUCH_CLEANUP_GAS
    + INDEX_TOUCHED_LENGTH_CLEANUP_GAS;

#[derive(Debug, Default)]
pub(super) struct TestAuthenticatedTree(Mutex<HashMap<EntityRef, crate::Commitment>>);

impl TestAuthenticatedTree {
    pub(super) fn insert(&self, entity: EntityRef, commitment: crate::Commitment) {
        self.0.lock().unwrap().insert(entity, commitment);
    }
}

impl AuthenticatedParentTree for TestAuthenticatedTree {
    fn parent_block_hash(&self) -> B256 {
        B256::ZERO
    }

    fn parent_root(&self) -> B256 {
        crate::sealed_root(B256::ZERO).unwrap()
    }

    fn read_leaf_verified(
        &self,
        entity: EntityRef,
        expected_parent_root: B256,
    ) -> Result<Option<crate::Commitment>> {
        assert_eq!(
            expected_parent_root,
            crate::sealed_root(B256::ZERO).unwrap()
        );
        Ok(self.0.lock().unwrap().get(&entity).copied())
    }

    fn partition_present_verified(
        &self,
        _partition: PartitionRef,
        _expected_parent_root: B256,
    ) -> Result<bool> {
        Ok(false)
    }

    fn prepare_seal(
        &self,
        block_number: u64,
        _mutations: &[FinalLeafMutation],
        _retirements: &[PartitionRef],
    ) -> Result<ProvisionalTreeBatch> {
        ProvisionalTreeBatch::new_fixture_single_collection(
            block_number,
            B256::ZERO,
            B256::ZERO,
            B256::ZERO,
            Default::default(),
            Default::default(),
        )
        .map_err(|error| PrecompileError::Fatal(error.to_string()))
    }
}

pub(super) fn scope_with_tree(tree: Arc<TestAuthenticatedTree>) -> ExecutionScope {
    ExecutionScope::with_parent_tree(tree, CeWorkConfig::new(0, 0, u64::MAX))
}

pub(super) fn overlay_leaf(
    storage: StorageHandle<'_>,
    collection: Collection,
    id: WwdEntityId,
) -> Option<crate::Commitment> {
    match State::new(storage).pending(collection, id).unwrap().1 {
        PendingWord::Set(commitment) => Some(commitment),
        PendingWord::Untouched | PendingWord::Deleted => None,
    }
}

#[derive(Default)]
pub(super) struct MemoryParent {
    pub(super) bodies: HashMap<EntityRef, StoredBody>,
    pub(super) tribute_by_owner: HashMap<Address, Vec<WwdEntityId>>,
    pub(super) tribute_by_day: HashMap<WorldwideDay, Vec<WwdEntityId>>,
    pub(super) nod_by_owner: HashMap<Address, Vec<WwdEntityId>>,
    pub(super) nod_all: Vec<WwdEntityId>,
    pub(super) get_calls: Cell<u32>,
    pub(super) list_calls: Cell<u32>,
    pub(super) reverse_pages: bool,
}

impl MemoryParent {
    pub(super) fn insert_tribute(&mut self, body: &TributeBodyV1) -> StoredBody {
        let stored = stored_tribute(body);
        self.bodies
            .insert(EntityRef::Tribute(body.tribute_id), stored.clone());
        self.tribute_by_owner
            .entry(body.owner)
            .or_default()
            .push(body.tribute_id);
        self.tribute_by_day
            .entry(body.worldwide_day)
            .or_default()
            .push(body.tribute_id);
        self.sort_indexes();
        stored
    }

    pub(super) fn insert_nod_item(&mut self, body: &NodItemBodyV1) -> StoredBody {
        let stored = stored_nod_item(body);
        self.bodies
            .insert(EntityRef::NodItem(body.nod_id), stored.clone());
        self.nod_by_owner
            .entry(body.owner)
            .or_default()
            .push(body.nod_id);
        self.nod_all.push(body.nod_id);
        self.sort_indexes();
        stored
    }

    fn sort_indexes(&mut self) {
        for ids in self.tribute_by_owner.values_mut() {
            ids.sort_unstable();
        }
        for ids in self.tribute_by_day.values_mut() {
            ids.sort_unstable();
        }
        for ids in self.nod_by_owner.values_mut() {
            ids.sort_unstable();
        }
        self.nod_all.sort_unstable();
    }

    fn ids(&self, query: QueryRef) -> Vec<WwdEntityId> {
        match query {
            QueryRef::TributeByOwner(owner) => self
                .tribute_by_owner
                .get(&owner)
                .cloned()
                .unwrap_or_default(),
            QueryRef::TributeByDay(day) => {
                self.tribute_by_day.get(&day).cloned().unwrap_or_default()
            }
            QueryRef::NodByOwner(owner) => {
                self.nod_by_owner.get(&owner).cloned().unwrap_or_default()
            }
            QueryRef::NodAll => self.nod_all.clone(),
        }
    }
}

impl ParentBodySource for MemoryParent {
    fn get(
        &self,
        entity: EntityRef,
    ) -> core::result::Result<Option<StoredBody>, ParentBodySourceError> {
        self.get_calls.set(self.get_calls.get() + 1);
        Ok(self.bodies.get(&entity).cloned())
    }

    fn list(
        &self,
        query: QueryRef,
        request: IdPageRequest,
    ) -> core::result::Result<IdPage, ParentBodySourceError> {
        self.list_calls.set(self.list_calls.get() + 1);
        let mut ids = self.ids(query);
        if self.reverse_pages {
            ids.reverse();
        }
        let start = request
            .after
            .map_or(0, |after| ids.partition_point(|id| *id <= after));
        let end = (start + request.limit as usize).min(ids.len());
        let page_ids = ids[start..end].to_vec();
        let next_after = (end < ids.len()).then(|| *page_ids.last().expect("non-empty page"));
        Ok(IdPage {
            ids: page_ids,
            next_after,
        })
    }
}

pub(super) fn entity(day: u32, suffix: u8) -> WwdEntityId {
    WwdEntityId::from_day_and_digest(WorldwideDay::new(day), [suffix; 32])
}

pub(super) fn tribute(id: WwdEntityId, owner: Address, price: u64) -> TributeBodyV1 {
    TributeBodyV1 {
        tribute_id: id,
        owner,
        worldwide_day: id.worldwide_day(),
        issuance_amount_minor: U256::from(10),
        issuance_currency: 840,
        nominal_amount_minor: U256::from(20),
        reference_currency: 978,
        tribute_price_minor: U256::from(price),
        exclude_from_intex_issuance: false,
    }
}

pub(super) fn nod_item(id: WwdEntityId, owner: Address) -> NodItemBodyV1 {
    NodItemBodyV1 {
        nod_id: id,
        owner,
        gratis_load_minor: U256::from(1),
        worldwide_day: id.worldwide_day(),
        league_id: 3,
        floor_price_minor: U256::from(4),
        bucket_key: B256::repeat_byte(5),
        issuance_currency: 840,
        reference_currency: 978,
        issued_at: 7,
    }
}

pub(super) fn stored_tribute(body: &TributeBodyV1) -> StoredBody {
    StoredBody::new_v1(encode_tribute_v1(body).unwrap()).unwrap()
}

fn stored_nod_item(body: &NodItemBodyV1) -> StoredBody {
    StoredBody::new_v1(encode_nod_item_v1(body).unwrap()).unwrap()
}

pub(super) fn tribute_commitment(body: &TributeBodyV1) -> crate::Commitment {
    let payload = encode_tribute_v1(body).unwrap();
    body_commitment(
        ACTIVE_COMMITMENT_SCHEME,
        BODY_SCHEMA_V1,
        body.tribute_id,
        &payload,
    )
    .unwrap()
}

pub(super) fn seed_parent_tribute(
    parent: &mut MemoryParent,
    tree: &TestAuthenticatedTree,
    body: &TributeBodyV1,
) {
    parent.insert_tribute(body);
    tree.insert(
        EntityRef::Tribute(body.tribute_id),
        tribute_commitment(body),
    );
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum FixtureBody {
    Tribute(TributeBodyV1),
    NodItem(NodItemBodyV1),
    NodBucket(NodBucketBodyV1),
}

impl FixtureBody {
    pub(super) fn input(&self) -> BodyInput<'_> {
        match self {
            Self::Tribute(body) => BodyInput::Tribute(body),
            Self::NodItem(body) => BodyInput::NodItem(body),
            Self::NodBucket(body) => BodyInput::NodBucket(body),
        }
    }

    pub(super) fn entity_ref(&self) -> EntityRef {
        match self {
            Self::Tribute(body) => EntityRef::Tribute(body.tribute_id),
            Self::NodItem(body) => EntityRef::NodItem(body.nod_id),
            Self::NodBucket(body) => EntityRef::NodBucket(body.entity_id()),
        }
    }

    pub(super) fn assert_verified(&self, verified: &VerifiedBody) {
        match self {
            Self::Tribute(expected) => {
                assert_eq!(verified.payload().as_tribute(), Some(expected));
            }
            Self::NodItem(expected) => {
                assert_eq!(verified.payload().as_nod_item(), Some(expected));
            }
            Self::NodBucket(expected) => {
                assert_eq!(verified.payload().as_nod_bucket(), Some(expected));
            }
        }
    }

    pub(super) fn expected_index_touches(&self) -> u32 {
        match self {
            Self::Tribute(_) | Self::NodItem(_) => 2,
            Self::NodBucket(_) => 0,
        }
    }

    pub(super) fn emitter(&self) -> Address {
        match self {
            Self::Tribute(_) => TRIBUTE_ADDRESS,
            Self::NodItem(_) | Self::NodBucket(_) => NOD_ADDRESS,
        }
    }
}
