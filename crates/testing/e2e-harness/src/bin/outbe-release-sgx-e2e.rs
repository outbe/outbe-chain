#[tokio::main]
async fn main() {
    outbe_e2e_harness::release_sgx::run().await;
}
