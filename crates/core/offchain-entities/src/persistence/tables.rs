use reth_db::{
    table::{Table, TableInfo},
    TableSet,
};

#[derive(Debug)]
pub struct CeMetadata;
impl Table for CeMetadata {
    const NAME: &'static str = "OutbeCompressedEntitiesMetadataV3";
    const DUPSORT: bool = false;
    type Key = Vec<u8>;
    type Value = Vec<u8>;
}
impl TableInfo for CeMetadata {
    fn name(&self) -> &'static str {
        <Self as Table>::NAME
    }
    fn is_dupsort(&self) -> bool {
        <Self as Table>::DUPSORT
    }
}

#[derive(Debug)]
pub struct CeBranches;
impl Table for CeBranches {
    const NAME: &'static str = "OutbeCompressedEntitiesBranchesV3";
    const DUPSORT: bool = false;
    type Key = Vec<u8>;
    type Value = Vec<u8>;
}
impl TableInfo for CeBranches {
    fn name(&self) -> &'static str {
        <Self as Table>::NAME
    }
    fn is_dupsort(&self) -> bool {
        <Self as Table>::DUPSORT
    }
}

#[derive(Debug)]
pub struct CeLeaves;
impl Table for CeLeaves {
    const NAME: &'static str = "OutbeCompressedEntitiesLeavesV3";
    const DUPSORT: bool = false;
    type Key = Vec<u8>;
    type Value = Vec<u8>;
}
impl TableInfo for CeLeaves {
    fn name(&self) -> &'static str {
        <Self as Table>::NAME
    }
    fn is_dupsort(&self) -> bool {
        <Self as Table>::DUPSORT
    }
}

#[derive(Debug)]
pub struct CeTreeRoots;
impl Table for CeTreeRoots {
    const NAME: &'static str = "OutbeCompressedEntitiesTreeRootsV3";
    const DUPSORT: bool = false;
    type Key = Vec<u8>;
    type Value = Vec<u8>;
}
impl TableInfo for CeTreeRoots {
    fn name(&self) -> &'static str {
        <Self as Table>::NAME
    }
    fn is_dupsort(&self) -> bool {
        <Self as Table>::DUPSORT
    }
}

#[derive(Debug)]
pub struct CeTables;
impl TableSet for CeTables {
    fn tables() -> Box<dyn Iterator<Item = Box<dyn TableInfo>>> {
        Box::new(
            [
                Box::new(CeMetadata) as Box<dyn TableInfo>,
                Box::new(CeBranches) as Box<dyn TableInfo>,
                Box::new(CeLeaves) as Box<dyn TableInfo>,
                Box::new(CeTreeRoots) as Box<dyn TableInfo>,
            ]
            .into_iter(),
        )
    }
}
