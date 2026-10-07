/// Generates `fn $name(&mut self, entry: $entry, day: u32)`: appends `entry` to the hour's slots,
/// records the slot it took, and adds the hour to `$tree` when nothing was live in it.
#[macro_export]
macro_rules! expiry_queue_placement {
    (
        fn $name:ident(entry: $entry:ty);
        len: $len:ident,
        at: $at:ident,
        slot_key: $slot_key:path,
        slot_of: $slot_of:ident,
        packed_slot: $packed_slot:path,
        live: $live:ident,
        tree: $tree:ident $(,)?
    ) => {
        fn $name(&mut self, entry: $entry, day: u32) -> ::outbe_primitives::error::Result<()> {
            let slot = self.$len.read(&day)?;
            self.$at.write(&$slot_key(day, slot), entry)?;
            self.$len.write(&day, slot.saturating_add(1))?;
            self.$slot_of.write(&entry, $packed_slot(day, slot))?;

            let live = self.$live.read(&day)?;
            self.$live.write(&day, live.saturating_add(1))?;
            if live == 0 {
                ::outbe_primitives::math::tree_math::add(&$tree(&*self), day)?;
            }
            Ok(())
        }
    };
}
