use crate::support::path::data_path_files;
use azure_storage::{CloudLocation, ConnectionString};
use azure_storage_blobs::prelude::BlobServiceClient;
use std::borrow::Cow;
use testcontainers::core::WaitFor;
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, Image};

pub struct AzuriteContainer {
    pub container: ContainerAsync<AzuriteImage>,
    pub connection_string: String,
    pub container_name: String,
}

impl AzuriteContainer {
    pub async fn start() -> Result<Self, Box<dyn std::error::Error + 'static>> {
        let container = AzuriteImage.start().await?;

        let host_port = container.get_host_port_ipv4(10000).await?;

        let cs = container.image().connection_string(host_port);
        let cn = "sample-container".to_string();

        let client = container.image().blob_client(host_port).await;
        let blob_container_client = client.container_client(cn.as_str());

        blob_container_client.create().await?;

        for dp in data_path_files() {
            blob_container_client
                .blob_client(dp.relative_path.strip_suffix(".zip").unwrap())
                .put_block_blob(dp.read())
                .await?;
        }

        Ok(Self {
            container,
            connection_string: cs,
            container_name: cn,
        })
    }
}

#[derive(Debug)]
pub struct AzuriteImage;

impl AzuriteImage {
    pub fn connection_string(&self, port: u16) -> String {
        format!(
            "DefaultEndpointsProtocol=http;AccountName=devstoreaccount1;AccountKey=Eby8vdM02xNOcqFlqUwJPLlmEtlCDXJ1OUzFT50uSRZ6IFsuFq2UVErCz4I6tq/K1SZFPTOtr/KBHBeksoGMGw==;BlobEndpoint=http://127.0.0.1:{port}/devstoreaccount1;"
        )
    }

    async fn blob_client(&self, port: u16) -> BlobServiceClient {
        let cs = self.connection_string(port);
        let connection_string = ConnectionString::new(cs.as_str()).unwrap();

        let client_builder = BlobServiceClient::builder(
            connection_string.account_name.expect("Valid account name"),
            connection_string
                .storage_credentials()
                .expect("Valid storage credentials"),
        )
        .cloud_location(CloudLocation::Custom {
            account: connection_string
                .account_name
                .expect("Valid account name")
                .to_string(),
            uri: connection_string
                .blob_endpoint
                .expect("Valid blob endpoint")
                .to_string(),
        });

        client_builder.blob_service_client()
    }
}

impl Image for AzuriteImage {
    fn name(&self) -> &str {
        "mcr.microsoft.com/azure-storage/azurite"
    }

    fn tag(&self) -> &str {
        "latest"
    }

    fn ready_conditions(&self) -> Vec<WaitFor> {
        vec![WaitFor::message_on_stdout(
            "Azurite Blob service is successfully listening",
        )]
    }

    fn cmd(&self) -> impl IntoIterator<Item = impl Into<Cow<'_, str>>> {
        vec!["azurite", "--blobHost", "0.0.0.0"]
    }
}
