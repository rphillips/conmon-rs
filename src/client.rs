use anyhow::{anyhow, Error};
use conmon::conmon_client::ConmonClient;
use conmon::VersionRequest;

pub mod conmon {
    tonic::include_proto!("conmon");
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    let mut client = ConmonClient::connect("http://[::1]:50051")
        .await
        .map_err(|err| anyhow!("error on connect: {}", err))?;

    let req = tonic::Request::new(VersionRequest {});

    let resp = client.version(req).await?;

    println!("Version: {:?}", resp);

    Ok(())
}
