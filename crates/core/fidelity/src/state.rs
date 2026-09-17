use crate::schema::FidelityContract;
use outbe_primitives::error::Result;
impl FidelityContract<'_> {
    pub fn first_qualified_start(&self) -> Result<u64> {
        self.first_qualified_start.read()
    }
}
