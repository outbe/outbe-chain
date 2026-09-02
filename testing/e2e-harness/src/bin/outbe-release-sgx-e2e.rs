#[path = "../release_dcap.rs"]
#[allow(dead_code)]
mod release_dcap;
#[path = "../release_sgx.rs"]
mod release_sgx;

#[tokio::main]
async fn main() {
    release_sgx::run().await;
}
