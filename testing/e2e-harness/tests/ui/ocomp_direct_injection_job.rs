use outbe_e2e_harness::world::ocomp::OcompTopology;

fn main() {
    let inject: fn(&mut OcompTopology, [u8; 32]) = OcompTopology::inject_job;
    let _ = inject;
}
