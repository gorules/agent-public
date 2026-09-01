use crate::support::path::data_path_files;
use aws_config::BehaviorVersion;
use aws_config::meta::region::RegionProviderChain;
use aws_sdk_s3::Client;
use aws_sdk_s3::config::Credentials;
use aws_sdk_s3::primitives::ByteStream;
use std::borrow::Cow;
use std::collections::HashMap;
use testcontainers::core::WaitFor;
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, Image};

pub struct MinioContainer {
    pub container: ContainerAsync<MinioImage>,
    #[allow(dead_code)]
    pub client: Client,
}

impl MinioContainer {
    pub async fn start() -> Result<Self, Box<dyn std::error::Error + 'static>> {
        let container = MinioImage::default().start().await?;
        let host_port = container.get_host_port_ipv4(9000).await?;
        let client = container.image().s3_client(host_port).await;

        let bucket = container.image().bucket_name.as_str();

        client.create_bucket().bucket(bucket).send().await?;
        for pf in data_path_files() {
            let buf_data = pf.read();

            client
                .put_object()
                .bucket(bucket)
                .key(pf.relative_path.strip_suffix(".zip").unwrap())
                .body(ByteStream::from(buf_data))
                .send()
                .await?;
        }

        Ok(Self { container, client })
    }
}

#[derive(Debug)]
pub struct MinioImage {
    pub username: String,
    pub password: String,
    pub bucket_name: String,
}

impl Default for MinioImage {
    fn default() -> Self {
        let bucket_name = "sample-bucket".to_string();

        Self {
            username: "minio-username".to_string(),
            password: "minio-password".to_string(),
            bucket_name,
        }
    }
}

impl MinioImage {
    pub fn endpoint(&self, host_port: u16) -> String {
        format!("http://127.0.0.1:{host_port}")
    }

    async fn s3_client(&self, host_port: u16) -> Client {
        let endpoint_uri = self.endpoint(host_port);
        let region_provider = RegionProviderChain::default_provider().or_else("us-east-1");
        let creds = Credentials::new(&self.username, &self.password, None, None, "test");

        // Default MinIO credentials (Can be overridden by ENV container variables)
        let shared_config = aws_config::defaults(BehaviorVersion::latest())
            .region(region_provider)
            .endpoint_url(endpoint_uri)
            .credentials_provider(creds)
            .load()
            .await;

        Client::new(&shared_config)
    }
}

impl Image for MinioImage {
    fn name(&self) -> &str {
        "minio/minio"
    }

    fn tag(&self) -> &str {
        "RELEASE.2024-09-22T00-33-43Z"
    }

    fn ready_conditions(&self) -> Vec<WaitFor> {
        vec![WaitFor::message_on_stderr("API:")]
    }

    fn env_vars(
        &self,
    ) -> impl IntoIterator<Item = (impl Into<Cow<'_, str>>, impl Into<Cow<'_, str>>)> {
        let mut variables = HashMap::new();
        variables.insert("MINIO_ROOT_USER", &self.username);
        variables.insert("MINIO_ROOT_PASSWORD", &self.password);

        variables
    }

    fn cmd(&self) -> impl IntoIterator<Item = impl Into<Cow<'_, str>>> {
        vec!["server", "/data"]
    }
}
