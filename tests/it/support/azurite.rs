use crate::support::path::data_path_files;
use agent::AzureSharedKeyPolicy;
use azure_core::http::policies::Policy;
use azure_core::http::{ClientOptions, RequestContent, Url};
use azure_storage_blob::{BlobContainerClient, BlobContainerClientOptions};
use std::borrow::Cow;
use std::sync::Arc;
use testcontainers::core::WaitFor;
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, Image};

const ACCOUNT_NAME: &str = "devstoreaccount1";
const ACCOUNT_KEY: &str =
    "Eby8vdM02xNOcqFlqUwJPLlmEtlCDXJ1OUzFT50uSRZ6IFsuFq2UVErCz4I6tq/K1SZFPTOtr/KBHBeksoGMGw==";

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

        let blob_container_client = container.image().container_client(host_port, cn.as_str())?;

        blob_container_client.create(None).await?;

        for dp in data_path_files() {
            blob_container_client
                .blob_client(dp.relative_path.strip_suffix(".zip").unwrap())
                .upload(RequestContent::from(dp.read()), None)
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
            "DefaultEndpointsProtocol=http;AccountName={ACCOUNT_NAME};AccountKey={ACCOUNT_KEY};BlobEndpoint=http://127.0.0.1:{port}/{ACCOUNT_NAME};"
        )
    }

    fn container_client(
        &self,
        port: u16,
        container_name: &str,
    ) -> Result<BlobContainerClient, Box<dyn std::error::Error + 'static>> {
        let url = Url::parse(&format!(
            "http://127.0.0.1:{port}/{ACCOUNT_NAME}/{container_name}"
        ))?;
        let options = BlobContainerClientOptions {
            client_options: ClientOptions {
                per_try_policies: vec![Arc::new(AzureSharedKeyPolicy::new(
                    ACCOUNT_NAME,
                    ACCOUNT_KEY,
                )) as Arc<dyn Policy>],
                ..Default::default()
            },
            ..Default::default()
        };

        Ok(BlobContainerClient::new(url, None, Some(options))?)
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
        vec!["azurite", "--blobHost", "0.0.0.0", "--skipApiVersionCheck"]
    }
}
